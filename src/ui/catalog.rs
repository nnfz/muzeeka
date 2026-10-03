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
    pub db: Connection,
}

pub fn load() -> Result<LoadedLibrary, String> {
    let root = app_data_dir();
    let db_path = root.join("library.db");
    if !db_path.is_file() {
        return Err(format!(
            "Library database was not found at {}",
            db_path.display()
        ));
    }

    let db = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| format!("Failed to open {}: {error}", db_path.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    db.pragma_update(None, "foreign_keys", true)
        .map_err(|error| error.to_string())?;

    let roots = load_roots(&db)?;
    let covers = index_covers(&root.join("covers"));
    let lyrics = index_lyrics(&root.join("lyrics"));
    let tracks = load_tracks(&db, &roots, &covers, &lyrics)?;
    let by_id: HashMap<u32, usize> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.id.0, index))
        .collect();
    let playlists = load_playlists(&db, &root.join("playlist_covers"), &tracks, &by_id)?;
    let (view, current_file, volume, shuffle, repeat, position) = load_state(&db)?;
    let view = match view {
        Some(id) if playlists.iter().any(|playlist| playlist.id == id) => {
            LibraryView::Playlist(id)
        }
        _ => LibraryView::All,
    };
    let current = current_file.as_deref().and_then(|path| {
        let key = path_key(path);
        tracks
            .iter()
            .find(|track| path_key(&track.path) == key)
            .map(|track| track.id)
    });
    let position = if current.is_some() { position } else { 0.0 };

    Ok(LoadedLibrary {
        tracks,
        playlists,
        view,
        current,
        position,
        volume,
        shuffle,
        repeat,
        db,
    })
}

fn load_roots(db: &Connection) -> Result<HashMap<i64, String>, String> {
    let mut statement = db
        .prepare("SELECT id, path FROM library_roots")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
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
            .prepare("SELECT track_id FROM liked_tracks")
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
    fs::create_dir_all(&dir).map_err(|error| format!("Could not create the cover folder: {error}"))?;
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

fn load_state(
    db: &Connection,
) -> Result<(Option<String>, Option<String>, f32, bool, RepeatMode, f64), String> {
    db.query_row(
        "SELECT active_playlist_id, current_file, volume, shuffle_enabled, repeat_mode,
                playback_position
           FROM app_state WHERE id = 1",
        [],
        |row| {
            let volume: Option<f64> = row.get(2)?;
            let repeat: Option<String> = row.get(4)?;
            let position: Option<f64> = row.get(5)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                volume.map(|value| value as f32).unwrap_or(0.7).clamp(0.0, 1.0),
                row.get::<_, i64>(3)? != 0,
                match repeat.as_deref() {
                    Some("all") => RepeatMode::All,
                    Some("one") => RepeatMode::One,
                    _ => RepeatMode::Off,
                },
                position.unwrap_or(0.0).max(0.0),
            ))
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
    }
}
