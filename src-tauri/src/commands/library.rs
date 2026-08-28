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

/// Payload for the track Properties editor.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TrackMetadataUpdate {
    pub path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub genre: Option<String>,
    /// Snapshot fields preserved for CUE / DB-only updates when we cannot re-read the file.
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub cover_path: Option<String>,
    #[serde(default)]
    pub cover_path_full: Option<String>,
    #[serde(default)]
    pub audio_path: Option<String>,
    #[serde(default)]
    pub cue_start_secs: Option<f64>,
    #[serde(default)]
    pub cue_end_secs: Option<f64>,
}

/// Result of writing tags + updating the library row.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackMetadataUpdateResult {
    pub track: library::MusicFile,
    /// True when tags were written into the on-disk audio file (plain file or CUE image).
    pub wrote_to_file: bool,
}

/// Resolve the real audio file that should receive embedded tags.
/// CUE virtual paths (`image.flac#cue:3`) map to the container audio image.
fn tag_write_path(track_path: &str, audio_path_hint: Option<&str>) -> Result<std::path::PathBuf, String> {
    if crate::cue::is_cue_track_path(track_path) {
        let audio = audio_path_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                crate::cue::parse_virtual_cue_path(track_path).map(|(audio, _)| audio)
            })
            .ok_or_else(|| format!("Could not resolve CUE audio file for: {track_path}"))?;
        let p = std::path::PathBuf::from(&audio);
        if !p.is_file() {
            return Err(format!("CUE audio file not found: {audio}"));
        }
        return Ok(p);
    }

    let p = std::path::PathBuf::from(track_path);
    if !p.is_file() {
        return Err(format!("File not found: {track_path}"));
    }
    Ok(p)
}

/// Probe bitrate / sample rate for the Properties window (CUE → audio image).
#[tauri::command]
pub async fn library_audio_tech_info(
    database: State<'_, LibraryDatabase>,
    path: String,
    audio_path: Option<String>,
) -> Result<crate::metadata::AudioTechInfo, String> {
    let track_path = path.clone();
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let write_path = tag_write_path(path.trim(), audio_path.as_deref())?;
        let mut info = crate::metadata::read_audio_tech_info(&write_path);
        // Play stats are keyed by the playlist/virtual track path (incl. #cue:N).
        if let Ok(stats) = database.get_playback_stats(track_path.trim()) {
            info.play_count = Some(stats.play_count);
            info.first_played_unix = stats.first_played_unix;
            info.last_played_unix = stats.last_played_unix;
        }
        Ok(info)
    })
    .await
    .map_err(|e| format!("Audio tech probe failed: {e}"))?
}

/// Foobar-style full tag dump for the Properties → Metadata table.
#[tauri::command]
pub async fn library_get_tag_table(
    path: String,
    audio_path: Option<String>,
) -> Result<Vec<crate::tag_table::TagTableRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let write_path = tag_write_path(path.trim(), audio_path.as_deref())?;
        crate::tag_table::read_tag_table(&write_path)
    })
    .await
    .map_err(|e| format!("Tag table read failed: {e}"))?
}

