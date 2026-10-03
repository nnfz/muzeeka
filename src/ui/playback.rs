//! BASS output for the GPUI window.
//!
//! The effect rack is the one saved by the installable app in
//! `%APPDATA%\com.nnfz.muzeeka\settings.json`. Playback position is not written
//! back into `library.db`, so a running Tauri window keeps its own resume point.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use muzeeka::audio::dsp_chain::{ChainSlotSettings, EffectSettings, MAX_SLOTS};
use muzeeka::audio::equalizer::EqualizerSettings;
use muzeeka::audio::events::EventSink;
use muzeeka::audio::limiter::LimiterSettings;
use muzeeka::audio::player::{GaplessTrack, PlaybackState, Player, PlayerStateSnapshot};

use crate::ui::catalog;

pub struct Engine {
    player: Player,
    /// Set when the saved rack could not be applied. Playback still starts.
    pub warning: Option<String>,
}

/// The audio page of `settings.json`: the live rack, speed, and saved presets.
///
/// `persist` is false for the preview session and when the file could not be
/// parsed, so a test or a bad read cannot overwrite the installable app's copy.
#[derive(Clone)]
pub struct AudioRack {
    pub chain: Vec<ChainSlotSettings>,
    pub playback_rate: f32,
    pub pitch_enabled: bool,
    pub eq_presets: Vec<EqPreset>,
    pub filter_presets: Vec<FilterPreset>,
    pub limiter_presets: Vec<LimiterPreset>,
    pub chain_presets: Vec<ChainPreset>,
    pub persist: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqPreset {
    pub name: String,
    pub preamp_db: f32,
    #[serde(default)]
    pub bands_db: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterPreset {
    pub name: String,
    pub lp_hz: f32,
    pub hp_hz: f32,
    pub resonance: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimiterPreset {
    pub name: String,
    pub gain_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    #[serde(default)]
    pub clip: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainPreset {
    pub name: String,
    pub slots: Vec<ChainSlotSettings>,
}

#[derive(Deserialize)]
struct SavedAudioFile {
    #[serde(default)]
    dsp_chain: Option<Vec<ChainSlotSettings>>,
    #[serde(default)]
    equalizer: EqualizerSettings,
    #[serde(default)]
    limiter: LimiterSettings,
    #[serde(default = "default_playback_rate")]
    playback_rate: f32,
    #[serde(default = "default_pitch_enabled")]
    pitch_enabled: bool,
    #[serde(default)]
    custom_presets: Vec<EqPreset>,
    #[serde(default)]
    filter_presets: Vec<FilterPreset>,
    #[serde(default)]
    limiter_presets: Vec<LimiterPreset>,
    #[serde(default)]
    chain_presets: Vec<ChainPreset>,
}

fn default_playback_rate() -> f32 {
    1.0
}

fn default_pitch_enabled() -> bool {
    true
}

impl Engine {
    /// Opens BASS, then installs the saved rack before any file is played.
    pub fn attach(volume: f32, rack: &AudioRack) -> Result<Self, String> {
        let player = Player::new();
        player.set_bass_dir(bass_directory());
        player.set_ui_hot(true);
        let sink = EventSink::new();
        player.set_event_sink(sink.clone());
        player
            .init()
            .map_err(|error| format!("BASS init failed: {error}"))?;

        let mut warning = None;
        if let Err(error) = player.set_dsp_chain(rack.chain.clone()) {
            warning = Some(format!("DSP chain was not applied: {error}"));
        }
        if (rack.playback_rate - 1.0).abs() > 0.001 {
            if let Err(error) = player.set_playback_rate(rack.playback_rate) {
                warning = Some(format!("Playback rate was not applied: {error}"));
            }
        }
        if !rack.pitch_enabled {
            if let Err(error) = player.set_pitch_enabled(false) {
                warning = Some(format!("Pitch mode was not applied: {error}"));
            }
        }
        player
            .set_volume(volume.clamp(0.0, 1.0))
            .map_err(|error| format!("Volume was not applied: {error}"))?;
        player.start_position_emitter(sink);
        eprintln!(
            "[audio] BASS ready, {} DSP slot{}",
            rack.chain.len(),
            if rack.chain.len() == 1 { "" } else { "s" }
        );
        Ok(Self { player, warning })
    }

    pub fn apply_chain(&self, slots: Vec<ChainSlotSettings>) -> Result<(), String> {
        self.player.set_dsp_chain(slots)
    }

    pub fn set_rate(&self, rate: f32) -> Result<(), String> {
        self.player.set_playback_rate(rate)
    }

    pub fn set_pitch(&self, enabled: bool) -> Result<(), String> {
        self.player.set_pitch_enabled(enabled)
    }

    /// Limiter gain-reduction meters. Lock-free on the player, so the settings
    /// window can poll this without waiting on a file open.
    pub fn limiter_readings(&self) -> Vec<(String, f32)> {
        self.player
            .get_dsp_chain_status()
            .slots
            .into_iter()
            .map(|slot| (slot.id, slot.meter_db))
            .collect()
    }

    pub fn play(&self, path: &str, queue: Vec<GaplessTrack>) -> Result<(), String> {
        self.player.play(path, None, None, None, queue)
    }

    pub fn pause(&self) -> Result<(), String> {
        self.player.pause()
    }

    pub fn resume(&self) -> Result<(), String> {
        self.player.resume()
    }

    pub fn seek(&self, seconds: f64) -> Result<(), String> {
        self.player.seek(seconds)
    }

    pub fn set_volume(&self, volume: f32) -> Result<(), String> {
        self.player.set_volume(volume)
    }

    pub fn prepare(&self, current: &str, queue: Vec<GaplessTrack>) {
        let _ = self.player.prepare_next(Some(current), queue);
    }

    pub fn snapshot(&self) -> PlayerStateSnapshot {
        self.player.get_state()
    }

    /// True while this path is the open stream, including a pause fade.
    pub fn holds(&self, path: &str) -> bool {
        let snap = self.snapshot();
        matches!(
            snap.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Stalled
        ) && snap
            .current_file
            .as_deref()
            .is_some_and(|open| same_audio_path(open, path))
    }
}

pub fn gapless_track(path: &str) -> GaplessTrack {
    let audio = match path.rfind("#cue:") {
        Some(index) if index > 0 => path[..index].to_string(),
        _ => path.to_string(),
    };
    GaplessTrack {
        track_path: path.to_string(),
        audio_path: audio,
        cue_start: None,
        cue_end: None,
    }
}

pub fn same_audio_path(left: &str, right: &str) -> bool {
    fn norm(path: &str) -> String {
        path.trim()
            .trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_lowercase()
    }
    norm(left) == norm(right)
}

fn bass_directory() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("bass"));
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("src").join("bin").join("bass"));
    candidates.push(manifest.join("bass"));
    candidates
        .into_iter()
        .find(|dir| dir.join("bass.dll").is_file())
        .unwrap_or_else(|| manifest.join("src").join("bin").join("bass"))
}

impl AudioRack {
    /// Empty rack used by tests and by the window before a file is read.
    /// Nothing is written back.
    pub fn flat() -> Self {
        Self {
            chain: Vec::new(),
            playback_rate: 1.0,
            pitch_enabled: true,
            eq_presets: Vec::new(),
            filter_presets: Vec::new(),
            limiter_presets: Vec::new(),
            chain_presets: Vec::new(),
            persist: false,
            warning: None,
        }
    }

