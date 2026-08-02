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
use crate::metadata;
use crate::path_store::{self, join_root, path_key, storage_path_key, to_relative};

const SCHEMA_VERSION: i64 = 2;

const TRACK_SELECT: &str = "t.id, t.root_id, t.rel_path, t.path_key, t.file_name, t.extension, t.size,
     t.title, t.artist, t.album, t.duration_secs, t.year, t.track_number, t.genre,
     t.cover_id, t.audio_rel_path, t.cue_start_secs, t.cue_end_secs, t.library_position";

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

        // Per-connection PRAGMAs (NOT stored in the .db file). External tools that
        // open library.db will still report foreign_keys=0 — only this connection matters.
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(db_error)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(db_error)?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(db_error)?;
        connection
            .pragma_update(None, "temp_store", "MEMORY")
            .map_err(db_error)?;

        let fk_on: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .map_err(db_error)?;
        if fk_on == 0 {
            return Err(
                "SQLite foreign_keys stayed OFF — CASCADE deletes would leave orphans".into(),
            );
        }

        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_info (
                     version INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS library_roots (
                     id INTEGER PRIMARY KEY,
                     path TEXT NOT NULL,
                     path_key TEXT NOT NULL UNIQUE
                 );
                 CREATE TABLE IF NOT EXISTS playlists (
                     id TEXT PRIMARY KEY,
                     name TEXT NOT NULL,
                     cover_path TEXT,
                     position INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS playlists_position ON playlists(position);
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

        let tracks_is_v1 = table_has_column(&connection, "tracks", "cover_path")?;
        let tracks_is_v2 = table_has_column(&connection, "tracks", "cover_id")?;

        match version {
            None if !tracks_is_v1 && !tracks_is_v2 => {
                create_tracks_v2(&connection)?;
                connection
                    .execute(
                        "INSERT INTO schema_info(version) VALUES (?1)",
                        [SCHEMA_VERSION],
                    )
                    .map_err(db_error)?;
            }
            None | Some(1) if tracks_is_v1 && !tracks_is_v2 => {
                migrate_v1_to_v2(&connection)?;
                if version.is_none() {
                    connection
                        .execute(
                            "INSERT INTO schema_info(version) VALUES (?1)",
                            [SCHEMA_VERSION],
                        )
                        .map_err(db_error)?;
                } else {
                    connection
                        .execute("UPDATE schema_info SET version = ?1", [SCHEMA_VERSION])
                        .map_err(db_error)?;
                }
            }
            Some(SCHEMA_VERSION) | None if tracks_is_v2 => {
                ensure_tracks_v2_indexes(&connection)?;
                if version.is_none() {
                    connection
                        .execute(
                            "INSERT INTO schema_info(version) VALUES (?1)",
                            [SCHEMA_VERSION],
                        )
                        .map_err(db_error)?;
                }
            }
            Some(SCHEMA_VERSION) => {
                ensure_tracks_v2_indexes(&connection)?;
            }
            Some(other) => {
                return Err(format!(
                    "Unsupported library database schema {other}; expected {SCHEMA_VERSION}"
                ));
            }
            None => {
                // Ambiguous empty-ish DB: create v2 tracks.
                create_tracks_v2(&connection)?;
                connection
                    .execute(
                        "INSERT INTO schema_info(version) VALUES (?1)",
                        [SCHEMA_VERSION],
                    )
                    .map_err(db_error)?;
            }
        }

        // Membership tables need tracks() to exist for FK references.
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS playlist_tracks (
                     playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                     track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                     position INTEGER NOT NULL,
                     PRIMARY KEY (playlist_id, track_id)
                 );
                 CREATE INDEX IF NOT EXISTS playlist_tracks_order
                     ON playlist_tracks(playlist_id, position);
                 CREATE INDEX IF NOT EXISTS playlist_tracks_track
                     ON playlist_tracks(track_id);
                 CREATE TABLE IF NOT EXISTS liked_tracks (
                     track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                     position INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS liked_tracks_order ON liked_tracks(position);",
            )
            .map_err(db_error)?;

        // Orphans if FK was ever off historically.
        connection
            .execute_batch(
                "DELETE FROM playlist_tracks
                   WHERE track_id NOT IN (SELECT id FROM tracks)
                      OR playlist_id NOT IN (SELECT id FROM playlists);
                 DELETE FROM liked_tracks
                   WHERE track_id NOT IN (SELECT id FROM tracks);",
            )
            .map_err(db_error)?;

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                connection: Mutex::new(connection),
                revision: AtomicU64::new(1),
            }),
        })
    }

    /// Register a library root (scan/drop folder). Tracks under it are stored relatively.
    pub fn ensure_root(&self, absolute_path: &str) -> Result<i64, String> {
        let mut connection = self.inner.connection.lock();
        let id = ensure_root_tx(&connection, absolute_path)?;
        // Best-effort: rewrite absolute tracks under this root to relative form.
        rewrite_tracks_under_root(&mut connection, id)?;
        drop(connection);
        self.changed();
        Ok(id)
    }

    /// Point an existing root at a new absolute path (drive letter / machine move).
    pub fn relocate_root(&self, root_id: i64, new_absolute: &str) -> Result<(), String> {
        let cleaned = path_store::normalize_display_path(new_absolute);
        if cleaned.trim().is_empty() {
            return Err("Root path is empty".into());
        }
        let key = path_key(&cleaned);
        let connection = self.inner.connection.lock();
        connection
            .execute(
                "UPDATE library_roots SET path = ?2, path_key = ?3 WHERE id = ?1",
                params![root_id, cleaned, key],
            )
            .map_err(db_error)?;
        drop(connection);
        self.changed();
        Ok(())
    }

    pub fn list_roots(&self) -> Result<Vec<(i64, String)>, String> {
        let connection = self.inner.connection.lock();
        load_roots(&connection)
    }

    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    fn changed(&self) {
        self.inner.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn load(&self) -> Result<PlaylistsData, String> {
        let connection = self.inner.connection.lock();
        let roots = load_roots(&connection)?;
        let library_tracks = query_tracks(
            &connection,
            &format!(
                "SELECT {TRACK_SELECT} FROM tracks t ORDER BY t.library_position, t.id"
            ),
            [],
            &roots,
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
                &format!(
                    "SELECT {TRACK_SELECT}
                       FROM playlist_tracks pt
                       JOIN tracks t ON t.id = pt.track_id
                      WHERE pt.playlist_id = ?1
                      ORDER BY pt.position, pt.track_id"
                ),
                [&id],
                &roots,
            )?;
            playlists.push(SavedPlaylist {
                id,
                name,
                tracks,
                cover_path,
            });
        }

        let liked_paths = {
            let liked = query_tracks(
                &connection,
                &format!(
                    "SELECT {TRACK_SELECT}
                       FROM liked_tracks l
                       JOIN tracks t ON t.id = l.track_id
                      ORDER BY l.position, t.id"
                ),
                [],
                &roots,
            )?;
            liked.into_iter().map(|t| t.path).collect::<Vec<_>>()
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

    /// Cover paths stored for a track path (list thumb + fullscreen full).
    pub fn get_track_covers(
        &self,
        path: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>, String> {
        let connection = self.inner.connection.lock();
        let Some(track_id) = find_track_id(&connection, path)? else {
            return Ok(None);
        };
        let cover_id: Option<String> = connection
            .query_row(
                "SELECT cover_id FROM tracks WHERE id = ?1",
                [track_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let Some(cover_id) = cover_id.filter(|id| !id.is_empty()) else {
            return Ok(Some((None, None)));
        };
        let covers = metadata::cover_paths_for_id(&cover_id);
        Ok(Some((covers.thumb, covers.full)))
    }

    /// Persist cover paths for an existing track row (no-op if the path is not in the library).
    /// Accepts full disk paths or a bare content id; stores only `cover_id`.
    pub fn set_track_covers(
        &self,
        path: &str,
        cover_path: Option<&str>,
        cover_path_full: Option<&str>,
    ) -> Result<(), String> {
        let cover_id = metadata::cover_id_from_paths(cover_path, cover_path_full)
            .or_else(|| cover_path.and_then(metadata::cover_id_from_cache_path))
            .or_else(|| cover_path_full.and_then(metadata::cover_id_from_cache_path));
        let Some(cover_id) = cover_id else {
            return Ok(());
        };
        let connection = self.inner.connection.lock();
        let Some(track_id) = find_track_id(&connection, path)? else {
            return Ok(());
        };
        connection
            .execute(
                "UPDATE tracks SET cover_id = ?2 WHERE id = ?1",
                params![track_id, cover_id],
            )
            .map_err(db_error)?;
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
            if let Some(track_id) = find_track_id(&transaction, path)? {
                transaction
                    .execute(
                        "DELETE FROM playlist_tracks
                          WHERE playlist_id = ?1 AND track_id = ?2",
                        params![playlist_id, track_id],
                    )
                    .map_err(db_error)?;
            }
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
            if let Some(track_id) = find_track_id(&transaction, path)? {
                transaction
                    .execute("DELETE FROM tracks WHERE id = ?1", [track_id])
                    .map_err(db_error)?;
            }
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
        let track_id = find_track_id(&transaction, path)?;
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

fn db_error(error: rusqlite::Error) -> String {
    format!("Library database error: {error}")
}

fn table_has_column(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, String> {
    if !table_exists(connection, table)? {
        return Ok(false);
    }
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_error)?;
    for name in rows {
        if name.map_err(db_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    Ok(count > 0)
}

fn create_tracks_v2(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS tracks (
                 id INTEGER PRIMARY KEY,
                 root_id INTEGER REFERENCES library_roots(id) ON DELETE SET NULL,
                 rel_path TEXT NOT NULL,
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
                 cover_id TEXT,
                 audio_rel_path TEXT,
                 cue_start_secs REAL,
                 cue_end_secs REAL,
                 library_position INTEGER NOT NULL
             );",
        )
        .map_err(db_error)?;
    ensure_tracks_v2_indexes(connection)
}

fn ensure_tracks_v2_indexes(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS tracks_library_position ON tracks(library_position);
             CREATE INDEX IF NOT EXISTS tracks_artist ON tracks(artist);
             CREATE INDEX IF NOT EXISTS tracks_album ON tracks(album);
             CREATE INDEX IF NOT EXISTS tracks_title ON tracks(title);
             CREATE INDEX IF NOT EXISTS tracks_root ON tracks(root_id);
             CREATE INDEX IF NOT EXISTS tracks_cover_id ON tracks(cover_id);",
        )
        .map_err(db_error)
}

fn migrate_v1_to_v2(connection: &Connection) -> Result<(), String> {
    // Rebuild tracks: absolute path → rel_path (no root yet), cover_path* → cover_id.
    connection
        .execute_batch(
            "CREATE TABLE tracks_v2 (
                 id INTEGER PRIMARY KEY,
                 root_id INTEGER REFERENCES library_roots(id) ON DELETE SET NULL,
                 rel_path TEXT NOT NULL,
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
                 cover_id TEXT,
                 audio_rel_path TEXT,
                 cue_start_secs REAL,
                 cue_end_secs REAL,
                 library_position INTEGER NOT NULL
             );",
        )
        .map_err(db_error)?;

    {
        let mut select = connection
            .prepare(
                "SELECT id, path, path_key, file_name, extension, size, title, artist, album,
                        duration_secs, year, track_number, genre, cover_path, cover_path_full,
                        audio_path, cue_start_secs, cue_end_secs, library_position
                   FROM tracks",
            )
            .map_err(db_error)?;
        let rows = select
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<f64>>(16)?,
                    row.get::<_, Option<f64>>(17)?,
                    row.get::<_, i64>(18)?,
                ))
            })
            .map_err(db_error)?;

        let mut insert = connection
            .prepare(
                "INSERT INTO tracks_v2(
                     id, root_id, rel_path, path_key, file_name, extension, size,
                     title, artist, album, duration_secs, year, track_number, genre,
                     cover_id, audio_rel_path, cue_start_secs, cue_end_secs, library_position
                 ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            )
            .map_err(db_error)?;

        for row in rows {
            let (
                id,
                path,
                old_key,
                file_name,
                extension,
                size,
                title,
                artist,
                album,
                duration_secs,
                year,
                track_number,
                genre,
                cover_path,
                cover_path_full,
                audio_path,
                cue_start,
                cue_end,
                library_position,
            ) = row.map_err(db_error)?;
            let cover_id = metadata::cover_id_from_paths(
                cover_path.as_deref(),
                cover_path_full.as_deref(),
            );
            // Keep absolute in rel_path with root_id NULL until ensure_root rewrites.
            let rel_path = path;
            let path_key = if old_key.is_empty() {
                storage_path_key(None, &rel_path)
            } else {
                old_key
            };
            insert
                .execute(params![
                    id,
                    rel_path,
                    path_key,
                    file_name,
                    extension,
                    size,
                    title,
                    artist,
                    album,
                    duration_secs,
                    year,
                    track_number,
                    genre,
                    cover_id,
                    audio_path,
                    cue_start,
                    cue_end,
                    library_position,
                ])
                .map_err(db_error)?;
        }
    }

    connection
        .execute_batch(
            "DROP TABLE tracks;
             ALTER TABLE tracks_v2 RENAME TO tracks;",
        )
        .map_err(db_error)?;
    ensure_tracks_v2_indexes(connection)
}

fn load_roots(connection: &Connection) -> Result<Vec<(i64, String)>, String> {
    let mut statement = connection
        .prepare("SELECT id, path FROM library_roots ORDER BY length(path) DESC, id")
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn ensure_root_tx(connection: &Connection, absolute_path: &str) -> Result<i64, String> {
    let cleaned = path_store::normalize_display_path(absolute_path);
    let cleaned = cleaned
        .trim_end_matches(['/', '\\'])
        .to_string();
    if cleaned.is_empty() {
        return Err("Root path is empty".into());
    }
    let key = path_key(&cleaned);
    if let Some(id) = connection
        .query_row(
            "SELECT id FROM library_roots WHERE path_key = ?1",
            [&key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(db_error)?
    {
        // Refresh display path casing if needed.
        let _ = connection.execute(
            "UPDATE library_roots SET path = ?2 WHERE id = ?1",
            params![id, cleaned],
        );
        return Ok(id);
    }
    connection
        .execute(
            "INSERT INTO library_roots(path, path_key) VALUES (?1, ?2)",
            params![cleaned, key],
        )
        .map_err(db_error)?;
    Ok(connection.last_insert_rowid())
}

fn rewrite_tracks_under_root(connection: &mut Connection, root_id: i64) -> Result<(), String> {
    let root_path: String = connection
        .query_row(
            "SELECT path FROM library_roots WHERE id = ?1",
            [root_id],
            |row| row.get(0),
        )
        .map_err(db_error)?;

    let mut select = connection
        .prepare(
            "SELECT id, rel_path, audio_rel_path FROM tracks
              WHERE root_id IS NULL OR root_id = ?1",
        )
        .map_err(db_error)?;
    let rows = select
        .query_map([root_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(db_error)?;

    let mut updates: Vec<(i64, String, Option<String>, String)> = Vec::new();
    for row in rows {
        let (id, rel_path, audio_rel) = row.map_err(db_error)?;
        // Treat current rel_path as absolute if no root or still absolute-looking.
        let looks_absolute = (rel_path.len() >= 2 && rel_path.as_bytes()[1] == b':')
            || rel_path.starts_with('\\')
            || rel_path.starts_with('/');
        if !looks_absolute {
            continue;
        }
        let abs = rel_path.clone();
        let Some(new_rel) = to_relative(&root_path, &abs) else {
            continue;
        };
        let new_audio = audio_rel.and_then(|a| {
            let abs_audio = (a.len() >= 2 && a.as_bytes().get(1) == Some(&b':'))
                || a.starts_with('\\')
                || a.starts_with('/');
            if abs_audio {
                to_relative(&root_path, &a)
            } else {
                Some(a)
            }
        });
        let new_key = storage_path_key(Some(root_id), &new_rel);
        updates.push((id, new_rel, new_audio, new_key));
    }
    drop(select);

    for (id, new_rel, new_audio, new_key) in updates {
        connection
            .execute(
                "UPDATE tracks SET root_id = ?2, rel_path = ?3, audio_rel_path = ?4, path_key = ?5
                  WHERE id = ?1",
                params![id, root_id, new_rel, new_audio, new_key],
            )
            .map_err(db_error)?;
    }
    Ok(())
}

fn resolve_stored_path(
    roots: &[(i64, String)],
    root_id: Option<i64>,
    rel_path: &str,
) -> String {
    if let Some(id) = root_id {
        if let Some((_, root)) = roots.iter().find(|(rid, _)| *rid == id) {
            return join_root(root, rel_path);
        }
    }
    rel_path.to_string()
}

fn split_for_storage(
    connection: &Connection,
    absolute: &str,
) -> Result<(Option<i64>, String, String), String> {
    let roots = load_roots(connection)?;
    let abs = path_store::normalize_display_path(absolute);
    // Longest root first (load_roots orders by length DESC).
    for (id, root) in &roots {
        if let Some(rel) = to_relative(root, &abs) {
            let key = storage_path_key(Some(*id), &rel);
            return Ok((Some(*id), rel, key));
        }
    }
    let key = storage_path_key(None, &abs);
    Ok((None, abs, key))
}

fn find_track_id(connection: &Connection, absolute: &str) -> Result<Option<i64>, String> {
    let roots = load_roots(connection)?;
    let mut candidates = vec![storage_path_key(None, absolute), path_key(absolute)];
    for (id, root) in &roots {
        if let Some(rel) = to_relative(root, absolute) {
            candidates.push(storage_path_key(Some(*id), &rel));
        }
    }
    candidates.sort();
    candidates.dedup();
    for key in candidates {
        if let Some(id) = connection
            .query_row(
                "SELECT id FROM tracks WHERE path_key = ?1",
                [&key],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(db_error)?
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn upsert_track(transaction: &Transaction<'_>, track: &MusicFile) -> Result<i64, String> {
    if track.path.trim().is_empty() {
        return Err("Cannot store a track with an empty path".to_string());
    }
    let (root_id, rel_path, key) = split_for_storage(transaction, &track.path)?;
    let audio_rel = match track.audio_path.as_deref() {
        Some(audio) if !audio.is_empty() => {
            if let Some(rid) = root_id {
                let roots = load_roots(transaction)?;
                if let Some((_, root)) = roots.iter().find(|(i, _)| *i == rid) {
                    if let Some(rel) = to_relative(root, audio) {
                        Some(rel)
                    } else {
                        let (_, rel, _) = split_for_storage(transaction, audio)?;
                        Some(rel)
                    }
                } else {
                    let (_, rel, _) = split_for_storage(transaction, audio)?;
                    Some(rel)
                }
            } else {
                let (_, rel, _) = split_for_storage(transaction, audio)?;
                Some(rel)
            }
        }
        _ => None,
    };
    let cover_id = metadata::cover_id_from_paths(
        track.cover_path.as_deref(),
        track.cover_path_full.as_deref(),
    );

    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO tracks(
                 root_id, rel_path, path_key, file_name, extension, size, title, artist, album,
                 duration_secs, year, track_number, genre, cover_id, audio_rel_path,
                 cue_start_secs, cue_end_secs, library_position
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 ?16, ?17, (SELECT COALESCE(MAX(library_position), -1) + 1 FROM tracks)
             )
             ON CONFLICT(path_key) DO UPDATE SET
                 root_id = excluded.root_id,
                 rel_path = excluded.rel_path,
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
                 cover_id = COALESCE(excluded.cover_id, tracks.cover_id),
                 audio_rel_path = COALESCE(excluded.audio_rel_path, tracks.audio_rel_path),
                 cue_start_secs = COALESCE(excluded.cue_start_secs, tracks.cue_start_secs),
                 cue_end_secs = COALESCE(excluded.cue_end_secs, tracks.cue_end_secs)
             RETURNING id",
        )
        .map_err(db_error)?;

    statement
        .query_row(
            params![
                root_id,
                rel_path,
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
                cover_id,
                audio_rel,
                track.cue_start_secs,
                track.cue_end_secs,
            ],
            |row| row.get(0),
        )
        .map_err(db_error)
}

/// Columns: see TRACK_SELECT
fn row_to_track(row: &Row<'_>, roots: &[(i64, String)]) -> rusqlite::Result<MusicFile> {
    let root_id: Option<i64> = row.get(1)?;
    let rel_path: String = row.get(2)?;
    let path = resolve_stored_path(roots, root_id, &rel_path);
    let size = row.get::<_, i64>(6)?.max(0) as u64;
    let year = row
        .get::<_, Option<i64>>(11)?
        .and_then(|value| u32::try_from(value).ok());
    let track_number = row
        .get::<_, Option<i64>>(12)?
        .and_then(|value| u32::try_from(value).ok());
    let cover_id: Option<String> = row.get(14)?;
    let covers = cover_id
        .as_deref()
        .map(metadata::cover_paths_for_id)
        .unwrap_or_default();
    let audio_rel: Option<String> = row.get(15)?;
    let audio_path = audio_rel.map(|rel| resolve_stored_path(roots, root_id, &rel));

    Ok(MusicFile {
        path,
        file_name: row.get(4)?,
        extension: row.get(5)?,
        size,
        title: row.get(7)?,
        artist: row.get(8)?,
        album: row.get(9)?,
        duration_secs: row.get(10)?,
        year,
        track_number,
        genre: row.get(13)?,
        cover_path: covers.thumb,
        cover_path_full: covers.full,
        audio_path,
        cue_start_secs: row.get(16)?,
        cue_end_secs: row.get(17)?,
    })
}

fn query_tracks<P>(
    connection: &Connection,
    sql: &str,
    params: P,
    roots: &[(i64, String)],
) -> Result<Vec<MusicFile>, String>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql).map_err(db_error)?;
    let rows = statement
        .query_map(params, |row| row_to_track(row, roots))
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
        let track_id = find_track_id(transaction, path)?
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
    fn foreign_keys_enforced_on_open() {
        let db = database();
        let connection = db.inner.connection.lock();
        let fk_on: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk_on, 1, "foreign_keys must be ON for CASCADE");
    }

    #[test]
    fn stores_relative_paths_and_cover_id() {
        let db = database();
        let root = if cfg!(windows) {
            r"Z:\library_root"
        } else {
            "/library_root"
        };
        db.ensure_root(root).unwrap();
        let track_path = if cfg!(windows) {
            r"Z:\library_root\Album\a.flac"
        } else {
            "/library_root/Album/a.flac"
        };
        let mut t = track(track_path, None);
        t.cover_path = Some(
            r"C:\Users\x\AppData\Roaming\com.nnfz.muzeeka\covers\c-0123456789abcdef-thumb.webp"
                .into(),
        );
        db.upsert_tracks(&[t]).unwrap();

        let connection = db.inner.connection.lock();
        let (rel, cover_id, root_id): (String, Option<String>, Option<i64>) = connection
            .query_row(
                "SELECT rel_path, cover_id, root_id FROM tracks LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(root_id.is_some());
        assert!(
            !rel.contains(':') || rel.starts_with("Album") || rel.starts_with("album"),
            "expected relative path, got {rel}"
        );
        assert_eq!(cover_id.as_deref(), Some("0123456789abcdef"));
        drop(connection);

        let data = db.load().unwrap();
        assert_eq!(path_key(&data.library_tracks[0].path), path_key(track_path));
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