/// Write the full tag table back to the audio file and refresh library fields.
#[tauri::command]
pub async fn library_set_tag_table(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
    path: String,
    audio_path: Option<String>,
    rows: Vec<crate::tag_table::TagTableRow>,
    snapshot: Option<library::MusicFile>,
) -> Result<library::MusicFile, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let track_path = path.trim().to_string();
        if track_path.is_empty() {
            return Err("Empty track path".to_string());
        }
        let write_path = tag_write_path(&track_path, audio_path.as_deref())?;
        crate::tag_table::write_tag_table(&write_path, &rows)?;

        let fields = crate::tag_table::track_fields_from_table(&rows);
        let is_cue = crate::cue::is_cue_track_path(&track_path);

        let track = if is_cue {
            let snap = snapshot.unwrap_or(library::MusicFile {
                path: track_path.clone(),
                file_name: track_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&track_path)
                    .to_string(),
                extension: write_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase(),
                size: 0,
                title: None,
                artist: None,
                album: None,
                duration_secs: None,
                year: None,
                track_number: None,
                genre: None,
                cover_path: None,
                cover_path_full: None,
                audio_path: None,
                cue_start_secs: None,
                cue_end_secs: None,
            });
            let covers = crate::metadata::extract_covers_for_file(&write_path);
            library::MusicFile {
                path: track_path,
                file_name: snap.file_name,
                extension: snap.extension,
                size: if snap.size > 0 {
                    snap.size
                } else {
                    std::fs::metadata(&write_path)
                        .map(|m| m.len())
                        .unwrap_or(0)
                },
                title: fields.title.or(snap.title),
                artist: fields.artist.or(snap.artist),
                album: fields.album.or(snap.album),
                duration_secs: snap.duration_secs,
                year: fields.year.or(snap.year),
                track_number: fields.track_number.or(snap.track_number),
                genre: fields.genre.or(snap.genre),
                cover_path: covers.thumb.or(snap.cover_path),
                cover_path_full: covers.full.or(snap.cover_path_full),
                audio_path: Some(write_path.to_string_lossy().into_owned()),
                cue_start_secs: snap.cue_start_secs,
                cue_end_secs: snap.cue_end_secs,
            }
        } else {
            let mut files = library::fetch_metadata(&[track_path.clone()])?;
            let mut file = files
                .pop()
                .ok_or_else(|| format!("Could not re-read track: {track_path}"))?;
            // Prefer table values for the core columns we just wrote.
            file.title = fields.title.or(file.title.take());
            file.artist = fields.artist.or(file.artist.take());
            file.album = fields.album;
            file.genre = fields.genre;
            file.year = fields.year;
            file.track_number = fields.track_number;
            file
        };

        database.upsert_tracks(&[track.clone()])?;
        if let Err(e) = app.emit("track:metadata-updated", &track) {
            eprintln!("[library_set_tag_table] emit failed: {e}");
        }
        Ok(track)
    })
    .await
    .map_err(|e| format!("Tag table write failed: {e}"))?
}

/// Embed a new cover image into the audio file, refresh cover cache, upsert library.
/// `snapshot` preserves CUE segment fields / current tags when re-read is partial.
#[tauri::command]
pub async fn library_set_track_cover(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
    path: String,
    image_path: String,
    snapshot: Option<library::MusicFile>,
) -> Result<library::MusicFile, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let track_path = path.trim().to_string();
        if track_path.is_empty() {
            return Err("Empty track path".to_string());
        }
        let image = std::path::PathBuf::from(image_path.trim());
        if !image.is_file() {
            return Err(format!("Image not found: {}", image.display()));
        }
        let bytes = std::fs::read(&image).map_err(|e| format!("Failed to read image: {e}"))?;
        if bytes.is_empty() {
            return Err("Image file is empty".to_string());
        }
        let mime_hint = image
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| match e.to_ascii_lowercase().as_str() {
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "bmp" => "image/bmp",
                "tif" | "tiff" => "image/tiff",
                _ => "image/jpeg",
            });

        let audio_hint = snapshot
            .as_ref()
            .and_then(|t| t.audio_path.as_deref());
        let write_path = tag_write_path(&track_path, audio_hint)?;
        eprintln!(
            "[library_set_track_cover] embedding {} bytes into {}",
            bytes.len(),
            write_path.display()
        );
        crate::metadata::write_track_cover(&write_path, &bytes, mime_hint)?;

        // Rebuild content-addressed cover cache from the freshly tagged file.
        let covers = crate::metadata::extract_covers_for_file(&write_path);
        if covers.thumb.is_none() && covers.full.is_none() {
            eprintln!(
                "[library_set_track_cover] warning: wrote cover but extract found none for {}",
                write_path.display()
            );
        }

        let is_cue = crate::cue::is_cue_track_path(&track_path);
        let track = if is_cue {
            let snap = snapshot.unwrap_or(library::MusicFile {
                path: track_path.clone(),
                file_name: track_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&track_path)
                    .to_string(),
                extension: write_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase(),
                size: 0,
                title: None,
                artist: None,
                album: None,
                duration_secs: None,
                year: None,
                track_number: None,
                genre: None,
                cover_path: None,
                cover_path_full: None,
                audio_path: None,
                cue_start_secs: None,
                cue_end_secs: None,
            });
            let audio = write_path.to_string_lossy().into_owned();
            let file_size = if snap.size > 0 {
                snap.size
            } else {
                std::fs::metadata(&write_path)
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            library::MusicFile {
                path: track_path,
                file_name: snap.file_name,
                extension: if snap.extension.is_empty() {
                    write_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase()
                } else {
                    snap.extension
                },
                size: file_size,
                title: snap.title,
                artist: snap.artist,
                album: snap.album,
                duration_secs: snap.duration_secs,
                year: snap.year,
                track_number: snap.track_number,
                genre: snap.genre,
                cover_path: covers.thumb,
                cover_path_full: covers.full,
                audio_path: Some(audio),
                cue_start_secs: snap.cue_start_secs,
                cue_end_secs: snap.cue_end_secs,
            }
        } else {
            let mut files = library::fetch_metadata(&[track_path.clone()])?;
            let mut file = files
                .pop()
                .ok_or_else(|| format!("Could not re-read track: {track_path}"))?;
            file.cover_path = covers.thumb.or(file.cover_path.take());
            file.cover_path_full = covers.full.or(file.cover_path_full.take());
            file
        };

        database.upsert_tracks(&[track.clone()])?;
        if let Err(e) = app.emit("track:metadata-updated", &track) {
            eprintln!("[library_set_track_cover] emit failed: {e}");
        }
        Ok(track)
    })
    .await
    .map_err(|e| format!("Cover update task failed: {e}"))?
}

