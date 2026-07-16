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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub equalizer: EqualizerSettings,
    #[serde(default)]
    pub custom_presets: Vec<CustomPreset>,
    /// Playback rate multiplier. 1.0 = normal. Persisted so it survives restarts.
    #[serde(default = "default_playback_rate")]
    pub playback_rate: f32,
    /// When true, speed changes also shift pitch. When false, pitch is preserved.
    #[serde(default = "default_pitch_enabled")]
    pub pitch_enabled: bool,
    /// Custom folder for yt-dlp downloads. Falls back to app_data/downloads.
    #[serde(default)]
    pub download_folder: Option<String>,
    /// Playlist ID to auto-add downloaded tracks. Falls back to "Downloads" playlist.
    #[serde(default)]
    pub download_playlist_id: Option<String>,
    /// Show the current track in Discord Rich Presence.
    #[serde(default = "default_discord_rpc_enabled")]
    pub discord_rpc_enabled: bool,
    /// Last main window position and size.
    #[serde(default)]
    pub window_state: Option<WindowState>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            equalizer: EqualizerSettings::default(),
            custom_presets: Vec::new(),
            playback_rate: default_playback_rate(),
            pitch_enabled: default_pitch_enabled(),
            download_folder: None,
            download_playlist_id: None,
            discord_rpc_enabled: default_discord_rpc_enabled(),
            window_state: None,
        }
    }
}

fn default_playback_rate() -> f32 {
    1.0
}

fn default_pitch_enabled() -> bool {
    true
}

fn default_discord_rpc_enabled() -> bool {
    true
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

fn write_file_atomic(path: &PathBuf, contents: &[u8]) -> Result<(), String> {
    let tmp_path = path.with_extension("json.tmp");
    let bak_path = path.with_extension("json.bak");

    let write_result = (|| {
        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| format!("Failed to create temporary settings file: {}", e))?;
        use std::io::Write as _;
        file.write_all(contents)
            .map_err(|e| format!("Failed to write temporary settings file: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to flush temporary settings file: {}", e))?;
        drop(file);

        match fs::rename(&tmp_path, path) {
            Ok(()) => Ok(()),
            Err(first_error) if path.exists() => {
                let _ = fs::remove_file(&bak_path);
                fs::rename(path, &bak_path)
                    .map_err(|e| format!("Failed to back up settings file before replace: {}", e))?;

                match fs::rename(&tmp_path, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&bak_path);
                        Ok(())
                    }
                    Err(second_error) => {
                        let _ = fs::rename(&bak_path, path);
                        Err(format!(
                            "Failed to replace settings file: {}; original rename error: {}",
                            second_error, first_error
                        ))
                    }
                }
            }
            Err(error) => Err(format!("Failed to replace settings file: {}", error)),
        }
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

pub fn save_settings(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    write_file_atomic(&path, &json)
}