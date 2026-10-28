use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use serde::{Deserialize, Serialize};

use muzeeka::audio::dsp_chain::{ChainSlotSettings, EffectSettings, MAX_SLOTS};
use muzeeka::audio::equalizer::EqualizerSettings;
use muzeeka::audio::events::EventSink;
use muzeeka::audio::limiter::LimiterSettings;
use muzeeka::audio::player::{GaplessTrack, PlaybackState, Player, PlayerStateSnapshot};

use crate::ui::catalog;

pub struct Engine {
    player: Player,
    pub warning: Option<String>,
    fallback_rate: Arc<AtomicU32>,
}

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
    pub fn attach(volume: f32, rack: &AudioRack) -> Result<Self, String> {
        let player = Player::new();
        let bass_dir = bass_directory();
        pin_dll_directory(&bass_dir);
        player.set_bass_dir(bass_dir);
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
        let fallback_rate = Arc::new(AtomicU32::new(rack.playback_rate.to_bits()));
        install_library_hook(&player, Arc::clone(&fallback_rate));
        eprintln!(
            "[audio] BASS ready, {} DSP slot{}",
            rack.chain.len(),
            if rack.chain.len() == 1 { "" } else { "s" }
        );
        Ok(Self {
            player,
            warning,
            fallback_rate,
        })
    }

    pub fn apply_chain(&self, slots: Vec<ChainSlotSettings>) -> Result<(), String> {
        self.player.set_dsp_chain(slots)
    }

    pub fn set_rate(&self, rate: f32) -> Result<(), String> {
        self.player.set_playback_rate(rate)
    }

    pub fn set_rate_immediate(&self, rate: f32) -> Result<(), String> {
        self.player.set_playback_rate_immediate(rate)
    }

    pub fn set_fallback_rate(&self, rate: f32) {
        let rate = if rate.is_finite() {
            rate.clamp(0.25, 2.0)
        } else {
            1.0
        };
        self.fallback_rate.store(rate.to_bits(), Ordering::Relaxed);
    }

    pub fn current_path(&self) -> Option<String> {
        self.player.get_state().current_file
    }

    pub fn clone_player(&self) -> Player {
        self.player.clone()
    }

    pub fn detect_bpm(&self, path: &str) -> Result<f32, String> {
        muzeeka::audio::bpm::detect_bpm_for_path(&self.player, path)
    }

    pub fn set_pitch(&self, enabled: bool) -> Result<(), String> {
        self.player.set_pitch_enabled(enabled)
    }

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

    pub fn output_devices(&self) -> Result<Vec<muzeeka::audio::player::OutputDevice>, String> {
        self.player.list_output_devices()
    }

    pub fn add_output(
        &self,
        device_id: i32,
        volume: f32,
    ) -> Result<muzeeka::audio::player::ExtraOutputInfo, String> {
        self.player.add_extra_output(device_id, volume)
    }

    pub fn remove_output(&self, id: &str) -> Result<(), String> {
        self.player.remove_extra_output(id)
    }

    pub fn set_output_volume(
        &self,
        id: &str,
        volume: f32,
    ) -> Result<muzeeka::audio::player::ExtraOutputInfo, String> {
        self.player.set_extra_output_volume(id, volume)
    }

    pub fn outputs(&self) -> Result<Vec<muzeeka::audio::player::ExtraOutputInfo>, String> {
        self.player.extra_outputs()
    }

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
    let candidates = bass_candidates();
    pick_bass_dir(&candidates).unwrap_or_else(|| candidates.into_iter().next().unwrap_or_default())
}

fn bass_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("bin").join("bass"));
            candidates.push(parent.join("bass"));
            candidates.push(parent.to_path_buf());
        }
    }
    if cfg!(debug_assertions) {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        candidates.push(manifest.join("src").join("bin").join("bass"));
        candidates.push(manifest.join("bass"));
    }
    candidates
}

fn pick_bass_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|dir| dir.join("bass.dll").is_file())
        .cloned()
}

fn pin_dll_directory(dir: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn SetDllDirectoryW(path: *const u16) -> i32;
        }
        let wide: Vec<u16> = dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            SetDllDirectoryW(wide.as_ptr());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
    }
}

impl AudioRack {
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
                        eprintln!(
                            "[audio] could not read {}: {last_error:?}",
                            candidate.display()
                        );
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
    let mut file: SavedAudioFile = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let mut chain = match file.dsp_chain.take() {
        Some(chain) => chain,
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

fn install_library_hook(player: &Player, fallback: Arc<AtomicU32>) {
    let player_for_hook = player.clone();
    player.set_play_hook(Some(Arc::new(move |path| {
        on_track_started(&player_for_hook, &fallback, path);
    })));
}

fn on_track_started(player: &Player, fallback: &AtomicU32, path: &str) {
    let Ok(db) = catalog::open_library() else {
        return;
    };
    let _ = catalog::record_play(&db, path);
    let rate = catalog::track_playback_rate(&db, path)
        .ok()
        .flatten()
        .unwrap_or_else(|| f32::from_bits(fallback.load(Ordering::Relaxed)));
    let _ = player.set_playback_rate_immediate(rate);
    let path = path.to_string();
    thread::spawn(move || maybe_auto_video(&path));
}

fn maybe_auto_video(path: &str) {
    if path.trim().is_empty() || !crate::ui::ytdlp::auto_video_bg_enabled() {
        return;
    }
    let Ok(db) = catalog::open_library() else {
        return;
    };
    if catalog::track_video_bg(&db, path).ok().flatten().is_some() {
        return;
    }
    if catalog::track_video_bg_tried(&db, path).unwrap_or(true) {
        return;
    }
    let Some((title, artist)) = catalog::labels_for_playback_path(&db, path) else {
        return;
    };
    if title.trim().is_empty() || artist.trim().is_empty() {
        return;
    }
    if catalog::set_track_video_bg_tried(&db, path).is_err() {
        return;
    }
    let output = crate::ui::ytdlp::video_bg_output(path);
    if crate::ui::ytdlp::download_video_bg_clip(&artist, &title, &output).is_err() {
        return;
    }
    let _ = catalog::set_track_video_bg(&db, path, Some(&output.to_string_lossy()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bass_in_bin_beats_a_copy_beside_the_executable() {
        let root = std::env::temp_dir().join(format!("muzeeka-bass-{}", std::process::id()));
        let beside = root.join("app");
        let nested = beside.join("bin").join("bass");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(beside.join("bass.dll"), b"").unwrap();
        std::fs::write(nested.join("bass.dll"), b"").unwrap();
        let picked = pick_bass_dir(&[nested.clone(), beside.clone()]).unwrap();
        assert_eq!(picked, nested);
        let _ = std::fs::remove_dir_all(&root);
    }

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