/// Write track tags into the source audio file and upsert SQLite.
#[tauri::command]
pub async fn library_update_track_metadata(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
    update: TrackMetadataUpdate,
) -> Result<TrackMetadataUpdateResult, String> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = update.path.trim().to_string();
        if path.is_empty() {
            return Err("Empty track path".to_string());
        }

        let tags = crate::metadata::TrackTags {
            title: update.title.clone(),
            artist: update.artist.clone(),
            album: update.album.clone(),
            year: update.year,
            track_number: update.track_number,
            genre: update.genre.clone(),
        };

        let is_cue = crate::cue::is_cue_track_path(&path);
        let write_path = tag_write_path(&path, update.audio_path.as_deref())?;
        crate::metadata::write_track_metadata(&write_path, &tags)?;
        let wrote_to_file = true;

        let clean_text = |value: &Option<String>| -> Option<String> {
            value
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };

        let track = if is_cue {
            // Keep CUE segment identity; tags live on the shared audio image.
            let audio_path = write_path.to_string_lossy().into_owned();
            let file_name = update
                .file_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    path.rsplit(['\\', '/'])
                        .next()
                        .unwrap_or(&path)
                        .to_string()
                });
            let size = if update.size.unwrap_or(0) > 0 {
                update.size.unwrap_or(0)
            } else {
                std::fs::metadata(&write_path)
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            let extension = update.extension.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| {
                write_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
            });

            // Prefer covers already known for this row; fall back to re-read from image.
            let (cover_path, cover_path_full) =
                if update.cover_path.is_some() || update.cover_path_full.is_some() {
                    (update.cover_path.clone(), update.cover_path_full.clone())
                } else {
                    let meta = crate::metadata::read_metadata(
                        &write_path,
                        write_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(""),
                    );
                    (meta.cover_path, meta.cover_path_full)
                };

            library::MusicFile {
                path: path.clone(),
                file_name,
                extension,
                size,
                title: clean_text(&tags.title),
                artist: clean_text(&tags.artist),
                album: clean_text(&tags.album),
                duration_secs: update.duration_secs,
                year: tags.year.filter(|&y| y > 0),
                track_number: tags.track_number.filter(|&n| n > 0),
                genre: clean_text(&tags.genre),
                cover_path,
                cover_path_full,
                audio_path: Some(audio_path),
                cue_start_secs: update.cue_start_secs,
                cue_end_secs: update.cue_end_secs,
            }
        } else {
            let mut files = library::fetch_metadata(&[path.clone()])?;
            let mut file = files
                .pop()
                .ok_or_else(|| format!("Could not re-read tags: {path}"))?;
            // Mirror what the editor saved (file re-read can lag / normalize encoding).
            file.title = clean_text(&tags.title);
            file.artist = clean_text(&tags.artist);
            file.album = clean_text(&tags.album);
            file.genre = clean_text(&tags.genre);
            file.year = tags.year.filter(|&y| y > 0);
            file.track_number = tags.track_number.filter(|&n| n > 0);
            file
        };

        database.upsert_tracks(&[track.clone()])?;

        if let Err(e) = app.emit("track:metadata-updated", &track) {
            eprintln!("[library_update_track_metadata] emit failed: {e}");
        }

        Ok(TrackMetadataUpdateResult {
            track,
            wrote_to_file,
        })
    })
    .await
    .map_err(|e| format!("Metadata update task failed: {e}"))?
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
    if path.is_empty() {
        return false;
    }
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return false;
    }
    // Reject empty/truncated race leftovers so we re-extract instead of serving them.
    crate::metadata::cached_cover_file_ok(p)
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

    // Missing 96px WebP: rebuild from the cached full JPEG. Never serve the
    // 720px full as a list thumb (that is what made the UI hitch).
    if thumb.is_none() {
        if let Some(full_path) = full.as_deref() {
            if let Some(id) = crate::metadata::cover_id_from_cache_path(full_path) {
                thumb = crate::metadata::rebuild_list_thumb_from_full(&id);
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
        let (thumb, _full) = resolve_covers_via_db(&database, &path, false);
        Ok(thumb)
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
pub fn playlist_set_mix_mode(
    database: State<'_, LibraryDatabase>,
    id: String,
    mix_mode: bool,
) -> Result<(), String> {
    database.set_playlist_mix_mode(&id, mix_mode)
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

/// Persist the sidebar order of playlists (drag & drop reorder).
#[tauri::command]
pub fn playlists_reorder(
    database: State<'_, LibraryDatabase>,
    ids: Vec<String>,
) -> Result<(), String> {
    database.reorder_playlists(&ids)
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

/// Per-track playback rate override (`None` = use global Settings rate).
#[tauri::command]
pub fn track_prefs_get_playback_rate(
    database: State<'_, LibraryDatabase>,
    path: String,
) -> Result<Option<f32>, String> {
    database.get_track_playback_rate(&path)
}

/// Set or clear per-track playback rate. Pass `null` to remove the override.
#[tauri::command]
pub fn track_prefs_set_playback_rate(
    database: State<'_, LibraryDatabase>,
    path: String,
    rate: Option<f32>,
) -> Result<(), String> {
    database.set_track_playback_rate(&path, rate)
}

/// Waveform peaks for the Mix transition editor (top/bottom track strips).
#[tauri::command]
pub async fn library_get_waveform(
    player: State<'_, Player>,
    cache: State<'_, crate::waveform::WaveformCache>,
    path: String,
    audio_path: Option<String>,
    bins: Option<usize>,
    cue_start_secs: Option<f64>,
    cue_end_secs: Option<f64>,
) -> Result<crate::waveform::WaveformPeaks, String> {
    let write_path = tag_write_path(path.trim(), audio_path.as_deref())?;
    let path_str = write_path.to_string_lossy().into_owned();
    let player = player.inner().clone();
    let cache = cache.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::waveform::peaks_for_path(
            &player,
            &cache,
            &path_str,
            bins,
            cue_start_secs,
            cue_end_secs,
        )
    })
    .await
    .map_err(|e| format!("Waveform task failed: {e}"))?
}

/// Detect BPM by analyzing the audio stream (BASS decode + energy autocorrelation).
#[tauri::command]
pub async fn library_detect_bpm(
    player: State<'_, Player>,
    path: String,
    audio_path: Option<String>,
) -> Result<f32, String> {
    let write_path = tag_write_path(path.trim(), audio_path.as_deref())?;
    let path_str = write_path.to_string_lossy().into_owned();
    // Clone the managed Player handle (same process-wide BASS device).
    let player = player.inner().clone();
    // Heavy decode must not sit on the async runtime; BASS work is then
    // hopped onto the audio/main thread inside `detect_bpm_for_path`.
    match tauri::async_runtime::spawn_blocking(move || {
        crate::bpm::detect_bpm_for_path(&player, &path_str)
    })
    .await
    {
        Ok(Ok(bpm)) => Ok(bpm),
        Ok(Err(e)) => {
            eprintln!("[library_detect_bpm] {e}");
            Err(e)
        }
        Err(e) => Err(format!("BPM detect task failed: {e}")),
    }
}

/// Read BPM currently stored in the file tags (if any).
#[tauri::command]
pub async fn library_get_bpm(
    path: String,
    audio_path: Option<String>,
) -> Result<Option<f32>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let write_path = tag_write_path(path.trim(), audio_path.as_deref())?;
        Ok(crate::tag_table::read_bpm(&write_path))
    })
    .await
    .map_err(|e| format!("BPM read task failed: {e}"))?
}

