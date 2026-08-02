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

/// How shuffle picks the next track.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ShuffleMode {
    /// Classic random order of the full playlist (can reshuffle freely).
    Normal,
    /// Avoid tracks already heard in this playlist until every track has played once.
    #[default]
    Smart,
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
    /// Local phone/browser remote control HTTP server.
    #[serde(default = "default_remote_enabled")]
    pub remote_enabled: bool,
    /// Port for the remote control server (default 8765).
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,
    /// Shuffle algorithm: normal random vs smart no-repeat-until-exhausted.
    #[serde(default)]
    pub shuffle_mode: ShuffleMode,
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
            remote_enabled: default_remote_enabled(),
            remote_port: default_remote_port(),
            shuffle_mode: ShuffleMode::default(),
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

fn default_remote_enabled() -> bool {
    true
}

fn default_remote_port() -> u16 {
    crate::remote_server::DEFAULT_REMOTE_PORT
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

fn settings_bak_path(primary: &std::path::Path) -> PathBuf {
    primary.with_extension("json.bak")
}

/// Clamp fields that can be hand-edited or come from older app versions.
fn normalize_settings(mut settings: AppSettings) -> AppSettings {
    settings.equalizer = settings.equalizer.clamp();
    settings.playback_rate = settings.playback_rate.clamp(0.25, 2.0);
    settings.remote_port = crate::remote_server::sanitize_port(settings.remote_port);
    for preset in &mut settings.custom_presets {
        preset.preamp_db = preset.preamp_db.clamp(-15.0, 15.0);
        for gain in &mut preset.bands_db {
            *gain = gain.clamp(-20.0, 20.0);
        }
    }
    settings
}

fn parse_settings_file(path: &std::path::Path) -> Result<AppSettings, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

pub fn load_settings(app: &AppHandle) -> Result<AppSettings, String> {
    let path = settings_path(app)?;
    let bak_path = settings_bak_path(&path);

    if !path.exists() {
        // Primary missing — still try backup (e.g. crash mid-replace left only .bak).
        if bak_path.is_file() {
            match parse_settings_file(&bak_path) {
                Ok(settings) => {
                    eprintln!(
                        "[settings] settings.json missing; restored from {}",
                        bak_path.display()
                    );
                    let settings = normalize_settings(settings);
                    // Heal primary so the next launch does not depend on .bak alone.
                    let _ = save_settings(app, &settings);
                    return Ok(settings);
                }
                Err(error) => {
                    eprintln!("[settings] backup also unreadable: {error}");
                }
            }
        }
        return Ok(AppSettings::default());
    }

    match parse_settings_file(&path) {
        Ok(settings) => Ok(normalize_settings(settings)),
        Err(primary_error) => {
            if bak_path.is_file() {
                match parse_settings_file(&bak_path) {
                    Ok(settings) => {
                        eprintln!(
                            "[settings] {primary_error}; restored from {}",
                            bak_path.display()
                        );
                        let settings = normalize_settings(settings);
                        let _ = save_settings(app, &settings);
                        return Ok(settings);
                    }
                    Err(bak_error) => {
                        eprintln!(
                            "[settings] primary and backup both failed ({primary_error}; {bak_error}); using defaults"
                        );
                    }
                }
            } else {
                eprintln!(
                    "[settings] {primary_error}; no .bak available, using defaults"
                );
            }
            Ok(AppSettings::default())
        }
    }
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