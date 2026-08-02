// Library, playlist, and cover-related Tauri commands.

use tauri::{AppHandle, Emitter, State};

use crate::library;
use crate::player::Player;
use crate::playlists::{LibraryDatabase, LibraryState, PlaylistMeta, PlaylistsData};

fn make_scan_progress_emitter(
    app: AppHandle,
) -> impl Fn(usize, usize, &std::path::Path) + Send + Sync + 'static {
    library::make_throttled_scan_progress(move |current, total, label| {
        let _ = app.emit(
            "library:scan-progress",
            library::LibraryScanProgress {
                current,
                total,
                label: label.to_string(),
            },
        );
    })
}

/// Scan a directory recursively for music files.
#[tauri::command]
pub async fn library_scan(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
    directory: String,
) -> Result<Vec<library::MusicFile>, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = database.ensure_root(&directory) {
            eprintln!("[library_scan] ensure_root failed: {e}");
        }
        library::scan_directory_with_progress(&directory, &make_scan_progress_emitter(app))
    })
    .await
    .map_err(|e| format!("Scan task failed: {e}"))?
}

/// Scan dropped file and folder paths for music files.
#[tauri::command]
pub async fn library_scan_paths(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
    paths: Vec<String>,
) -> Result<Vec<library::MusicFile>, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        for path in &paths {
            let p = std::path::Path::new(path);
            if p.is_dir() {
                if let Err(e) = database.ensure_root(path) {
                    eprintln!("[library_scan_paths] ensure_root({path}) failed: {e}");
                }
            } else if let Some(parent) = p.parent() {
                if let Err(e) = database.ensure_root(&parent.to_string_lossy()) {
                    eprintln!("[library_scan_paths] ensure_root(parent) failed: {e}");
                }
            }
        }
        library::scan_paths_with_progress(&paths, &make_scan_progress_emitter(app))
    })
    .await
    .map_err(|e| format!("Scan task failed: {e}"))?
}

/// Read or refresh metadata for known file paths.
#[tauri::command]
pub async fn library_fetch_metadata(paths: Vec<String>) -> Result<Vec<library::MusicFile>, String> {
    tauri::async_runtime::spawn_blocking(move || library::fetch_metadata(&paths))
        .await
        .map_err(|e| format!("Metadata task failed: {e}"))?
}

fn cover_audio_path(path: &str) -> String {
    if crate::cue::is_cue_track_path(path) {
        if let Some((audio, _)) = crate::cue::parse_virtual_cue_path(path) {
            return audio;
        }
    }
    path.to_string()
}

fn cover_file_ok(path: &str) -> bool {
    !path.is_empty() && std::path::Path::new(path).is_file()
}

/// Prefer SQLite cover paths; extract from the audio file only on miss / stale / missing full.
fn resolve_covers_via_db(
    database: &LibraryDatabase,
    track_path: &str,
    need_full: bool,
) -> (Option<String>, Option<String>) {
    let audio = cover_audio_path(track_path);
    let keys = if audio == track_path {
        vec![track_path.to_string()]
    } else {
        vec![track_path.to_string(), audio.clone()]
    };

    let mut thumb: Option<String> = None;
    let mut full: Option<String> = None;
    for key in &keys {
        if let Ok(Some((db_thumb, db_full))) = database.get_track_covers(key) {
            if thumb.is_none() {
                thumb = db_thumb.filter(|p| cover_file_ok(p));
            }
            if full.is_none() {
                full = db_full
                    .filter(|p| cover_file_ok(p) && !crate::metadata::is_thumb_cache_path(p));
            }
        }
    }

    let have_what_we_need = thumb.is_some() && (!need_full || full.is_some());
    if have_what_we_need {
        return (thumb, full);
    }

    // Miss / wiped covers dir / thumb-only when fullscreen asked for full.
    let extract_path = std::path::Path::new(&audio);
    if !extract_path.is_file() {
        return (thumb, full);
    }
    let covers = crate::metadata::extract_covers_for_file(extract_path);
    let new_thumb = covers.thumb.or(thumb);
    let new_full = covers
        .full
        .filter(|p| !crate::metadata::is_thumb_cache_path(p))
        .or(full);

    if new_thumb.is_some() || new_full.is_some() {
        for key in &keys {
            if let Err(e) = database.set_track_covers(
                key,
                new_thumb.as_deref(),
                new_full.as_deref(),
            ) {
                eprintln!("[resolve_covers] set_track_covers({key}) failed: {e}");
            }
        }
    }
    (new_thumb, new_full)
}