/// Align beat-grid phase to kicks/onsets for a known BPM.
/// Returns offset seconds in [0, beat_period) from track start.
#[tauri::command]
pub async fn library_detect_beat_offset(
    player: State<'_, Player>,
    path: String,
    audio_path: Option<String>,
    bpm: f32,
) -> Result<f64, String> {
    let write_path = tag_write_path(path.trim(), audio_path.as_deref())?;
    let path_str = write_path.to_string_lossy().into_owned();
    let player = player.inner().clone();
    match tauri::async_runtime::spawn_blocking(move || {
        crate::bpm::detect_beat_offset_for_path(&player, &path_str, bpm)
    })
    .await
    {
        Ok(Ok(off)) => Ok(off),
        Ok(Err(e)) => {
            eprintln!("[library_detect_beat_offset] {e}");
            Err(e)
        }
        Err(e) => Err(format!("Beat align task failed: {e}")),
    }
}

/// Write BPM into the audio file tags (`Bpm` + `IntegerBpm`).
#[tauri::command]
pub async fn library_set_track_bpm(
    app: AppHandle,
    database: State<'_, LibraryDatabase>,
    path: String,
    audio_path: Option<String>,
    bpm: f32,
    snapshot: Option<library::MusicFile>,
) -> Result<library::MusicFile, String> {
    if !bpm.is_finite() || bpm <= 0.0 || bpm >= 1000.0 {
        return Err("BPM must be between 1 and 999".into());
    }
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let track_path = path.trim().to_string();
        if track_path.is_empty() {
            return Err("Empty track path".to_string());
        }
        let write_path = tag_write_path(&track_path, audio_path.as_deref())?;
        crate::tag_table::write_bpm(&write_path, bpm)?;

        // Re-read library row (covers/duration stay intact).
        let is_cue = crate::cue::is_cue_track_path(&track_path);
        let track = if is_cue {
            let mut snap = snapshot.unwrap_or(library::MusicFile {
                path: track_path.clone(),
                file_name: track_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&track_path)
                    .to_string(),
                extension: write_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase(),
                size: 0,
                title: None,
                artist: None,
                album: None,
                duration_secs: None,
                year: None,
                track_number: None,
                genre: None,
                cover_path: None,
                cover_path_full: None,
                audio_path: None,
                cue_start_secs: None,
                cue_end_secs: None,
            });
            snap.path = track_path;
            snap.audio_path = Some(write_path.to_string_lossy().into_owned());
            snap
        } else {
            let mut files = library::fetch_metadata(&[track_path.clone()])?;
            files
                .pop()
                .ok_or_else(|| format!("Could not re-read track: {track_path}"))?
        };

        database.upsert_tracks(&[track.clone()])?;
        if let Err(e) = app.emit("track:metadata-updated", &track) {
            eprintln!("[library_set_track_bpm] emit failed: {e}");
        }
        Ok(track)
    })
    .await
    .map_err(|e| format!("BPM write task failed: {e}"))?
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
