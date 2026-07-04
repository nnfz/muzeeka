// Playlist persistence — saved to the app data directory as JSON.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::library::MusicFile;

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
    pub volume: Option<f32>,
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
        playlist.tracks.retain(|track| std::path::Path::new(&track.path).exists());
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

pub fn save_playlists(app: &AppHandle, data: &PlaylistsData) -> Result<(), String> {
    let path = playlists_path(app)?;
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| format!("Failed to serialize playlists: {}", e))?;

    fs::write(&path, json).map_err(|e| format!("Failed to write playlists file: {}", e))
}