//! The on-disk Muzeeka library.
//!
//! The installable app keeps using Tauri's data directory,
//! `%APPDATA%\com.nnfz.muzeeka`: `library.db`, `covers/`, and `lyrics/`.
//! Nothing is copied out of that folder.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::ui::session::{LibraryView, Playlist, RepeatMode, Track, TrackId};

const LYRICS_HIT_SUFFIX: &str = ".match-v1.ttml";

pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("com.nnfz.muzeeka")
}

pub struct LoadedLibrary {
    pub tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub view: LibraryView,
    pub current: Option<TrackId>,
    pub position: f64,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Playlist that owned the queue, when the last track was started from one.
    pub playing_playlist_id: Option<String>,
    /// True when the process last exited with this track still playing.
    pub was_playing: bool,
    pub db: Connection,
}

/// Last track, playhead, and the playback switches stored in `app_state`.
pub(crate) struct PlaybackMemory {
    pub active_playlist_id: Option<String>,
    pub playing_playlist_id: Option<String>,
    pub current_file: Option<String>,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub position: f64,
    pub was_playing: bool,
}

/// Tracks and playlists currently stored in `library.db`.
pub(crate) fn read_lists(db: &Connection) -> Result<(Vec<Track>, Vec<Playlist>), String> {
    let root = app_data_dir();
    let roots = load_roots(db)?;
    let covers = index_covers(&root.join("covers"));
    let lyrics = index_lyrics(&root.join("lyrics"));
    let mut tracks = load_tracks(db, &roots, &covers, &lyrics)?;
    backfill_cue_covers(db, &mut tracks);
    let by_id: HashMap<u32, usize> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.id.0, index))
        .collect();
    let playlists = load_playlists(db, &root.join("playlist_covers"), &tracks, &by_id)?;
    Ok((tracks, playlists))
}

pub fn load() -> Result<LoadedLibrary, String> {
    let root = app_data_dir();
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
    load_db(&root.join("library.db"))
}