/// Resolve the small cover used by virtualized lists and the transport bar.
#[tauri::command]
pub async fn library_resolve_cover(
    database: State<'_, LibraryDatabase>,
    path: String,
) -> Result<Option<String>, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (thumb, full) = resolve_covers_via_db(&database, &path, false);
        Ok(thumb.or(full))
    })
    .await
    .map_err(|e| format!("Cover resolve task failed: {e}"))?
}

/// Resolve a full-resolution cover path for a track (creates cache if needed).
#[tauri::command]
pub async fn library_resolve_full_cover(
    database: State<'_, LibraryDatabase>,
    path: String,
) -> Result<Option<String>, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_thumb, full) = resolve_covers_via_db(&database, &path, true);
        Ok(full)
    })
    .await
    .map_err(|e| format!("Cover resolve task failed: {e}"))?
}

/// Return a data URL for a cover image file (works for library paths outside asset scope).
#[tauri::command]
pub fn library_cover_data_url(path: String) -> Result<Option<String>, String> {
    use std::path::Path;
    crate::metadata::cover_data_url(Path::new(&path))
}

/// Wipe the track cover cache, re-extract covers as WebP, convert playlist GIF/JPG → WebP.
#[tauri::command]
pub async fn library_rebuild_covers(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
) -> Result<crate::metadata::CoverRebuildStats, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut data = database.load()?;
        let track_paths: Vec<String> = data
            .library_tracks
            .iter()
            .map(|track| track.path.clone())
            .collect();

        let playlist_covers: Vec<(String, Option<String>)> = data
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.cover_path.clone()))
            .collect();

        let (stats, cover_updates) =
            crate::metadata::rebuild_cover_cache(&track_paths, &playlist_covers)?;

        // Refresh track cover paths from regenerated cache (clear stale paths too).
        for track in &mut data.library_tracks {
            let (thumb, full) = crate::metadata::fresh_cover_paths_for_track(&track.path);
            track.cover_path = thumb;
            track.cover_path_full = full;
        }
        database.upsert_tracks(&data.library_tracks)?;

        for (id, path) in cover_updates {
            if let Err(e) = database.set_playlist_cover(&id, path.as_deref()) {
                eprintln!("[library_rebuild_covers] set_playlist_cover({id}) failed: {e}");
            }
        }

        if let Err(e) = app.emit("covers:rebuilt", &stats) {
            eprintln!("[library_rebuild_covers] emit covers:rebuilt failed: {e}");
        }
        Ok(stats)
    })
    .await
    .map_err(|e| format!("Cover rebuild task failed: {e}"))?
}

// ── Playlist persistence ──────────────────────────────────────────────────────

/// Load saved playlists from disk (full data + missing-track prune).
/// Runs off the async runtime so path checks cannot freeze the UI thread.
#[tauri::command]
pub async fn playlists_load(database: State<'_, LibraryDatabase>) -> Result<PlaylistsData, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.load())
        .await
        .map_err(|e| format!("Playlists load task failed: {e}"))?
}

/// Lightweight playlist list for UI pickers (id + name only, no prune / no tracks).
/// Safe to call from secondary windows on open — will not walk the filesystem.
#[tauri::command]
pub fn playlists_list_meta(
    database: State<'_, LibraryDatabase>,
) -> Result<Vec<PlaylistMeta>, String> {
    database.list_meta()
}

/// Save playlists to disk.
#[tauri::command]
pub fn library_state_save(
    database: State<'_, LibraryDatabase>,
    state: LibraryState,
) -> Result<(), String> {
    database.save_state(&state)
}