    /// Reads the installable app's settings. A missing file is an empty rack
    /// that may be saved later. A file that does not parse is not saved over.
    pub fn load() -> Self {
        let path = catalog::app_data_dir().join("settings.json");
        let backup = path.with_extension("json.bak");
        let mut saw_file = false;
        let mut last_error = None;
        for candidate in [&path, &backup] {
            if !candidate.is_file() {
                continue;
            }
            saw_file = true;
            match std::fs::read_to_string(candidate) {
                Ok(raw) => match parse_saved_audio(&raw) {
                    Ok(rack) => return rack,
                    Err(error) => {
                        last_error = Some(error);
                        eprintln!("[audio] could not read {}: {last_error:?}", candidate.display());
                    }
                },
                Err(error) => {
                    last_error = Some(error.to_string());
                    eprintln!("[audio] could not read {}: {error}", candidate.display());
                }
            }
        }
        let mut rack = Self::flat();
        if !saw_file {
            rack.persist = true;
        } else if let Some(error) = last_error {
            rack.warning = Some(format!(
                "Audio settings could not be read ({error}). Changes here will not be saved."
            ));
        }
        rack
    }

    /// Patches the audio keys in `settings.json` and leaves every other key.
    pub fn save(&self) {
        if !self.persist {
            return;
        }
        let path = catalog::app_data_dir().join("settings.json");
        let current = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
        let raw = if current.trim().is_empty() {
            "{}"
        } else {
            current.as_str()
        };
        let merged = match merge_audio_json(raw, self) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("[audio] could not update settings.json: {error}");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&path, merged) {
            eprintln!("[audio] could not write settings.json: {error}");
        }
    }
}