/// Opens `library.db`, creating an empty library when the file is missing.
fn load_db(db_path: &Path) -> Result<LoadedLibrary, String> {
    let db = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|error| format!("Failed to open {}: {error}", db_path.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    db.pragma_update(None, "foreign_keys", true)
        .map_err(|error| error.to_string())?;
    ensure_schema(&db)?;
    ensure_playback_columns(&db)?;
    ensure_app_state_row(&db)?;

    let (tracks, playlists) = read_lists(&db)?;
    let stored = load_state(&db)?;
    let view = match stored.active_playlist_id.clone() {
        Some(id) if playlists.iter().any(|playlist| playlist.id == id) => LibraryView::Playlist(id),
        _ => LibraryView::All,
    };
    let current = stored
        .current_file
        .as_deref()
        .and_then(|path| find_track(&tracks, path));
    let position = if current.is_some() {
        stored.position
    } else {
        0.0
    };

    Ok(LoadedLibrary {
        tracks,
        playlists,
        view,
        current,
        position,
        volume: stored.volume,
        shuffle: stored.shuffle,
        repeat: stored.repeat,
        playing_playlist_id: stored.playing_playlist_id,
        was_playing: stored.was_playing && current.is_some(),
        db,
    })
}

pub(crate) fn save_playback(db: &Connection, memory: &PlaybackMemory) -> Result<(), String> {
    ensure_playback_columns(db)?;
    let repeat = match memory.repeat {
        RepeatMode::All => "all",
        RepeatMode::One => "one",
        RepeatMode::Off => "off",
    };
    db.execute(
        "INSERT INTO app_state (
            id, active_playlist_id, playing_playlist_id, current_file, volume,
            shuffle_enabled, repeat_mode, playback_position, was_playing
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            active_playlist_id = excluded.active_playlist_id,
            playing_playlist_id = excluded.playing_playlist_id,
            current_file = excluded.current_file,
            volume = excluded.volume,
            shuffle_enabled = excluded.shuffle_enabled,
            repeat_mode = excluded.repeat_mode,
            playback_position = excluded.playback_position,
            was_playing = excluded.was_playing",
        rusqlite::params![
            memory.active_playlist_id,
            memory.playing_playlist_id,
            memory.current_file,
            memory.volume as f64,
            memory.shuffle as i64,
            repeat,
            memory.position,
            memory.was_playing as i64,
        ],
    )
    .map_err(|error| format!("Could not save playback: {error}"))?;
    Ok(())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS library_roots (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    path_key TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY,
    root_id INTEGER,
    rel_path TEXT NOT NULL,
    path_key TEXT NOT NULL UNIQUE,
    file_name TEXT,
    extension TEXT,
    size INTEGER,
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
    library_position INTEGER
);
CREATE TABLE IF NOT EXISTS liked_tracks (
    track_id INTEGER PRIMARY KEY,
    position INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlists (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    cover_path TEXT,
    mix_mode INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS playlist_tracks (
    playlist_id TEXT NOT NULL,
    track_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, track_id)
);
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
";

fn ensure_schema(db: &Connection) -> Result<(), String> {
    db.execute_batch(SCHEMA)
        .map_err(|error| format!("Could not create the library database: {error}"))
}

fn ensure_app_state_row(db: &Connection) -> Result<(), String> {
    db.execute(
        "INSERT OR IGNORE INTO app_state (id, volume, shuffle_enabled, repeat_mode, was_playing)
         VALUES (1, 0.7, 0, 'off', 0)",
        [],
    )
    .map_err(|error| format!("Could not create the library database: {error}"))?;
    Ok(())
}

fn ensure_playback_columns(db: &Connection) -> Result<(), String> {
    let mut statement = db
        .prepare("PRAGMA table_info(app_state)")
        .map_err(|error| error.to_string())?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    if names.is_empty() {
        return Err("app_state table is missing".into());
    }
    if !names.contains("playback_position") {
        db.execute(
            "ALTER TABLE app_state ADD COLUMN playback_position REAL",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    if !names.contains("was_playing") {
        db.execute(
            "ALTER TABLE app_state ADD COLUMN was_playing INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    if !names.contains("playing_playlist_id") {
        db.execute(
            "ALTER TABLE app_state ADD COLUMN playing_playlist_id TEXT",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn load_roots(db: &Connection) -> Result<HashMap<i64, String>, String> {
    let mut statement = db
        .prepare("SELECT id, path FROM library_roots")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<_, _>>()
        .map_err(|error| error.to_string())
}

fn load_tracks(
    db: &Connection,
    roots: &HashMap<i64, String>,
    covers: &HashMap<String, PathBuf>,
    lyrics: &HashSet<String>,
) -> Result<Vec<Track>, String> {
    let mut statement = db
        .prepare(
            "SELECT id, root_id, rel_path, title, artist, album, duration_secs, cover_id
               FROM tracks
              ORDER BY library_position, id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|error| error.to_string())?;

    let mut liked = HashSet::new();
    {
        let mut liked_rows = db
            .prepare("SELECT track_id FROM liked_tracks ORDER BY position, track_id")
            .map_err(|error| error.to_string())?;
        let ids = liked_rows
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string())?;
        for id in ids {
            liked.insert(id.map_err(|error| error.to_string())?);
        }
    }

    let mut tracks = Vec::new();
    for row in rows {
        let (id, root_id, rel_path, title, artist, album, duration, cover_id) =
            row.map_err(|error| error.to_string())?;
        let Ok(id) = u32::try_from(id) else {
            continue;
        };
        let artist = artist.unwrap_or_default();
        let album = album.unwrap_or_default();
        let duration = duration.unwrap_or(0.0).max(0.0);
        let title = display_title(title, &rel_path);
        let cover = cover_id
            .as_deref()
            .map(|id| id.trim().to_ascii_lowercase())
            .and_then(|id| covers.get(&id).cloned());
        let lyrics_path = lyrics_file(&lyrics, &title, &artist, &album, duration);
        tracks.push(Track {
            id: TrackId(id),
            title,
            artist,
            album,
            duration,
            liked: liked.contains(&(id as i64)),
            path: resolve_path(roots, root_id, &rel_path),
            cover,
            lyrics: lyrics_path,
        });
    }
    Ok(tracks)
}

/// Liked rows in the order the user arranged them.
pub(crate) fn load_liked_order(db: &Connection) -> Result<Vec<TrackId>, String> {
    let mut statement = db
        .prepare("SELECT track_id FROM liked_tracks ORDER BY position, track_id")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(|error| error.to_string())?;
    let mut ids = Vec::new();
    for id in rows {
        let id = id.map_err(|error| error.to_string())?;
        if let Ok(id) = u32::try_from(id) {
            ids.push(TrackId(id));
        }
    }
    Ok(ids)
}

fn load_playlists(
    db: &Connection,
    playlist_covers: &Path,
    tracks: &[Track],
    by_id: &HashMap<u32, usize>,
) -> Result<Vec<Playlist>, String> {
    let mix_column = playlist_has_mix_mode(db);
    let sql = if mix_column {
        "SELECT id, name, cover_path, COALESCE(mix_mode, 0) FROM playlists ORDER BY position, id"
    } else {
        "SELECT id, name, cover_path, 0 FROM playlists ORDER BY position, id"
    };
    let mut statement = db.prepare(sql).map_err(|error| error.to_string())?;
    let headers = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let mut membership = db
        .prepare(
            "SELECT track_id FROM playlist_tracks
              WHERE playlist_id = ?1
              ORDER BY position, track_id",
        )
        .map_err(|error| error.to_string())?;

    let mut playlists = Vec::with_capacity(headers.len());
    for (id, name, cover_path, mix_mode) in headers {
        let ids = membership
            .query_map([&id], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string())?
            .filter_map(|id| id.ok().and_then(|id| u32::try_from(id).ok()))
            .map(TrackId)
            .collect::<Vec<_>>();
        let stored = cover_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        let cover = playlist_cover(playlist_covers, &id, stored.as_deref(), &ids, tracks, by_id);
        playlists.push(Playlist {
            id,
            name,
            track_ids: ids,
            cover,
            cover_path: stored,
            mix_mode,
        });
    }
    Ok(playlists)
}

fn playlist_has_mix_mode(db: &Connection) -> bool {
    let Ok(mut statement) = db.prepare("SELECT name FROM pragma_table_info('playlists')") else {
        return false;
    };
    let names = match statement.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows.flatten().collect::<Vec<_>>(),
        Err(_) => return false,
    };
    names.iter().any(|name| name == "mix_mode")
}

/// Resize a picked image into `playlist_covers/{id}.webp` and return that path.
pub(crate) fn store_playlist_cover(playlist_id: &str, source: &Path) -> Result<PathBuf, String> {
    if !source.is_file() {
        return Err("Cover image file not found".into());
    }
    let safe = sanitize_playlist_id(playlist_id)?;
    let dir = app_data_dir().join("playlist_covers");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Could not create the cover folder: {error}"))?;
    remove_cover_files(&dir, &safe);
    let dest = dir.join(format!("{safe}.webp"));
    let image = image::open(source).map_err(|error| format!("Failed to open image: {error}"))?;
    let (width, height) = image::GenericImageView::dimensions(&image);
    let thumb = if width <= 256 && height <= 256 {
        image
    } else {
        image.resize(256, 256, image::imageops::FilterType::Triangle)
    };
    thumb
        .save_with_format(&dest, image::ImageFormat::WebP)
        .map_err(|error| format!("Failed to write playlist cover: {error}"))?;
    Ok(dest)
}

pub(crate) fn remove_playlist_cover_files(playlist_id: &str) {
    let Ok(safe) = sanitize_playlist_id(playlist_id) else {
        return;
    };
    remove_cover_files(&app_data_dir().join("playlist_covers"), &safe);
}

fn sanitize_playlist_id(playlist_id: &str) -> Result<String, String> {
    let safe: String = playlist_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    if safe.is_empty() {
        Err("Invalid playlist id".into())
    } else {
        Ok(safe)
    }
}

fn remove_cover_files(dir: &Path, safe_id: &str) {
    for ext in ["jpg", "jpeg", "gif", "png", "webp", "bmp", "tif", "tiff"] {
        let path = dir.join(format!("{safe_id}.{ext}"));
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}

fn playlist_cover(
    dir: &Path,
    id: &str,
    stored: Option<&str>,
    track_ids: &[TrackId],
    tracks: &[Track],
    by_id: &HashMap<u32, usize>,
) -> Option<PathBuf> {
    if let Some(stored) = stored.map(str::trim).filter(|path| !path.is_empty()) {
        let path = PathBuf::from(stored);
        if path.is_file() {
            return Some(path);
        }
    }
    let custom = dir.join(format!("{id}.webp"));
    if custom.is_file() {
        return Some(custom);
    }
    track_ids.iter().find_map(|id| {
        by_id
            .get(&id.0)
            .and_then(|index| tracks.get(*index))
            .and_then(|track| track.cover.clone())
    })
}

pub(crate) fn load_state(db: &Connection) -> Result<PlaybackMemory, String> {
    db.query_row(
        "SELECT active_playlist_id, playing_playlist_id, current_file, volume,
                shuffle_enabled, repeat_mode, playback_position, was_playing
           FROM app_state WHERE id = 1",
        [],
        |row| {
            let volume: Option<f64> = row.get(3)?;
            let repeat: Option<String> = row.get(5)?;
            let position: Option<f64> = row.get(6)?;
            let was_playing: Option<i64> = row.get(7)?;
            Ok(PlaybackMemory {
                active_playlist_id: row.get(0)?,
                playing_playlist_id: row.get(1)?,
                current_file: row.get(2)?,
                volume: volume
                    .map(|value| value as f32)
                    .unwrap_or(0.7)
                    .clamp(0.0, 1.0),
                shuffle: row.get::<_, i64>(4)? != 0,
                repeat: match repeat.as_deref() {
                    Some("all") => RepeatMode::All,
                    Some("one") => RepeatMode::One,
                    _ => RepeatMode::Off,
                },
                position: position.unwrap_or(0.0).max(0.0),
                was_playing: was_playing.unwrap_or(0) != 0,
            })
        },
    )
    .map_err(|error| error.to_string())
}

fn index_covers(dir: &Path) -> HashMap<String, PathBuf> {
    let mut covers = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return covers;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("c-") else {
            continue;
        };
        let Some((id, kind)) = rest.split_once('-') else {
            continue;
        };
        if id.len() != 16 {
            continue;
        }
        let prefer = kind.starts_with("thumb.");
        let replace = !matches!(covers.get(id), Some(_) if !prefer);
        if replace {
            covers.insert(id.to_string(), path);
        }
    }
    covers
}

fn index_lyrics(dir: &Path) -> HashSet<String> {
    let mut hits = HashSet::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return hits;
    };
    let mut cleared = HashSet::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(|name| name.to_string()) else {
            continue;
        };
        if let Some(key) = name.strip_suffix(LYRICS_HIT_SUFFIX) {
            hits.insert(key.to_string());
        } else if let Some(key) = name.strip_suffix(".cleared") {
            cleared.insert(key.to_string());
        }
    }
    hits.retain(|key| !cleared.contains(key));
    hits
}

fn lyrics_file(
    hits: &HashSet<String>,
    title: &str,
    artist: &str,
    album: &str,
    duration: f64,
) -> Option<PathBuf> {
    if title.trim().is_empty() && artist.trim().is_empty() {
        return None;
    }
    let key = lyrics_cache_key(title, artist, album, duration);
    if !hits.contains(&key) {
        return None;
    }
    Some(
        app_data_dir()
            .join("lyrics")
            .join(format!("{key}{LYRICS_HIT_SUFFIX}")),
    )
}

/// Same key as the Tauri lyrics cache: lowercase title, artist, album, rounded seconds.
pub fn lyrics_cache_key(title: &str, artist: &str, album: &str, duration_secs: f64) -> String {
    let duration = if duration_secs > 0.0 {
        format!("{}", duration_secs.round() as u32)
    } else {
        String::new()
    };
    let normalized = format!(
        "{}\0{}\0{}\0{}",
        title.trim().to_lowercase(),
        artist.trim().to_lowercase(),
        album.trim().to_lowercase(),
        duration
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn display_title(title: Option<String>, rel_path: &str) -> String {
    if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
        return title;
    }
    let base = rel_path.rsplit(['\\', '/']).next().unwrap_or(rel_path);
    let base = base.split("#cue:").next().unwrap_or(base);
    base.rsplit_once('.')
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| base.to_string())
}

fn resolve_path(roots: &HashMap<i64, String>, root_id: Option<i64>, rel_path: &str) -> String {
    if let Some(id) = root_id {
        if let Some(root) = roots.get(&id) {
            return join_root(root, rel_path);
        }
    }
    rel_path.to_string()
}

fn join_root(root: &str, rel: &str) -> String {
    let (rel_base, cue) = split_cue(rel);
    let root = root.trim_end_matches(['/', '\\']);
    let rel_base = rel_base.trim_start_matches(['/', '\\']);
    let joined = if rel_base.is_empty() {
        root.to_string()
    } else {
        format!("{root}\\{rel_base}")
    };
    match cue {
        Some(suffix) => format!("{joined}{suffix}"),
        None => joined,
    }
}

fn split_cue(path: &str) -> (&str, Option<&str>) {
    match path.find("#cue:") {
        Some(index) => (&path[..index], Some(&path[index..])),
        None => (path, None),
    }
}

/// Match a stored path to a library row. Full path first, then a unique file name.
fn find_track(tracks: &[Track], current_file: &str) -> Option<TrackId> {
    let key = path_key(current_file);
    if let Some(track) = tracks.iter().find(|track| path_key(&track.path) == key) {
        return Some(track.id);
    }
    let (base, cue) = split_cue(current_file.trim());
    let name = file_name_key(base);
    let mut found = None;
    for track in tracks {
        let (track_base, track_cue) = split_cue(&track.path);
        if track_cue != cue || file_name_key(track_base) != name {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(track.id);
    }
    found
}

fn file_name_key(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_lowercase()
}

/// Audio path without a `#cue:` suffix, lowercased, with slashes normalized.
pub(crate) fn audio_key(path: &str) -> String {
    let (base, _) = split_cue(path.trim());
    let base = base.strip_prefix(r"\\?\").unwrap_or(base);
    base.replace('/', "\\").to_lowercase()
}

pub(crate) fn cue_base(path: &str) -> Option<&str> {
    let (base, cue) = split_cue(path);
    cue.is_some().then_some(base)
}

/// Copy a cover from the full audio row, or from a sibling cue row, onto
/// cue tracks that were stored without one.
pub(crate) fn inherit_cue_covers(tracks: &mut [Track]) -> Vec<TrackId> {
    let mut covers: HashMap<String, PathBuf> = HashMap::new();
    for track in tracks.iter() {
        let Some(cover) = track.cover.clone() else {
            continue;
        };
        let base = cue_base(&track.path).unwrap_or(track.path.as_str());
        covers.entry(audio_key(base)).or_insert(cover);
    }
    let mut filled = Vec::new();
    for track in tracks.iter_mut() {
        if track.cover.is_some() {
            continue;
        }
        let Some(base) = cue_base(&track.path) else {
            continue;
        };
        let Some(cover) = covers.get(&audio_key(base)) else {
            continue;
        };
        track.cover = Some(cover.clone());
        filled.push(track.id);
    }
    filled
}

/// The full file is a container when the library also has its `#cue:` rows.
pub(crate) fn cue_container_ids(tracks: &[Track]) -> HashSet<TrackId> {
    let mut bases = HashSet::new();
    for track in tracks {
        if let Some(base) = cue_base(&track.path) {
            bases.insert(audio_key(base));
        }
    }
    if bases.is_empty() {
        return HashSet::new();
    }
    tracks
        .iter()
        .filter(|track| cue_base(&track.path).is_none() && bases.contains(&audio_key(&track.path)))
        .map(|track| track.id)
        .collect()
}

/// Give cue rows the embedded cover of their audio file, and remember it.
fn backfill_cue_covers(db: &Connection, tracks: &mut [Track]) {
    let filled = inherit_cue_covers(tracks);
    let mut rows = cover_rows(tracks, &filled);
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, track) in tracks.iter().enumerate() {
        if track.cover.is_some() {
            continue;
        }
        let Some(base) = cue_base(&track.path) else {
            continue;
        };
        groups.entry(base.to_string()).or_default().push(index);
    }
    for (audio, indexes) in groups {
        let path = Path::new(&audio);
        if !path.is_file() {
            continue;
        }
        let Some((cover_id, thumb)) = crate::ui::import::stored_cover(path) else {
            continue;
        };
        for index in indexes {
            tracks[index].cover = Some(thumb.clone());
            rows.push((tracks[index].id.0 as i64, cover_id.clone()));
        }
    }
    if rows.is_empty() {
        return;
    }
    let Ok(tx) = db.unchecked_transaction() else {
        return;
    };
    for (id, cover_id) in rows {
        let _ = tx.execute(
            "UPDATE tracks SET cover_id = ?1
              WHERE id = ?2 AND (cover_id IS NULL OR cover_id = '')",
            rusqlite::params![cover_id, id],
        );
    }
    let _ = tx.commit();
}

fn cover_rows(tracks: &[Track], ids: &[TrackId]) -> Vec<(i64, String)> {
    let mut rows = Vec::new();
    for id in ids {
        let Some(track) = tracks.iter().find(|track| track.id == *id) else {
            continue;
        };
        let Some(path) = track.cover.as_deref().and_then(|path| path.to_str()) else {
            continue;
        };
        let Some(cover_id) = crate::ui::import::cover_id_from_path(path) else {
            continue;
        };
        rows.push((id.0 as i64, cover_id));
    }
    rows
}

fn path_key(path: &str) -> String {
    let (base, cue) = split_cue(path.trim());
    let base = base
        .strip_prefix(r"\\?\")
        .unwrap_or(base)
        .replace('/', "\\")
        .to_lowercase();
    match cue {
        Some(suffix) => format!("{base}{suffix}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrics_key_is_stable() {
        assert_eq!(
            lyrics_cache_key("Hotline Bling", "Drake", "", 267.4),
            lyrics_cache_key(" hotline bling ", " DRAKE ", "", 267.0)
        );
    }

    #[test]
    fn installed_library_loads_when_present() {
        if !app_data_dir().join("library.db").is_file() {
            return;
        }
        let library = load().expect("open library");
        assert!(library.tracks.len() > 1000);
        assert!(library.playlists.len() > 1);
        assert!(library.tracks.iter().any(|track| track.cover.is_some()));
        assert!(library.tracks.iter().any(|track| track.lyrics.is_some()));
        assert!(library
            .tracks
            .iter()
            .any(|track| track.path.to_lowercase().ends_with(".mp3")
                || track.path.to_lowercase().contains(".flac")
                || track.path.contains("#cue:")));
        assert!(
            library.current.is_some(),
            "saved current_file should match a library track"
        );
    }

    fn sample_track(id: u32, path: &str) -> Track {
        Track {
            id: TrackId(id),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            duration: 10.0,
            liked: false,
            path: path.into(),
            cover: None,
            lyrics: None,
        }
    }

    #[test]
    fn saved_path_matches_ignoring_case_and_slashes() {
        let tracks = vec![sample_track(7, r"Z:\Music\Album\Song.mp3#cue:2")];
        assert_eq!(
            find_track(&tracks, "z:/music/album/song.mp3#cue:2"),
            Some(TrackId(7))
        );
        assert_eq!(
            find_track(&tracks, r"D:\moved\Song.mp3#cue:2"),
            Some(TrackId(7))
        );
        assert_eq!(find_track(&tracks, r"D:\moved\Song.mp3"), None);
    }

    #[test]
    fn missing_database_is_created_empty() {
        let path = std::env::temp_dir().join(format!(
            "muzeeka-fresh-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_file(&path);
        let library = load_db(&path).expect("create library");
        assert!(library.tracks.is_empty());
        assert!(library.playlists.is_empty());
        assert!(path.is_file());
        assert!((library.volume - 0.7).abs() < 1e-6);
        assert!(!library.was_playing);
        assert!(library.current.is_none());
        drop(library);
        let again = load_db(&path).expect("reopen library");
        assert!(again.tracks.is_empty());
        drop(again);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn playback_memory_roundtrip() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE app_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                active_playlist_id TEXT,
                current_file TEXT,
                volume REAL,
                shuffle_enabled INTEGER NOT NULL DEFAULT 0,
                repeat_mode TEXT NOT NULL DEFAULT 'off'
            );",
        )
        .unwrap();
        let memory = PlaybackMemory {
            active_playlist_id: Some("downloads".into()),
            playing_playlist_id: Some("downloads".into()),
            current_file: Some(r"Z:\Music\Song.mp3".into()),
            volume: 0.4,
            shuffle: true,
            repeat: RepeatMode::All,
            position: 42.5,
            was_playing: true,
        };
        save_playback(&db, &memory).unwrap();
        let loaded = load_state(&db).unwrap();
        assert_eq!(loaded.current_file.as_deref(), Some(r"Z:\Music\Song.mp3"));
        assert_eq!(loaded.playing_playlist_id.as_deref(), Some("downloads"));
        assert!((loaded.position - 42.5).abs() < 1e-6);
        assert!(loaded.was_playing);
        assert!(loaded.shuffle);
        assert_eq!(loaded.repeat, RepeatMode::All);
        assert!((loaded.volume - 0.4).abs() < 1e-6);
    }
}
