// Playlist persistence — saved to the app data directory as JSON.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::cue;
use crate::library::MusicFile;

fn repair_playlist_tracks(tracks: &mut Vec<MusicFile>) {
    let mut repaired = Vec::with_capacity(tracks.len());

    for mut track in tracks.drain(..) {
        if cue::is_cue_sheet_path(&track.path) {
            repaired.extend(cue::expand_cue_file(std::path::Path::new(&track.path)));
            continue;
        }

        cue::repair_track(&mut track);
        repaired.push(track);
    }

    *tracks = repaired;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlaylist {
    pub id: String,
    pub name: String,
    pub tracks: Vec<MusicFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaylistsData {
    pub playlists: Vec<SavedPlaylist>,
    pub active_playlist_id: Option<String>,
    #[serde(default)]
    pub playing_playlist_id: Option<String>,
    #[serde(default)]
    pub current_file: Option<String>,
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

fn playlists_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {}", e))?;

    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create app data dir: {}", e))?;

    Ok(dir.join("playlists.json"))
}

fn prune_missing_tracks(data: &mut PlaylistsData) {
    for playlist in &mut data.playlists {
        repair_playlist_tracks(&mut playlist.tracks);
        playlist.tracks.retain(cue::track_file_exists);
    }
}

pub fn load_playlists(app: &AppHandle) -> Result<PlaylistsData, String> {
    let path = playlists_path(app)?;

    if !path.exists() {
        return Ok(PlaylistsData::default());
    }

    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read playlists file: {}", e))?;

    let mut data: PlaylistsData = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse playlists file: {}", e))?;

    prune_missing_tracks(&mut data);

    if let Some(active_id) = &data.active_playlist_id {
        if !data.playlists.iter().any(|p| p.id == *active_id) {
            data.active_playlist_id = data.playlists.first().map(|p| p.id.clone());
        }
    }

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