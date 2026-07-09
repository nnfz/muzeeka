use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::equalizer::EqualizerSettings;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CustomPreset {
    pub name: String,
    pub preamp_db: f32,
    #[serde(default)]
    pub bands_db: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppSettings {
    #[serde(default)]
    pub equalizer: EqualizerSettings,
    #[serde(default)]
    pub custom_presets: Vec<CustomPreset>,
    /// Playback rate multiplier. 1.0 = normal. Persisted so it survives restarts.
    #[serde(default)]
    pub playback_rate: f32,
    /// Custom folder for yt-dlp downloads. Falls back to app_data/downloads.
    #[serde(default)]
    pub download_folder: Option<String>,
    /// Playlist ID to auto-add downloaded tracks. Falls back to "Downloads" playlist.
    #[serde(default)]
    pub download_playlist_id: Option<String>,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {}", e))?;

    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create app data dir: {}", e))?;

    Ok(dir.join("settings.json"))
}

pub fn load_settings(app: &AppHandle) -> Result<AppSettings, String> {
    let path = settings_path(app)?;

    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read settings file: {}", e))?;

    serde_json::from_str(&raw).map_err(|e| format!("Failed to parse settings file: {}", e))
}

pub fn save_settings(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    fs::write(&path, json).map_err(|e| format!("Failed to write settings file: {}", e))
}