fn parse_saved_audio(raw: &str) -> Result<AudioRack, String> {
    let mut file: SavedAudioFile =
        serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let mut chain = match file.dsp_chain.take() {
        Some(chain) => chain,
        // A file from before the rack stored one EQ and one limiter.
        None => {
            let mut slots = Vec::new();
            if file.equalizer != EqualizerSettings::default() {
                slots.push(ChainSlotSettings {
                    id: "legacy-equalizer".into(),
                    enabled: file.equalizer.enabled,
                    effect: EffectSettings::Equalizer(file.equalizer.clone()),
                });
            }
            if file.limiter != LimiterSettings::default() {
                slots.push(ChainSlotSettings {
                    id: "legacy-limiter".into(),
                    enabled: file.limiter.enabled,
                    effect: EffectSettings::Limiter(file.limiter.clone()),
                });
            }
            slots
        }
    };
    chain.truncate(MAX_SLOTS);
    for (index, slot) in chain.iter_mut().enumerate() {
        slot.effect = slot.effect.clone().clamp();
        if slot.id.trim().is_empty() {
            slot.id = format!("slot-{index}");
        }
    }
    let playback_rate = if file.playback_rate.is_finite() && file.playback_rate > 0.0 {
        file.playback_rate.clamp(0.25, 2.0)
    } else {
        1.0
    };
    Ok(AudioRack {
        chain,
        playback_rate,
        pitch_enabled: file.pitch_enabled,
        eq_presets: file.custom_presets,
        filter_presets: file.filter_presets,
        limiter_presets: file.limiter_presets,
        chain_presets: file.chain_presets,
        persist: true,
        warning: None,
    })
}

/// Rewrites only the audio keys. Other settings (downloads, Discord, plugins)
/// stay as they were stored.
pub(crate) fn merge_audio_json(raw: &str, rack: &AudioRack) -> Result<String, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "settings.json is not an object".to_string())?;
    object.insert(
        "dsp_chain".into(),
        serde_json::to_value(&rack.chain).map_err(|error| error.to_string())?,
    );
    object.insert(
        "playback_rate".into(),
        serde_json::json!(rack.playback_rate),
    );
    object.insert(
        "pitch_enabled".into(),
        serde_json::json!(rack.pitch_enabled),
    );
    object.insert(
        "custom_presets".into(),
        serde_json::to_value(&rack.eq_presets).map_err(|error| error.to_string())?,
    );
    object.insert(
        "filter_presets".into(),
        serde_json::to_value(&rack.filter_presets).map_err(|error| error.to_string())?,
    );
    object.insert(
        "limiter_presets".into(),
        serde_json::to_value(&rack.limiter_presets).map_err(|error| error.to_string())?,
    );
    object.insert(
        "chain_presets".into(),
        serde_json::to_value(&rack.chain_presets).map_err(|error| error.to_string())?,
    );
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_virtual_path_keeps_the_audio_file() {
        let track = gapless_track(r"D:\music\album.flac#cue:2");
        assert_eq!(track.track_path, r"D:\music\album.flac#cue:2");
        assert_eq!(track.audio_path, r"D:\music\album.flac");
        assert!(track.cue_start.is_none());
    }

    #[test]
    fn saved_rack_round_trips() {
        let raw = r#"{
            "dsp_chain": [{
                "id": "equalizer-1",
                "enabled": true,
                "kind": "equalizer",
                "settings": {
                    "enabled": true,
                    "preamp_db": -0.3,
                    "bands_db": [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
                }
            }],
            "playback_rate": 1.0,
            "pitch_enabled": true
        }"#;
        let saved = parse_saved_audio(raw).unwrap();
        assert_eq!(saved.chain.len(), 1);
        assert_eq!(saved.chain[0].id, "equalizer-1");
        assert!(saved.chain[0].enabled);
    }

    #[test]
    fn installed_settings_rack_parses_when_present() {
        let path = catalog::app_data_dir().join("settings.json");
        if !path.is_file() {
            return;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let saved = parse_saved_audio(&raw).expect("settings.json dsp chain");
        assert!(saved.chain.len() <= MAX_SLOTS);
        assert!(saved.playback_rate.is_finite());
    }

    #[test]
    fn save_keeps_unrelated_keys() {
        let raw = r#"{"discord_rpc_enabled":false,"download_folder":"D:\\in","dsp_chain":[],"playback_rate":1.0}"#;
        let mut rack = AudioRack::flat();
        rack.playback_rate = 1.25;
        rack.pitch_enabled = false;
        let out = merge_audio_json(raw, &rack).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["discord_rpc_enabled"], false);
        assert_eq!(value["download_folder"], "D:\\in");
        assert_eq!(value["playback_rate"], 1.25);
        assert_eq!(value["pitch_enabled"], false);
        assert!(value["dsp_chain"].is_array());
    }
}
