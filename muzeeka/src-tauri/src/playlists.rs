use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::library::MusicFile;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlaylist {
    pub id: String,
    pub name: String,
    pub tracks: Vec<MusicFile>,
    pub cover_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistMeta {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryState {
    pub active_playlist_id: Option<String>,
    pub playing_playlist_id: Option<String>,
    pub current_file: Option<String>,
    pub volume: Option<f32>,
    pub shuffle_enabled: bool,
    pub repeat_mode: Option<String>,
    /// Seekbar position in seconds (restored on cold start).
    #[serde(default)]
    pub playback_position: Option<f64>,
    /// True if audio was playing when the app last saved state.
    #[serde(default)]
    pub was_playing: bool,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            active_playlist_id: None,
            playing_playlist_id: None,
            current_file: None,
            volume: None,
            shuffle_enabled: false,
            repeat_mode: Some("off".to_string()),
            playback_position: None,
            was_playing: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaylistsData {
    pub playlists: Vec<SavedPlaylist>,
    pub library_tracks: Vec<MusicFile>,
    pub active_playlist_id: Option<String>,
    pub playing_playlist_id: Option<String>,
    pub current_file: Option<String>,
    pub volume: Option<f32>,
    pub liked_paths: Vec<String>,
    pub all_paths: Vec<String>,
    pub shuffle_enabled: bool,
    pub repeat_mode: Option<String>,
    #[serde(default)]
    pub playback_position: Option<f64>,
    #[serde(default)]
    pub was_playing: bool,
}

struct DatabaseInner {
    connection: Mutex<Connection>,
    revision: AtomicU64,
}

#[derive(Clone)]
pub struct LibraryDatabase {
    inner: Arc<DatabaseInner>,
}

impl LibraryDatabase {
    pub fn open(app: &AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("Failed to resolve app data dir: {error}"))?;
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Failed to create app data dir: {error}"))?;
        Self::open_path(dir.join("library.db"))
    }

    fn open_path(path: PathBuf) -> Result<Self, String> {
        let connection = Connection::open(&path)
            .map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> Result<Self, String> {
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(db_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA temp_store = MEMORY;
                 CREATE TABLE IF NOT EXISTS schema_info (
                     version INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS tracks (
                     id INTEGER PRIMARY KEY,
                     path TEXT NOT NULL,
                     path_key TEXT NOT NULL UNIQUE,
                     file_name TEXT NOT NULL,
                     extension TEXT NOT NULL,
                     size INTEGER NOT NULL,
                     title TEXT,
                     artist TEXT,
                     album TEXT,
                     duration_secs REAL,
                     year INTEGER,
                     track_number INTEGER,
                     genre TEXT,
                     cover_path TEXT,
                     cover_path_full TEXT,
                     audio_path TEXT,
                     cue_start_secs REAL,
                     cue_end_secs REAL,
                     library_position INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS tracks_library_position
                     ON tracks(library_position);
                 CREATE INDEX IF NOT EXISTS tracks_artist ON tracks(artist);
                 CREATE INDEX IF NOT EXISTS tracks_album ON tracks(album);
                 CREATE INDEX IF NOT EXISTS tracks_title ON tracks(title);
                 CREATE TABLE IF NOT EXISTS playlists (
                     id TEXT PRIMARY KEY,
                     name TEXT NOT NULL,
                     cover_path TEXT,
                     position INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS playlists_position ON playlists(position);
                 CREATE TABLE IF NOT EXISTS playlist_tracks (
                     playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                     track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                     position INTEGER NOT NULL,
                     PRIMARY KEY (playlist_id, track_id)
                 );
                 CREATE INDEX IF NOT EXISTS playlist_tracks_order
                     ON playlist_tracks(playlist_id, position);
                 CREATE TABLE IF NOT EXISTS liked_tracks (
                     track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                     position INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS liked_tracks_order ON liked_tracks(position);
                 CREATE TABLE IF NOT EXISTS app_state (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     active_playlist_id TEXT,
                     playing_playlist_id TEXT,
                     current_file TEXT,
                     volume REAL,
                     shuffle_enabled INTEGER NOT NULL DEFAULT 0,
                     repeat_mode TEXT NOT NULL DEFAULT 'off',
                     playback_position REAL,
                     was_playing INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT OR IGNORE INTO app_state(id) VALUES (1);",
            )
            .map_err(db_error)?;

        // Older DBs created before position/was_playing columns — add if missing.
        let _ = connection.execute(
            "ALTER TABLE app_state ADD COLUMN playback_position REAL",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE app_state ADD COLUMN was_playing INTEGER NOT NULL DEFAULT 0",
            [],
        );

        let version = connection
            .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db_error)?;
        match version {
            None => {
                connection
                    .execute(
                        "INSERT INTO schema_info(version) VALUES (?1)",
                        [SCHEMA_VERSION],
                    )
                    .map_err(db_error)?;
            }
            Some(SCHEMA_VERSION) => {}
            Some(other) => {
                return Err(format!(
                    "Unsupported library database schema {other}; expected {SCHEMA_VERSION}"
                ));
            }
        }

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                connection: Mutex::new(connection),
                revision: AtomicU64::new(1),
            }),
        })
    }

    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    fn changed(&self) {
        self.inner.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn load(&self) -> Result<PlaylistsData, String> {
        let connection = self.inner.connection.lock();
        let library_tracks = query_tracks(
            &connection,
            "SELECT t.* FROM tracks t ORDER BY t.library_position, t.id",
            [],
        )?;
        let all_paths = library_tracks
            .iter()
            .map(|track| track.path.clone())
            .collect();

        let playlist_headers = {
            let mut statement = connection
                .prepare("SELECT id, name, cover_path FROM playlists ORDER BY position, id")
                .map_err(db_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get(2)?,
                    ))
                })
                .map_err(db_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?
        };

        let mut playlists = Vec::with_capacity(playlist_headers.len());
        for (id, name, cover_path) in playlist_headers {
            let tracks = query_tracks(
                &connection,
                "SELECT t.*
                   FROM playlist_tracks pt
                   JOIN tracks t ON t.id = pt.track_id
                  WHERE pt.playlist_id = ?1
                  ORDER BY pt.position, t.id",
                [&id],
            )?;
            playlists.push(SavedPlaylist {
                id,
                name,
                tracks,
                cover_path,
            });
        }

        let liked_paths = {
            let mut statement = connection
                .prepare(
                    "SELECT t.path
                       FROM liked_tracks l
                       JOIN tracks t ON t.id = l.track_id
                      ORDER BY l.position, t.id",
                )
                .map_err(db_error)?;
            let rows = statement
                .query_map([], |row| row.get(0))
                .map_err(db_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?
        };

        let state = connection
            .query_row(
                "SELECT active_playlist_id, playing_playlist_id, current_file,
                        volume, shuffle_enabled, repeat_mode,
                        playback_position, was_playing
                   FROM app_state WHERE id = 1",
                [],
                |row| {
                    Ok(LibraryState {
                        active_playlist_id: row.get(0)?,
                        playing_playlist_id: row.get(1)?,
                        current_file: row.get(2)?,
                        volume: row.get(3)?,
                        shuffle_enabled: row.get::<_, i64>(4)? != 0,
                        repeat_mode: row.get(5)?,
                        playback_position: row.get(6)?,
                        was_playing: row.get::<_, i64>(7).unwrap_or(0) != 0,
                    })
                },
            )
            .map_err(db_error)?;

        Ok(PlaylistsData {
            playlists,
            library_tracks,
            active_playlist_id: state.active_playlist_id,
            playing_playlist_id: state.playing_playlist_id,
            current_file: state.current_file,
            volume: state.volume,
            liked_paths,
            all_paths,
            shuffle_enabled: state.shuffle_enabled,
            repeat_mode: state.repeat_mode,
            playback_position: state.playback_position,
            was_playing: state.was_playing,
        })
    }

    pub fn list_meta(&self) -> Result<Vec<PlaylistMeta>, String> {
        let connection = self.inner.connection.lock();
        let mut statement = connection
            .prepare("SELECT id, name FROM playlists ORDER BY position, id")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(PlaylistMeta {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn save_state(&self, state: &LibraryState) -> Result<(), String> {
        let connection = self.inner.connection.lock();
        connection
            .execute(
                "UPDATE app_state
                    SET active_playlist_id = ?1,
                        playing_playlist_id = ?2,
                        current_file = ?3,
                        volume = ?4,
                        shuffle_enabled = ?5,
                        repeat_mode = ?6,
                        playback_position = ?7,
                        was_playing = ?8
                  WHERE id = 1",
                params![
                    state.active_playlist_id,
                    state.playing_playlist_id,
                    state.current_file,
                    state.volume,
                    state.shuffle_enabled as i64,
                    state.repeat_mode.as_deref().unwrap_or("off"),
                    state.playback_position,
                    state.was_playing as i64,
                ],
            )
            .map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn create_playlist(&self, id: &str, name: &str) -> Result<(), String> {
        let connection = self.inner.connection.lock();
        connection
            .execute(
                "INSERT INTO playlists(id, name, position)
                 VALUES (?1, ?2, (SELECT COALESCE(MAX(position), -1) + 1 FROM playlists))",
                params![id, name],
            )
            .map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn clear_all(&self) -> Result<(), String> {
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        transaction.execute_batch(
            "DELETE FROM playlist_tracks;
             DELETE FROM playlists;
             DELETE FROM liked_tracks;
             DELETE FROM tracks;
             UPDATE app_state SET
                 active_playlist_id = NULL,
                 playing_playlist_id = NULL,
                 current_file = NULL
             WHERE id = 1;",
        ).map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn delete_playlist(&self, id: &str) -> Result<(), String> {
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        transaction
            .execute("DELETE FROM playlists WHERE id = ?1", [id])
            .map_err(db_error)?;
        let playlist_ids = ordered_text_ids(
            &transaction,
            "SELECT id FROM playlists ORDER BY position, id",
        )?;
        for (position, playlist_id) in playlist_ids.iter().enumerate() {
            transaction
                .execute(
                    "UPDATE playlists SET position = ?2 WHERE id = ?1",
                    params![playlist_id, position as i64],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn rename_playlist(&self, id: &str, name: &str) -> Result<(), String> {
        let connection = self.inner.connection.lock();
        connection
            .execute(
                "UPDATE playlists SET name = ?2 WHERE id = ?1",
                params![id, name],
            )
            .map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn set_playlist_cover(&self, id: &str, cover_path: Option<&str>) -> Result<(), String> {
        let connection = self.inner.connection.lock();
        connection
            .execute(
                "UPDATE playlists SET cover_path = ?2 WHERE id = ?1",
                params![id, cover_path],
            )
            .map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn upsert_tracks(&self, tracks: &[MusicFile]) -> Result<(), String> {
        if tracks.is_empty() {
            return Ok(());
        }
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        for track in tracks {
            upsert_track(&transaction, track)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn add_tracks_to_playlist(
        &self,
        playlist_id: &str,
        tracks: &[MusicFile],
    ) -> Result<(), String> {
        if tracks.is_empty() {
            return Ok(());
        }
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        let mut next_position: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1
                   FROM playlist_tracks WHERE playlist_id = ?1",
                [playlist_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        for track in tracks {
            let track_id = upsert_track(&transaction, track)?;
            let inserted = transaction
                .execute(
                    "INSERT OR IGNORE INTO playlist_tracks(playlist_id, track_id, position)
                     VALUES (?1, ?2, ?3)",
                    params![playlist_id, track_id, next_position],
                )
                .map_err(db_error)?;
            if inserted > 0 {
                next_position += 1;
            }
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn remove_tracks_from_playlist(
        &self,
        playlist_id: &str,
        paths: &[String],
    ) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        for path in paths {
            transaction
                .execute(
                    "DELETE FROM playlist_tracks
                      WHERE playlist_id = ?1
                        AND track_id = (SELECT id FROM tracks WHERE path_key = ?2)",
                    params![playlist_id, path_key(path)],
                )
                .map_err(db_error)?;
        }
        compact_positions(
            &transaction,
            "playlist_tracks",
            "track_id",
            Some(("playlist_id", playlist_id)),
        )?;
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn reorder_playlist(&self, playlist_id: &str, paths: &[String]) -> Result<(), String> {
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        let ids = track_ids_for_paths(&transaction, paths)?;
        let current = ordered_ids(
            &transaction,
            "SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position, track_id",
            [playlist_id],
        )?;
        validate_reorder(&current, &ids, "playlist")?;
        for (position, track_id) in ids.iter().enumerate() {
            transaction
                .execute(
                    "UPDATE playlist_tracks SET position = ?3
                      WHERE playlist_id = ?1 AND track_id = ?2",
                    params![playlist_id, track_id, position as i64],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn reorder_library(&self, paths: &[String]) -> Result<(), String> {
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        let ids = track_ids_for_paths(&transaction, paths)?;
        let current = ordered_ids(
            &transaction,
            "SELECT id FROM tracks ORDER BY library_position, id",
            [],
        )?;
        validate_reorder(&current, &ids, "library")?;
        for (position, track_id) in ids.iter().enumerate() {
            transaction
                .execute(
                    "UPDATE tracks SET library_position = ?2 WHERE id = ?1",
                    params![track_id, position as i64],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn remove_library_tracks(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        for path in paths {
            transaction
                .execute("DELETE FROM tracks WHERE path_key = ?1", [path_key(path)])
                .map_err(db_error)?;
        }
        compact_positions(&transaction, "tracks", "id", None)?;
        compact_positions(&transaction, "liked_tracks", "track_id", None)?;
        let playlist_ids = ordered_text_ids(
            &transaction,
            "SELECT id FROM playlists ORDER BY position, id",
        )?;
        for playlist_id in playlist_ids {
            compact_positions(
                &transaction,
                "playlist_tracks",
                "track_id",
                Some(("playlist_id", &playlist_id)),
            )?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn set_liked(&self, path: &str, liked: bool) -> Result<(), String> {
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        let track_id = transaction
            .query_row(
                "SELECT id FROM tracks WHERE path_key = ?1",
                [path_key(path)],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(track_id) = track_id {
            if liked {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO liked_tracks(track_id, position)
                         VALUES (?1, (SELECT COALESCE(MAX(position), -1) + 1 FROM liked_tracks))",
                        [track_id],
                    )
                    .map_err(db_error)?;
            } else {
                transaction
                    .execute("DELETE FROM liked_tracks WHERE track_id = ?1", [track_id])
                    .map_err(db_error)?;
                compact_positions(&transaction, "liked_tracks", "track_id", None)?;
            }
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn reorder_liked(&self, paths: &[String]) -> Result<(), String> {
        let mut connection = self.inner.connection.lock();
        let transaction = connection.transaction().map_err(db_error)?;
        let ids = track_ids_for_paths(&transaction, paths)?;
        let current = ordered_ids(
            &transaction,
            "SELECT track_id FROM liked_tracks ORDER BY position, track_id",
            [],
        )?;
        validate_reorder(&current, &ids, "liked tracks")?;
        for (position, track_id) in ids.iter().enumerate() {
            transaction
                .execute(
                    "UPDATE liked_tracks SET position = ?2 WHERE track_id = ?1",
                    params![track_id, position as i64],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }
}

fn path_key(path: &str) -> String {
    let trimmed = path.trim();
    let without_prefix = trimmed
        .strip_prefix(r"\\?\")
        .or_else(|| trimmed.strip_prefix("//?/"))
        .unwrap_or(trimmed);
    if cfg!(windows) {
        without_prefix.replace('/', "\\").to_lowercase()
    } else {
        without_prefix.to_string()
    }
}

fn db_error(error: rusqlite::Error) -> String {
    format!("Library database error: {error}")
}

fn upsert_track(transaction: &Transaction<'_>, track: &MusicFile) -> Result<i64, String> {
    let key = path_key(&track.path);
    if key.is_empty() {
        return Err("Cannot store a track with an empty path".to_string());
    }
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO tracks(
                 path, path_key, file_name, extension, size, title, artist, album,
                 duration_secs, year, track_number, genre, cover_path, cover_path_full,
                 audio_path, cue_start_secs, cue_end_secs, library_position
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, (SELECT COALESCE(MAX(library_position), -1) + 1 FROM tracks)
             )
             ON CONFLICT(path_key) DO UPDATE SET
                 path = excluded.path,
                 file_name = excluded.file_name,
                 extension = excluded.extension,
                 size = excluded.size,
                 title = COALESCE(excluded.title, tracks.title),
                 artist = COALESCE(excluded.artist, tracks.artist),
                 album = COALESCE(excluded.album, tracks.album),
                 duration_secs = COALESCE(excluded.duration_secs, tracks.duration_secs),
                 year = COALESCE(excluded.year, tracks.year),
                 track_number = COALESCE(excluded.track_number, tracks.track_number),
                 genre = COALESCE(excluded.genre, tracks.genre),
                 cover_path = COALESCE(excluded.cover_path, tracks.cover_path),
                 cover_path_full = COALESCE(excluded.cover_path_full, tracks.cover_path_full),
                 audio_path = COALESCE(excluded.audio_path, tracks.audio_path),
                 cue_start_secs = COALESCE(excluded.cue_start_secs, tracks.cue_start_secs),
                 cue_end_secs = COALESCE(excluded.cue_end_secs, tracks.cue_end_secs)
             RETURNING id",
        )
        .map_err(db_error)?;

    statement
        .query_row(
            params![
                track.path,
                key,
                track.file_name,
                track.extension,
                i64::try_from(track.size).unwrap_or(i64::MAX),
                track.title,
                track.artist,
                track.album,
                track.duration_secs,
                track.year.map(i64::from),
                track.track_number.map(i64::from),
                track.genre,
                track.cover_path,
                track.cover_path_full,
                track.audio_path,
                track.cue_start_secs,
                track.cue_end_secs,
            ],
            |row| row.get(0),
        )
        .map_err(db_error)
}

fn row_to_track(row: &Row<'_>) -> rusqlite::Result<MusicFile> {
    let size = row.get::<_, i64>(5)?.max(0) as u64;
    let year = row
        .get::<_, Option<i64>>(10)?
        .and_then(|value| u32::try_from(value).ok());
    let track_number = row
        .get::<_, Option<i64>>(11)?
        .and_then(|value| u32::try_from(value).ok());
    Ok(MusicFile {
        path: row.get(1)?,
        file_name: row.get(3)?,
        extension: row.get(4)?,
        size,
        title: row.get(6)?,
        artist: row.get(7)?,
        album: row.get(8)?,
        duration_secs: row.get(9)?,
        year,
        track_number,
        genre: row.get(12)?,
        cover_path: row.get(13)?,
        cover_path_full: row.get(14)?,
        audio_path: row.get(15)?,
        cue_start_secs: row.get(16)?,
        cue_end_secs: row.get(17)?,
    })
}

fn query_tracks<P>(connection: &Connection, sql: &str, params: P) -> Result<Vec<MusicFile>, String>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql).map_err(db_error)?;
    let rows = statement
        .query_map(params, row_to_track)
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn track_ids_for_paths(
    transaction: &Transaction<'_>,
    paths: &[String],
) -> Result<Vec<i64>, String> {
    let mut ids = Vec::with_capacity(paths.len());
    let mut seen = HashSet::with_capacity(paths.len());
    for path in paths {
        let track_id = transaction
            .query_row(
                "SELECT id FROM tracks WHERE path_key = ?1",
                [path_key(path)],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("Track is not in the library: {path}"))?;
        if !seen.insert(track_id) {
            return Err(format!("Duplicate track in order: {path}"));
        }
        ids.push(track_id);
    }
    Ok(ids)
}

fn ordered_ids<P>(transaction: &Transaction<'_>, sql: &str, params: P) -> Result<Vec<i64>, String>
where
    P: rusqlite::Params,
{
    let mut statement = transaction.prepare(sql).map_err(db_error)?;
    let rows = statement
        .query_map(params, |row| row.get(0))
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn ordered_text_ids(transaction: &Transaction<'_>, sql: &str) -> Result<Vec<String>, String> {
    let mut statement = transaction.prepare(sql).map_err(db_error)?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn validate_reorder(current: &[i64], requested: &[i64], label: &str) -> Result<(), String> {
    if current.len() != requested.len() {
        return Err(format!(
            "Cannot reorder {label}: expected {} tracks, received {}",
            current.len(),
            requested.len()
        ));
    }
    let current: HashSet<_> = current.iter().copied().collect();
    let requested: HashSet<_> = requested.iter().copied().collect();
    if current != requested {
        return Err(format!("Cannot reorder {label}: track set changed"));
    }
    Ok(())
}

fn compact_positions(
    transaction: &Transaction<'_>,
    table: &str,
    id_column: &str,
    filter: Option<(&str, &str)>,
) -> Result<(), String> {
    let (select, values): (String, Vec<String>) = match filter {
        Some((filter_column, filter_value)) => (
            format!(
                "SELECT {id_column} FROM {table} WHERE {filter_column} = ?1 ORDER BY position, {id_column}"
            ),
            vec![filter_value.to_string()],
        ),
        None => {
            let order_column = if table == "tracks" {
                "library_position"
            } else {
                "position"
            };
            (
                format!(
                    "SELECT {id_column} FROM {table} ORDER BY {order_column}, {id_column}"
                ),
                Vec::new(),
            )
        }
    };
    let ids = if values.is_empty() {
        ordered_ids(transaction, &select, [])?
    } else {
        ordered_ids(transaction, &select, [&values[0]])?
    };
    for (position, id) in ids.iter().enumerate() {
        let sql = match filter {
            Some((filter_column, _)) => format!(
                "UPDATE {table} SET position = ?1 WHERE {filter_column} = ?2 AND {id_column} = ?3"
            ),
            None if table == "tracks" => {
                format!("UPDATE {table} SET library_position = ?1 WHERE {id_column} = ?2")
            }
            None => format!("UPDATE {table} SET position = ?1 WHERE {id_column} = ?2"),
        };
        if let Some((_, filter_value)) = filter {
            transaction
                .execute(&sql, params![position as i64, filter_value, id])
                .map_err(db_error)?;
        } else {
            transaction
                .execute(&sql, params![position as i64, id])
                .map_err(db_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str, cue_start: Option<f64>) -> MusicFile {
        MusicFile {
            path: path.to_string(),
            file_name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_string(),
            extension: "flac".to_string(),
            size: 42,
            title: Some(path.to_string()),
            artist: None,
            album: None,
            duration_secs: Some(60.0),
            year: None,
            track_number: None,
            genre: None,
            cover_path: None,
            cover_path_full: None,
            audio_path: cue_start.map(|_| "album.flac".to_string()),
            cue_start_secs: cue_start,
            cue_end_secs: cue_start.map(|start| start + 60.0),
        }
    }

    fn database() -> LibraryDatabase {
        LibraryDatabase::from_connection(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn stores_tracks_once_and_preserves_playlist_order() {
        let db = database();
        db.create_playlist("one", "One").unwrap();
        let tracks = vec![track("a.flac", None), track("b.flac", None)];
        db.add_tracks_to_playlist("one", &tracks).unwrap();
        db.reorder_playlist("one", &["b.flac".into(), "a.flac".into()])
            .unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.library_tracks.len(), 2);
        assert_eq!(data.playlists[0].tracks[0].path, "b.flac");
        assert_eq!(data.playlists[0].tracks[1].path, "a.flac");
    }

    #[test]
    fn cue_segments_are_independent_library_rows() {
        let db = database();
        db.create_playlist("cue", "Cue").unwrap();
        let tracks = vec![
            track("album.flac#cue:1", Some(0.0)),
            track("album.flac#cue:2", Some(60.0)),
        ];
        db.add_tracks_to_playlist("cue", &tracks).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.library_tracks.len(), 2);
        assert_eq!(data.playlists[0].tracks[1].cue_start_secs, Some(60.0));
    }

    #[test]
    fn deleting_library_track_cascades_membership_and_like() {
        let db = database();
        db.create_playlist("one", "One").unwrap();
        db.add_tracks_to_playlist("one", &[track("a.flac", None)])
            .unwrap();
        db.set_liked("a.flac", true).unwrap();
        db.remove_library_tracks(&["a.flac".into()]).unwrap();

        let data = db.load().unwrap();
        assert!(data.library_tracks.is_empty());
        assert!(data.playlists[0].tracks.is_empty());
        assert!(data.liked_paths.is_empty());
    }

    #[test]
    fn library_and_liked_orders_are_persisted_independently() {
        let db = database();
        let tracks = vec![track("a.flac", None), track("b.flac", None)];
        db.upsert_tracks(&tracks).unwrap();
        db.set_liked("a.flac", true).unwrap();
        db.set_liked("b.flac", true).unwrap();
        db.reorder_library(&["b.flac".into(), "a.flac".into()])
            .unwrap();
        db.reorder_liked(&["a.flac".into(), "b.flac".into()])
            .unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.all_paths, ["b.flac", "a.flac"]);
        assert_eq!(data.liked_paths, ["a.flac", "b.flac"]);
    }

    #[test]
    fn stale_reorder_cannot_drop_tracks() {
        let db = database();
        db.create_playlist("one", "One").unwrap();
        db.add_tracks_to_playlist("one", &[track("a.flac", None), track("b.flac", None)])
            .unwrap();

        let error = db.reorder_playlist("one", &["a.flac".into()]).unwrap_err();
        assert!(error.contains("expected 2 tracks"));
        assert_eq!(db.load().unwrap().playlists[0].tracks.len(), 2);
    }

    #[test]
    fn deleting_playlist_keeps_library_tracks() {
        let db = database();
        db.create_playlist("one", "One").unwrap();
        db.create_playlist("two", "Two").unwrap();
        db.add_tracks_to_playlist("one", &[track("a.flac", None)])
            .unwrap();
        db.delete_playlist("one").unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.library_tracks.len(), 1);
        assert_eq!(data.playlists.len(), 1);
        assert_eq!(data.playlists[0].id, "two");
    }

    #[test]
    fn player_state_is_a_small_independent_row() {
        let db = database();
        db.save_state(&LibraryState {
            active_playlist_id: Some("__all__".into()),
            playing_playlist_id: Some("__liked__".into()),
            current_file: Some("a.flac".into()),
            volume: Some(0.42),
            shuffle_enabled: true,
            repeat_mode: Some("all".into()),
            playback_position: Some(123.5),
            was_playing: true,
        })
        .unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.active_playlist_id.as_deref(), Some("__all__"));
        assert_eq!(data.current_file.as_deref(), Some("a.flac"));
        assert_eq!(data.volume, Some(0.42));
        assert!(data.shuffle_enabled);
        assert_eq!(data.repeat_mode.as_deref(), Some("all"));
        assert_eq!(data.playback_position, Some(123.5));
        assert!(data.was_playing);
    }
}
