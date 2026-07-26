// Playlist persistence — saved to the app data directory as JSON.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::cue;
use crate::library::MusicFile;

/// Returns true if any track path/metadata was upgraded (e.g. multi-file #cue: → plain m4a).
fn repair_playlist_tracks(tracks: &mut Vec<MusicFile>) -> bool {
    let mut repaired = Vec::with_capacity(tracks.len());
    let mut changed = false;

    for mut track in tracks.drain(..) {
        if cue::is_cue_sheet_path(&track.path) {
            let expanded = cue::expand_cue_file(std::path::Path::new(&track.path));
            if expanded.is_empty() {
                // Keep original so user can see something went wrong.
                repaired.push(track);
            } else {
                changed = true;
                repaired.extend(expanded);
            }
            continue;
        }

        if cue::repair_track(&mut track) {
            changed = true;
        }
        repaired.push(track);
    }

    // Dedupe plain paths after multi-file #cue: → m4a rewrites.
    let mut seen = std::collections::HashSet::new();
    repaired.retain(|t| {
        let key = t.path.to_lowercase();
        if seen.contains(&key) {
            changed = true;
            false
        } else {
            seen.insert(key);
            true
        }
    });

    *tracks = repaired;
    changed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlaylist {
    pub id: String,
    pub name: String,
    pub tracks: Vec<MusicFile>,
    /// User-assigned cover image (cached under app data). When absent, UI picks a track cover.
    #[serde(default)]
    pub cover_path: Option<String>,
}

/// Minimal playlist info for pickers (settings download target, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistMeta {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaylistsData {
    pub playlists: Vec<SavedPlaylist>,
    pub active_playlist_id: Option<String>,
    #[serde(default)]
    pub playing_playlist_id: Option<String>,
    #[serde(default)]
    pub current_file: Option<String>,
    /// Last UI volume (0.0–1.0). Optional for older files; missing ≠ 0.
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub liked_paths: Vec<String>,
    #[serde(default)]
    pub all_paths: Vec<String>,
    #[serde(default)]
    pub shuffle_enabled: bool,
    /// `off`, `all`, or `one`
    #[serde(default)]
    pub repeat_mode: Option<String>,
}

pub fn playlists_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {}", e))?;

    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create app data dir: {}", e))?;

    Ok(dir.join("playlists.json"))
}

/// Returns true if playlist contents changed (repairs / missing prune).
fn prune_missing_tracks(data: &mut PlaylistsData) -> bool {
    let mut changed = false;
    for playlist in &mut data.playlists {
        if repair_playlist_tracks(&mut playlist.tracks) {
            changed = true;
        }
        let before = playlist.tracks.len();
        playlist.tracks.retain(cue::track_file_exists);
        if playlist.tracks.len() != before {
            changed = true;
        }
    }
    changed
}

fn parse_playlists_file(path: &PathBuf, prune: bool) -> Result<(PlaylistsData, bool), String> {
    if !path.exists() {
        return Ok((PlaylistsData::default(), false));
    }

    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read playlists file: {}", e))?;

    let mut data: PlaylistsData = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse playlists file: {}", e))?;

    let mut changed = false;
    if prune {
        changed = prune_missing_tracks(&mut data);
    }

    if let Some(active_id) = &data.active_playlist_id {
        if active_id != "__all__"
            && active_id != "__liked__"
            && !data.playlists.iter().any(|p| p.id == *active_id)
        {
            data.active_playlist_id = data.playlists.first().map(|p| p.id.clone());
            changed = true;
        }
    }

    Ok((data, changed))
}

pub fn load_playlists(app: &AppHandle) -> Result<PlaylistsData, String> {
    let path = playlists_path(app)?;
    let (data, changed) = parse_playlists_file(&path, true)?;
    // Persist multi-file CUE upgrades (m4a#cue:N → plain m4a) so durations stick.
    if changed {
        let _ = save_playlists(app, &data);
    }
    Ok(data)
}

/// Hot-path load for remote/polling — skips per-track filesystem checks.
pub fn load_playlists_fast(app: &AppHandle) -> Result<PlaylistsData, String> {
    let (data, _) = parse_playlists_file(&playlists_path(app)?, false)?;
    Ok(data)
}

fn write_file_atomic(path: &PathBuf, contents: &[u8]) -> Result<(), String> {
    let tmp_path = path.with_extension("json.tmp");
    let bak_path = path.with_extension("json.bak");

    let write_result = (|| {
        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| format!("Failed to create temporary playlists file: {}", e))?;
        use std::io::Write as _;
        file.write_all(contents)
            .map_err(|e| format!("Failed to write temporary playlists file: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to flush temporary playlists file: {}", e))?;
        drop(file);

        match fs::rename(&tmp_path, path) {
            Ok(()) => Ok(()),
            Err(first_error) if path.exists() => {
                let _ = fs::remove_file(&bak_path);
                fs::rename(path, &bak_path)
                    .map_err(|e| format!("Failed to back up playlists file before replace: {}", e))?;

                match fs::rename(&tmp_path, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&bak_path);
                        Ok(())
                    }
                    Err(second_error) => {
                        let _ = fs::rename(&bak_path, path);
                        Err(format!(
                            "Failed to replace playlists file: {}; original rename error: {}",
                            second_error, first_error
                        ))
                    }
                }
            }
            Err(error) => Err(format!("Failed to replace playlists file: {}", error)),
        }
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

pub fn save_playlists(app: &AppHandle, data: &PlaylistsData) -> Result<(), String> {
    let path = playlists_path(app)?;
    let json = serde_json::to_vec_pretty(data)
        .map_err(|e| format!("Failed to serialize playlists: {}", e))?;

    write_file_atomic(&path, &json)
}