#[tauri::command]
pub fn playlist_create(
    database: State<'_, LibraryDatabase>,
    id: String,
    name: String,
) -> Result<(), String> {
    database.create_playlist(&id, &name)
}

#[tauri::command]
pub fn playlist_delete(database: State<'_, LibraryDatabase>, id: String) -> Result<(), String> {
    database.delete_playlist(&id)
}

#[tauri::command]
pub fn playlist_rename(
    database: State<'_, LibraryDatabase>,
    id: String,
    name: String,
) -> Result<(), String> {
    database.rename_playlist(&id, &name)
}

#[tauri::command]
pub fn playlist_set_cover_path(
    database: State<'_, LibraryDatabase>,
    id: String,
    cover_path: Option<String>,
) -> Result<(), String> {
    database.set_playlist_cover(&id, cover_path.as_deref())
}

#[tauri::command]
pub async fn library_tracks_upsert(
    database: State<'_, LibraryDatabase>,
    tracks: Vec<library::MusicFile>,
) -> Result<(), String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.upsert_tracks(&tracks))
        .await
        .map_err(|error| format!("Track upsert task failed: {error}"))?
}

#[tauri::command]
pub async fn playlist_add_tracks(
    database: State<'_, LibraryDatabase>,
    playlist_id: String,
    tracks: Vec<library::MusicFile>,
) -> Result<(), String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        database.add_tracks_to_playlist(&playlist_id, &tracks)
    })
    .await
    .map_err(|error| format!("Playlist import task failed: {error}"))?
}

#[tauri::command]
pub fn playlist_remove_tracks(
    database: State<'_, LibraryDatabase>,
    playlist_id: String,
    paths: Vec<String>,
) -> Result<(), String> {
    database.remove_tracks_from_playlist(&playlist_id, &paths)
}

#[tauri::command]
pub fn playlist_reorder(
    database: State<'_, LibraryDatabase>,
    playlist_id: String,
    paths: Vec<String>,
) -> Result<(), String> {
    database.reorder_playlist(&playlist_id, &paths)
}

#[tauri::command]
pub fn library_reorder(
    database: State<'_, LibraryDatabase>,
    paths: Vec<String>,
) -> Result<(), String> {
    database.reorder_library(&paths)
}

#[tauri::command]
pub fn library_clear_all(
    app: AppHandle,
    player: State<'_, Player>,
    database: State<'_, LibraryDatabase>,
) -> Result<(), String> {
    if let Err(e) = player.stop() {
        eprintln!("[library_clear_all] player.stop failed: {e}");
    }
    database.clear_all()?;
    if let Err(e) = app.emit("library:cleared", ()) {
        eprintln!("[library_clear_all] emit library:cleared failed: {e}");
    }
    Ok(())
}

#[tauri::command]
pub fn library_remove_tracks(
    database: State<'_, LibraryDatabase>,
    paths: Vec<String>,
) -> Result<(), String> {
    database.remove_library_tracks(&paths)
}

#[tauri::command]
pub fn library_set_liked(
    database: State<'_, LibraryDatabase>,
    path: String,
    liked: bool,
) -> Result<(), String> {
    database.set_liked(&path, liked)
}

#[tauri::command]
pub fn library_reorder_liked(
    database: State<'_, LibraryDatabase>,
    paths: Vec<String>,
) -> Result<(), String> {
    database.reorder_liked(&paths)
}

/// Cache a user-selected image as a playlist cover.
#[tauri::command]
pub fn playlist_cache_cover(playlist_id: String, source_path: String) -> Result<String, String> {
    crate::metadata::cache_playlist_cover(&playlist_id, std::path::Path::new(&source_path))
}

/// Download a remote image and store it as the playlist cover.
#[tauri::command]
pub fn playlist_cache_cover_url(playlist_id: String, url: String) -> Result<String, String> {
    crate::metadata::cache_playlist_cover_from_url(&playlist_id, &url)
}

/// Delete a cached custom playlist cover file.
#[tauri::command]
pub fn playlist_remove_cover(playlist_id: String) -> Result<(), String> {
    crate::metadata::remove_playlist_cover_file(&playlist_id)
}
