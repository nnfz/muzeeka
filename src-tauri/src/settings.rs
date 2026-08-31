use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::dsp_chain::{ChainSlotSettings, EffectSettings};
use crate::equalizer::EqualizerSettings;
use crate::limiter::LimiterSettings;

/// EQ preset (legacy field name `custom_presets` in settings.json).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CustomPreset {
    pub name: String,
    pub preamp_db: f32,
    #[serde(default)]
    pub bands_db: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FilterPreset {
    pub name: String,
    pub lp_hz: f32,
    pub hp_hz: f32,
    pub resonance: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LimiterPreset {
    pub name: String,
    pub gain_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    #[serde(default)]
    pub clip: bool,
}

/// Full rack snapshot — ordered slots with bypass + settings (ids reminted on apply).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChainPreset {
    pub name: String,
    #[serde(default)]
    pub slots: Vec<ChainSlotSettings>,
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
    /// The effect rack: ordered slots, any effect any number of times.
    ///
    /// `None` means "written by a version that predates the rack" — see
    /// `migrate_dsp_chain`, which folds the legacy fields below into a chain once.
    /// After that the chain is the only source of truth for DSP.
    #[serde(default)]
    pub dsp_chain: Option<Vec<ChainSlotSettings>>,
    /// Legacy single equalizer. Kept only so an old settings.json still migrates;
    /// nothing reads it once `dsp_chain` exists.
    #[serde(default)]
    pub equalizer: EqualizerSettings,
    /// Legacy single limiter. Same story as `equalizer`.
    #[serde(default)]
    pub limiter: LimiterSettings,
    #[serde(default)]
    pub custom_presets: Vec<CustomPreset>,
    /// User-saved filter presets (no factory defaults).
    #[serde(default)]
    pub filter_presets: Vec<FilterPreset>,
    /// User-saved limiter presets (no factory defaults).
    #[serde(default)]
    pub limiter_presets: Vec<LimiterPreset>,
    /// User-saved full DSP chain presets (no factory defaults).
    #[serde(default)]
    pub chain_presets: Vec<ChainPreset>,
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
    /// Old settings.json key. Migrated into the `muzeeka.remote` plugin once.
    #[serde(rename = "remote_enabled", default = "default_legacy_remote_enabled", skip_serializing)]
    pub legacy_remote_enabled: bool,
    /// Old settings.json key. Migrated into the `muzeeka.remote` plugin once.
    #[serde(rename = "remote_port", default = "default_legacy_remote_port", skip_serializing)]
    pub legacy_remote_port: u16,
    /// Shuffle algorithm: normal random vs smart no-repeat-until-exhausted.
    #[serde(default)]
    pub shuffle_mode: ShuffleMode,
    /// Show the Development settings tab and in-app console.
    #[serde(default)]
    pub developer_mode: bool,
    /// Last main window position and size.
    #[serde(default)]
    pub window_state: Option<WindowState>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            // A fresh profile starts with an empty rack. `Some(vec![])` (not `None`)
            // marks it as "the chain is the source of truth", so `migrate_dsp_chain`
            // — which only fires for files that predate the rack — leaves it alone.
            dsp_chain: Some(Vec::new()),
            equalizer: EqualizerSettings::default(),
            limiter: LimiterSettings::default(),
            custom_presets: Vec::new(),
            filter_presets: Vec::new(),
            limiter_presets: Vec::new(),
            chain_presets: Vec::new(),
            playback_rate: default_playback_rate(),
            pitch_enabled: default_pitch_enabled(),
            download_folder: None,
            download_playlist_id: None,
            discord_rpc_enabled: default_discord_rpc_enabled(),
            legacy_remote_enabled: default_legacy_remote_enabled(),
            legacy_remote_port: default_legacy_remote_port(),
            shuffle_mode: ShuffleMode::default(),
            developer_mode: false,
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

fn default_legacy_remote_enabled() -> bool {
    true
}

fn default_legacy_remote_port() -> u16 {
    crate::plugins::http_server::DEFAULT_HTTP_PORT
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

/// Fold a pre-rack settings file into a chain: EQ then limiter, which is the
/// order the old fixed priorities produced. Runs once — after this the file has a
/// `dsp_chain` and the legacy fields are never consulted again.
///
/// Each slot keeps the effect's own `enabled`, so a user who had the EQ on and
/// the limiter off gets exactly that back, just as two rack rows.
///
/// An effect the user never touched contributes no slot: carrying a flat, disabled
/// EQ forward would mean every fresh install opens on a rack of dead rows. A new
/// profile therefore starts empty — see `AppSettings::default`.
fn migrate_dsp_chain(settings: &mut AppSettings) {
    if settings.dsp_chain.is_some() {
        return;
    }
    let mut slots = Vec::new();
    if settings.equalizer != EqualizerSettings::default() {
        slots.push(ChainSlotSettings {
            id: "legacy-equalizer".into(),
            enabled: settings.equalizer.enabled,
            effect: EffectSettings::Equalizer(settings.equalizer.clone()),
        });
    }
    if settings.limiter != LimiterSettings::default() {
        slots.push(ChainSlotSettings {
            id: "legacy-limiter".into(),
            enabled: settings.limiter.enabled,
            effect: EffectSettings::Limiter(settings.limiter.clone()),
        });
    }
    settings.dsp_chain = Some(slots);
}

/// Clamp fields that can be hand-edited or come from older app versions.
fn normalize_settings(mut settings: AppSettings) -> AppSettings {
    settings.equalizer = settings.equalizer.clamp();
    settings.limiter = settings.limiter.clone().clamp();
    migrate_dsp_chain(&mut settings);
    if let Some(chain) = settings.dsp_chain.as_mut() {
        chain.truncate(crate::dsp_chain::MAX_SLOTS);
        let mut seen = std::collections::HashSet::new();
        for (i, slot) in chain.iter_mut().enumerate() {
            slot.effect = slot.effect.clone().clamp();
            // Ids are identity in the rack: duplicates would make two rows fight
            // over one node, so a hand-edited file gets its collisions renamed.
            if slot.id.trim().is_empty() || !seen.insert(slot.id.clone()) {
                slot.id = format!("slot-{i}");
                seen.insert(slot.id.clone());
            }
        }
    }
    settings.playback_rate = settings.playback_rate.clamp(0.25, 2.0);
    settings.legacy_remote_port =
        crate::plugins::http_server::sanitize_port(settings.legacy_remote_port);
    for preset in &mut settings.custom_presets {
        preset.preamp_db = preset.preamp_db.clamp(-15.0, 15.0);
        for gain in &mut preset.bands_db {
            *gain = gain.clamp(-20.0, 20.0);
        }
    }
    for preset in &mut settings.filter_presets {
        preset.lp_hz = preset.lp_hz.clamp(20.0, 20_000.0);
        preset.hp_hz = preset.hp_hz.clamp(20.0, 20_000.0);
        preset.resonance = preset.resonance.clamp(0.5, 8.0);
    }
    for preset in &mut settings.limiter_presets {
        preset.gain_db = preset.gain_db.clamp(0.0, 12.0);
        preset.ceiling_db = preset.ceiling_db.clamp(-6.0, 0.0);
        preset.release_ms = preset.release_ms.clamp(10.0, 1000.0);
    }
    for preset in &mut settings.chain_presets {
        preset.slots.truncate(crate::dsp_chain::MAX_SLOTS);
        for (i, slot) in preset.slots.iter_mut().enumerate() {
            slot.effect = slot.effect.clone().clamp();
            if slot.id.trim().is_empty() {
                slot.id = format!("preset-slot-{i}");
            }
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
#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_json(body: &str) -> AppSettings {
        serde_json::from_str(body).expect("fixture parses")
    }

    #[test]
    fn a_fresh_profile_starts_with_an_empty_rack() {
        // Not a migration: nothing to carry over, so the user opens Settings on a
        // clean slate rather than on two dead rows.
        let settings = normalize_settings(AppSettings::default());
        assert!(settings.dsp_chain.expect("chain").is_empty());
    }

    #[test]
    fn an_untouched_legacy_file_migrates_to_nothing() {
        // Old file, DSP never used. Carrying a flat disabled EQ forward would put
        // clutter in the rack that the user never asked for.
        let settings = normalize_settings(legacy_json(r#"{"playback_rate": 1.0}"#));
        assert!(settings.dsp_chain.expect("chain").is_empty());
    }

    #[test]
    fn a_configured_legacy_eq_becomes_one_slot() {
        let settings = normalize_settings(legacy_json(
            r#"{"equalizer": {"enabled": true, "preamp_db": -3.0, "bands_db": [4.0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}}"#,
        ));
        let chain = settings.dsp_chain.expect("chain");
        assert_eq!(chain.len(), 1, "the untouched limiter must not add a slot");
        assert_eq!(chain[0].id, "legacy-equalizer");
        assert!(chain[0].enabled);
        match &chain[0].effect {
            EffectSettings::Equalizer(eq) => {
                assert_eq!(eq.preamp_db, -3.0);
                assert_eq!(eq.bands_db[0], 4.0);
            }
            other => panic!("expected an equalizer, got {other:?}"),
        }
    }

    #[test]
    fn a_disabled_but_edited_legacy_effect_still_migrates() {
        // The user tuned the limiter and then switched it off. The tuning is worth
        // keeping — it comes back as a bypassed row, not as a deletion.
        let settings = normalize_settings(legacy_json(
            r#"{"limiter": {"enabled": false, "gain_db": 6.0, "ceiling_db": -1.0, "release_ms": 200.0, "clip": true}}"#,
        ));
        let chain = settings.dsp_chain.expect("chain");
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, "legacy-limiter");
        assert!(!chain[0].enabled, "row should come back bypassed");
    }

    #[test]
    fn both_configured_legacy_effects_keep_eq_before_limiter() {
        // The order the old fixed BASS priorities produced.
        let settings = normalize_settings(legacy_json(
            r#"{"equalizer": {"enabled": true, "preamp_db": 2.0, "bands_db": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]},
                "limiter": {"enabled": true, "gain_db": 3.0, "ceiling_db": -0.3, "release_ms": 120.0, "clip": false}}"#,
        ));
        let chain = settings.dsp_chain.expect("chain");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, "legacy-equalizer");
        assert_eq!(chain[1].id, "legacy-limiter");
    }

    #[test]
    fn an_existing_rack_is_never_re_migrated() {
        // `dsp_chain` present — even empty — means the user already lives in the
        // rack world. Re-running the migration would resurrect deleted effects.
        let settings = normalize_settings(legacy_json(
            r#"{"dsp_chain": [], "equalizer": {"enabled": true, "preamp_db": 5.0, "bands_db": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}}"#,
        ));
        assert!(settings.dsp_chain.expect("chain").is_empty());
    }
}
