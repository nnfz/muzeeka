// Player state management
//
// Wraps BASS in a higher-level API that tracks the current track, volume,
// playback state, and emits Tauri events for position updates.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::bass::{self, BassLibrary};
use crate::cue::{self, PlaybackTarget};
use crate::discord_rpc::DiscordPresence;
use crate::equalizer::{eq_dsp_callback, EqDspContext, EqualizerSettings};

/// Next track queued for gapless transition.
#[derive(Debug, Clone)]
pub struct GaplessTrack {
    pub track_path: String,
    pub audio_path: String,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackChangedPayload {
    pub path: String,
}

// Spotify-like short musical fades (not on track changes)
const PAUSE_FADE_MS: u32 = 220;
const RESUME_FADE_MS: u32 = 180;
const PLAYBACK_RATE_RAMP_MS: u32 = 600;
const RATE_RAMP_STEPS: u32 = 30;

// For seek: we want it instant.
// Very short hard mute only during the flush to hide any restart artifact.
const SEEK_DIP_LEVEL: f32 = 0.0;

// Gapless: switch at the real segment boundary (tight tolerance avoids early cuts).
const GAPLESS_END_EPSILON_SECS: f64 = 0.008;
const POLL_INTERVAL_MS: u64 = 50;

// ── Playback state enum ───────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
    Stalled,
}

impl From<u32> for PlaybackState {
    fn from(v: u32) -> Self {
        match v {
            bass::BASS_ACTIVE_PLAYING => Self::Playing,
            bass::BASS_ACTIVE_PAUSED | bass::BASS_ACTIVE_PAUSED_DEVICE => Self::Paused,
            bass::BASS_ACTIVE_STALLED => Self::Stalled,
            _ => Self::Stopped,
        }
    }
}

// ── State snapshot (sent to frontend) ─────────────────────────────────────────
#[derive(Debug, Clone, Serialize)]
pub struct PlayerStateSnapshot {
    pub state: PlaybackState,
    pub is_playing: bool,
    pub is_paused: bool,
    pub volume: f32,
    pub position: f64,
    pub duration: f64,
    pub current_file: Option<String>,
    pub current_file_name: Option<String>,
}

// ── Equalizer diagnostics ─────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize)]
pub struct EqualizerStatus {
    pub settings: EqualizerSettings,
    pub dsp_attached: bool,
    pub process_count: u64,
}

// ── Position event payload ────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize)]
pub struct PositionPayload {
    pub position: f64,
    pub duration: f64,
    pub state: PlaybackState,
}

// ── Inner mutable state ───────────────────────────────────────────────────────
struct PlayerInner {
    bass: Option<BassLibrary>,
    bass_dir: PathBuf,
    /// The mixer stream (output). We play/pause this. DSP/EQ attached here.
    mixer_handle: u32,
    /// The current decode source plugged into the mixer (for the active track).
    current_source: u32,
    dsp_handle: u32,
    current_file: Option<String>,
    /// Resolved on-disk audio path for the active source (gapless detection).
    current_audio_path: Option<String>,
    volume: f32,
    playback_rate: f32,
    /// Rate currently applied to the active decode/tempo channel (may lag during ramps).
    applied_playback_rate: f32,
    /// Bumped to cancel an in-flight playback-rate ramp.
    rate_ramp_generation: u64,
    /// When true, speed changes also shift pitch (BASS_ATTRIB_FREQ). When false, tempo FX preserves pitch.
    pitch_enabled: bool,
    /// Raw decode handle when `current_source` is a tempo wrapper (otherwise 0).
    current_decode: u32,
    preloaded_decode: u32,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
    eq_context: &'static EqDspContext,
    /// Handles returned by BASS_PluginLoad — keep plugins registered.
    _plugin_handles: Vec<u32>,
    /// Full play-order queue; index points at the track currently playing.
    gapless_queue: Vec<GaplessTrack>,
    gapless_queue_index: usize,
    pending_next: Option<GaplessTrack>,
    preloaded_source: u32,
    preloaded_audio_path: Option<String>,
    /// Used to invalidate stale scheduled pause actions when user quickly plays new track.
    pause_generation: u64,
    /// Timestamp (millis since UNIX epoch) when current track play started (for manual plays).
    /// Prevents spurious early gapless advance right after clicking a track in the current que.
    current_track_start_time: u64,
    /// Logical pause state. Set immediately on pause() so that get_state() and emitted
    /// events report paused even while the volume fade is still playing out.
    user_paused: bool,
    /// Ignore spurious track-end detections while rebuilding playback channels.
    suppress_gapless_until: u64,
}

/// Official BASS format plugins that are known to work reliably.
/// Third-party plugins (e.g. basszxtune.dll or other tracker/chiptune addons)
/// placed in the bass/ folder will also be auto-detected and attempted.
const BASS_FORMAT_PLUGINS: &[&str] = &[
    "bassflac.dll",
    "bassape.dll",
    "basswv.dll",
    "bassopus.dll",
    "basswma.dll",
    "bassalac.dll",
    "basshls.dll",
    "bassmidi.dll",
    "basscd.dll",
];

/// DLLs that should never be loaded via BASS_PluginLoad (they are for mixing, effects, output etc.).
const NON_FORMAT_BASS_DLLS: &[&str] = &[
    "bass.dll",
    "bassmix.dll",
    "bass_fx.dll",
    "bassfx.dll",
    "basswasapi.dll",
];

// ── Public player handle ──────────────────────────────────────────────────────
#[derive(Clone)]
pub struct Player {
    inner: Arc<Mutex<PlayerInner>>,
    app: Arc<RwLock<Option<AppHandle>>>,
    bass_thread: Arc<RwLock<Option<thread::ThreadId>>>,
    discord: Arc<RwLock<Option<DiscordPresence>>>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlayerInner {
                bass: None,
                bass_dir: PathBuf::new(),
                mixer_handle: 0,
                current_source: 0,
                dsp_handle: 0,
                current_file: None,
                current_audio_path: None,
                volume: 1.0,
                playback_rate: 1.0,
                applied_playback_rate: 1.0,
                rate_ramp_generation: 0,
                pitch_enabled: true,
                current_decode: 0,
                preloaded_decode: 0,
                cue_start: None,
                cue_end: None,
                eq_context: Box::leak(Box::new(EqDspContext::new())),
                _plugin_handles: Vec::new(),
                gapless_queue: Vec::new(),
                gapless_queue_index: 0,
                pending_next: None,
                preloaded_source: 0,
                preloaded_audio_path: None,
                pause_generation: 0,
                current_track_start_time: 0,
                user_paused: false,
                suppress_gapless_until: 0,
            })),
            app: Arc::new(RwLock::new(None)),
            bass_thread: Arc::new(RwLock::new(None)),
            discord: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.write() = Some(app);
    }

    pub fn set_discord_presence(&self, discord: DiscordPresence) {
        *self.discord.write() = Some(discord);
    }

    fn sync_discord_presence(&self) {
        if let Some(discord) = self.discord.read().clone() {
            discord.update_from_player(&self.get_state());
        }
    }

    pub fn set_bass_dir(&self, bass_dir: PathBuf) {
        let mut inner = self.inner.lock();
        if inner.bass.is_none() {
            inner.bass_dir = bass_dir;
        }
    }

    fn on_bass_thread(&self) -> bool {
        self.bass_thread
            .read()
            .is_some_and(|id| id == thread::current().id())
    }

    fn run_on_bass_thread<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut PlayerInner) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        if self.on_bass_thread() {
            return f(&mut self.inner.lock());
        }

        let app = self
            .app
            .read()
            .clone()
            .ok_or("Player is not ready")?;
        let inner = Arc::clone(&self.inner);
        let (tx, rx) = mpsc::sync_channel(1);

        app.run_on_main_thread(move || {
            let mut guard = inner.lock();
            let _ = tx.send(f(&mut guard));
        })
        .map_err(|e| format!("Failed to dispatch to BASS thread: {e}"))?;

        rx.recv()
            .map_err(|_| "BASS thread did not respond".to_string())?
    }

    /// Initialize the BASS audio system. Must be called before any playback.
    pub fn init(&self) -> Result<(), String> {
        self.run_on_bass_thread(|inner| Self::init_inner(inner))
    }

    fn init_inner(inner: &mut PlayerInner) -> Result<(), String> {
        if inner.bass.is_some() {
            return Ok(());
        }

        if !inner.bass_dir.join("bass.dll").is_file() {
            return Err(format!(
                "BASS directory is invalid: {}",
                inner.bass_dir.display()
            ));
        }

        let mut bass = BassLibrary::load(&inner.bass_dir)?;

        // FLOATDSP must be configured before BASS_Init.
        let float_dsp_ok = bass.set_config(bass::BASS_CONFIG_FLOATDSP, 1.0).is_ok();

        match bass.init(-1, 44100) {
            Ok(()) => {}
            Err(e) => {
                if bass.last_error() != bass::BASS_ERROR_ALREADY {
                    return Err(e);
                }
            }
        }

        // 200ms buffer: 100ms faster than original 300ms for responsive switching,
        // while leaving 150ms margin over the 50ms gapless poll interval.
        let _ = bass.set_config(bass::BASS_CONFIG_BUFFER, 200.0);
        let _ = bass.set_config(bass::BASS_CONFIG_UPDATEPERIOD, 20.0);

        let fx_dll = inner.bass_dir.join("bass_fx.dll");
        if fx_dll.is_file() {
            let path_str = fx_dll.to_string_lossy().to_string();
            match bass.plugin_load(&path_str) {
                Ok(handle) => {
                    inner._plugin_handles.push(handle);
                    if bass.enable_fx_from_plugin() {
                        eprintln!("BASS FX loaded (pitch-preserving tempo available)");
                    } else {
                        eprintln!("bass_fx.dll loaded but FX entry points were not found");
                    }
                }
                Err(error) => eprintln!("BASS FX plugin not loaded: {error}"),
            }
        }

        inner.eq_context.set_float_dsp_enabled(float_dsp_ok);
        inner.bass = Some(bass);
        Self::load_bass_addons(inner);
        Self::create_mixer(inner)?;
        Ok(())
    }

    fn load_bass_addons(inner: &mut PlayerInner) {
        let Some(bass) = inner.bass.as_ref() else {
            return;
        };

        let Ok(entries) = std::fs::read_dir(&inner.bass_dir) else {
            eprintln!(
                "BASS addons: directory not found at {}",
                inner.bass_dir.display()
            );
            return;
        };

        // Collect names we already successfully loaded so we don't duplicate attempts.
        let mut attempted: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. Load known official format plugins first (better logging).
        for &plugin in BASS_FORMAT_PLUGINS {
            if attempted.contains(&plugin.to_lowercase()) {
                continue;
            }
            let path = inner.bass_dir.join(plugin);
            if !path.is_file() {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            match bass.plugin_load(&path_str) {
                Ok(handle) => {
                    eprintln!("BASS plugin loaded: {plugin}");
                    inner._plugin_handles.push(handle);
                    attempted.insert(plugin.to_lowercase());
                }
                Err(error) => {
                    eprintln!("BASS plugin not loaded: {plugin} ({error})");
                    attempted.insert(plugin.to_lowercase());
                }
            }
        }

        // 2. Auto-detect and load *any* other bass*.dll in the folder.
        // This allows user-provided tracker / chiptune plugins (e.g. basszxtune.dll
        // or similar) to be picked up automatically when placed in the bass/ directory.
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case("dll") {
                continue;
            }

            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_lowercase();
            if attempted.contains(&lower) {
                continue;
            }
            if NON_FORMAT_BASS_DLLS.iter().any(|&ex| lower == ex) {
                continue;
            }
            // Any remaining bass*.dll is a candidate for format plugin (tracker plugins etc.)
            if !lower.starts_with("bass") {
                continue;
            }

            let path_str = path.to_string_lossy().to_string();
            match bass.plugin_load(&path_str) {
                Ok(handle) => {
                    eprintln!("BASS plugin loaded: {name}");
                    inner._plugin_handles.push(handle);
                    attempted.insert(lower);
                }
                Err(error) => {
                    // Non-fatal. Many third-party tracker plugins are old and may
                    // not be compatible with the current bass.dll version.
                    eprintln!("BASS plugin not loaded: {name} ({error})");
                    attempted.insert(lower);
                }
            }
        }
    }

    fn create_mixer(inner: &mut PlayerInner) -> Result<(), String> {
        if inner.mixer_handle != 0 {
            return Ok(());
        }
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        // NONSTOP to keep mixer running (silent) between tracks.
        // We manage gapless manually by adding next source near end of current (avoids
        // keeping extra queued decode sources active during long playback, which can
        // contribute to crackling/underruns over time).
        let flags = bass::BASS_MIXER_NONSTOP | bass::BASS_SAMPLE_FLOAT;
        let mixer = bass.mixer_stream_create(44100, 2, flags)?;
        let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_BUFFER, 0.2);
        // Start the mixer (it will output silence until sources added, or play when first added).
        bass.channel_play(mixer, false)?;
        // Set initial volume on mixer
        bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, inner.volume)?;
        inner.mixer_handle = mixer;
        Ok(())
    }

    pub fn get_equalizer(&self) -> EqualizerSettings {
        if self.on_bass_thread() {
            return self.inner.lock().eq_context.get_settings();
        }
        self.run_on_bass_thread(|inner| Ok(inner.eq_context.get_settings()))
            .unwrap_or_default()
    }

    pub fn get_equalizer_status(&self) -> EqualizerStatus {
        if self.on_bass_thread() {
            return Self::equalizer_status_inner(&self.inner.lock());
        }
        self.run_on_bass_thread(|inner| Ok(Self::equalizer_status_inner(inner)))
            .unwrap_or(EqualizerStatus {
                settings: EqualizerSettings::default(),
                dsp_attached: false,
                process_count: 0,
            })
    }

    fn equalizer_status_inner(inner: &PlayerInner) -> EqualizerStatus {
        EqualizerStatus {
            settings: inner.eq_context.get_settings(),
            dsp_attached: inner.dsp_handle != 0,
            process_count: inner.eq_context.process_count(),
        }
    }

    pub fn set_equalizer(&self, settings: EqualizerSettings) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| Self::set_equalizer_inner(inner, settings))
    }

    fn set_equalizer_inner(
        inner: &mut PlayerInner,
        settings: EqualizerSettings,
    ) -> Result<(), String> {
        let enabled = settings.enabled;
        inner.eq_context.set_settings(settings);

        if inner.mixer_handle == 0 {
            return Ok(());
        }

        if enabled {
            if inner.dsp_handle == 0 {
                Self::attach_dsp_to_mixer(inner)?;
            }
        } else if inner.dsp_handle != 0 {
            Self::detach_dsp(inner);
        }
        Ok(())
    }

    fn detach_dsp(inner: &mut PlayerInner) {
        if inner.dsp_handle == 0 || inner.mixer_handle == 0 {
            inner.dsp_handle = 0;
            inner.eq_context.set_dsp_float_forced(false);
            return;
        }
        if let Some(bass) = inner.bass.as_ref() {
            let _ = bass.channel_remove_dsp(inner.mixer_handle, inner.dsp_handle);
        }
        inner.dsp_handle = 0;
        inner.eq_context.set_dsp_float_forced(false);
    }

    fn attach_dsp_to_mixer(inner: &mut PlayerInner) -> Result<(), String> {
        if inner.mixer_handle == 0 {
            return Ok(());
        }
        Self::detach_dsp(inner);

        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let info = bass.channel_get_info(inner.mixer_handle)?;
        let sample_rate = if info.freq > 0 {
            info.freq
        } else {
            bass.channel_get_attribute(inner.mixer_handle, bass::BASS_ATTRIB_FREQ)
                .unwrap_or(44100.0) as u32
        };
        let sample_rate = if sample_rate > 0 { sample_rate } else { 44100 };

        inner.eq_context.set_dsp_float_forced(true);
        inner
            .eq_context
            .configure_stream(sample_rate, info.chans, info.flags);

        let user = (inner.eq_context as *const EqDspContext) as *mut std::ffi::c_void;
        let dsp = match bass.channel_set_dsp_ex(
            inner.mixer_handle,
            eq_dsp_callback,
            user,
            bass::BASS_DSP_PRIORITY_FIRST,
            bass::BASS_DSP_FLOAT,
        ) {
            Ok(dsp) => dsp,
            Err(_) => {
                inner.eq_context.set_dsp_float_forced(
                    info.flags & bass::BASS_SAMPLE_FLOAT != 0,
                );
                bass.channel_set_dsp(
                    inner.mixer_handle,
                    eq_dsp_callback,
                    bass::BASS_DSP_PRIORITY_FIRST,
                    user,
                )?
            }
        };
        inner.dsp_handle = dsp;
        Ok(())
    }

    pub fn prepare_next(&self, queue: Vec<GaplessTrack>) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            inner.gapless_queue = queue;
            if let Some(current) = inner.current_file.clone() {
                Self::sync_gapless_index(inner, &current);
            } else {
                inner.gapless_queue_index = 0;
                Self::refresh_pending_next(inner);
            }
            Ok(())
        })
    }

    /// Play a file. Reuses the open stream when advancing within the same audio image (CUE).
    pub fn play(
        &self,
        track_path: &str,
        audio_path: Option<&str>,
        cue_start: Option<f64>,
        cue_end: Option<f64>,
        queue: Vec<GaplessTrack>,
    ) -> Result<(), String> {
        let track_path = track_path.to_string();
        let audio_path = audio_path.map(str::to_string);
        self.run_on_bass_thread(move |inner| {
            Self::set_gapless_queue(inner, queue);
            Self::sync_gapless_index(inner, &track_path);
            Self::play_inner(inner, &track_path, audio_path.as_deref(), cue_start, cue_end)?;
            inner.user_paused = false;
            // Record start time for this track (used to guard against early advance after manual que click)
            inner.current_track_start_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            Ok(())
        })
    }

    fn set_gapless_queue(inner: &mut PlayerInner, queue: Vec<GaplessTrack>) {
        inner.gapless_queue = queue;
        inner.gapless_queue_index = 0;
        Self::refresh_pending_next(inner);
    }

    fn sync_gapless_index(inner: &mut PlayerInner, track_path: &str) {
        if let Some(index) = inner
            .gapless_queue
            .iter()
            .position(|track| track.track_path == track_path)
        {
            inner.gapless_queue_index = index;
            Self::refresh_pending_next(inner);
        }
    }

    fn refresh_pending_next(inner: &mut PlayerInner) {
        inner.pending_next = inner
            .gapless_queue
            .get(inner.gapless_queue_index + 1)
            .cloned();
        if let Some(ref track) = inner.pending_next.clone() {
            if let Err(error) = Self::preload_next(inner, track) {
                eprintln!("Gapless preload failed: {error}");
            }
        } else {
            Self::clear_preload(inner);
        }
    }

    fn audio_path_key(path: &str) -> String {
        #[cfg(windows)]
        {
            path.to_lowercase()
        }
        #[cfg(not(windows))]
        {
            path.to_string()
        }
    }

    fn same_audio_path(a: &str, b: &str) -> bool {
        Self::audio_path_key(a) == Self::audio_path_key(b)
    }

    fn can_gapless_reuse(inner: &PlayerInner, audio_path: &str) -> bool {
        inner.current_source != 0
            && inner
                .current_audio_path
                .as_ref()
                .is_some_and(|current| Self::same_audio_path(current, audio_path))
    }

    fn can_use_preloaded(inner: &PlayerInner, audio_path: &str) -> bool {
        inner.preloaded_source != 0
            && inner
                .preloaded_audio_path
                .as_ref()
                .is_some_and(|p| Self::same_audio_path(p, audio_path))
    }

    fn clear_preload(inner: &mut PlayerInner) {
        if inner.preloaded_source != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.mixer_channel_remove(inner.preloaded_source);
                Self::free_playback_channel(
                    bass,
                    inner.preloaded_source,
                    inner.preloaded_decode,
                );
            }
            inner.preloaded_source = 0;
        }
        inner.preloaded_audio_path = None;
        inner.preloaded_decode = 0;
    }

    fn teardown_current(inner: &mut PlayerInner) {
        Self::cancel_rate_ramp(inner);
        if inner.current_source != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.mixer_channel_remove(inner.current_source);
                Self::free_playback_channel(bass, inner.current_source, inner.current_decode);
            }
            inner.current_source = 0;
        }
        // Note: do not detach DSP here — it stays on the mixer
        inner.current_audio_path = None;
        inner.cue_start = None;
        inner.cue_end = None;
        inner.current_decode = 0;
    }

    fn apply_segment_metadata(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) {
        inner.current_file = Some(track_path.to_string());
        inner.current_audio_path = Some(playback.audio_path.clone());
        inner.cue_start = playback.cue_start;
        inner.cue_end = playback.cue_end;
    }

    /// Seek an already-open source (CUE) to segment without reopening.
    fn apply_segment(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) -> Result<(), String> {
        let source = inner.current_source;
        if source == 0 {
            // fallback
            return Self::open_stream(inner, track_path, playback);
        }
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let target = playback.cue_start.unwrap_or(0.0);
        // Use mixer version if source is plugged
        let byte_pos = bass.channel_seconds2bytes(source, target);
        let _ = bass.mixer_channel_set_position(source, byte_pos, bass::BASS_POS_BYTE);
        // Make sure mixer is running
        if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
            let _ = bass.channel_play(inner.mixer_handle, false);
        }
        Self::apply_segment_metadata(inner, track_path, playback);
        Ok(())
    }

    fn has_next_in_gapless_queue(inner: &PlayerInner) -> bool {
        inner.gapless_queue_index + 1 < inner.gapless_queue.len()
    }

    fn create_decode_source(
        inner: &mut PlayerInner,
        audio_path: &str,
        cue_start: Option<f64>,
    ) -> Result<u32, String> {
        let ext = std::path::Path::new(audio_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let is_tracker = matches!(
            ext.as_str(),
            "mod" | "s3m" | "xm" | "it" | "mtm" | "669" | "far" | "okt" |
            "ay" | "ym" | "vgm" | "vgz" | "nsf" | "nsfe" | "gbs" | "hes" |
            "sap" | "kss" | "pt2" | "pt3" | "stc" | "stp" | "asc" | "sqt" | "psg"
        );

        // Tracker files are loaded via BASS_MusicLoad (better compatibility with module plugins).
        // Regular audio uses StreamCreateFile.
        let flags = if is_tracker {
            bass::BASS_SAMPLE_FLOAT | bass::BASS_MUSIC_DECODE | bass::BASS_MUSIC_RAMPS
        } else {
            // No PRESCAN: much faster track start and manual switching.
            // Prescan is only useful for accurate seeking in VBR MP3 without good headers.
            // For speed (like Foobar) we skip it. Duration comes from metadata or later.
            bass::BASS_SAMPLE_FLOAT | bass::BASS_STREAM_DECODE
        };

        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;

        // Try the preferred method first. For trackers we prefer MusicLoad because
        // most module plugins (including many chiptune ones) register via the music API.
        let source = if is_tracker {
            bass.music_load(audio_path, flags)
                .or_else(|e| {
                    // Fallback: some plugins only work through StreamCreateFile
                    if e.contains("unsupported file format") {
                        bass.stream_create_file(audio_path, bass::BASS_SAMPLE_FLOAT | bass::BASS_STREAM_DECODE)
                    } else {
                        Err(e)
                    }
                })
        } else {
            bass.stream_create_file(audio_path, flags)
        }
        .map_err(|error| format!("{error} — file: {}", audio_path))?;

        // Very short wait so length becomes available quickly, but we don't block long.
        // This is critical for fast manual play.
        for _ in 0..8 {
            if bass.channel_get_length(source, bass::BASS_POS_BYTE) > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        if let Some(start) = cue_start {
            Self::seek_channel_to_seconds(bass, source, start)?;
        }
        Ok(source)
    }

    fn add_source_to_mixer(inner: &mut PlayerInner, source: u32) -> Result<(), String> {
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        if inner.mixer_handle == 0 {
            return Err("Mixer not created".to_string());
        }
        // NORAMPIN is key for gapless: no volume ramp on start of this source.
        // No AUTOFREE: rate/pitch changes remove and re-add the channel; AUTOFREE would
        // destroy the decode handle and trigger false gapless track advances.
        let add_flags = bass::BASS_MIXER_CHAN_NORAMPIN;
        bass.mixer_stream_add_channel(inner.mixer_handle, source, add_flags)?;
        Ok(())
    }

    fn open_stream(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) -> Result<(), String> {
        // Teardown previous source (remove from mixer).
        Self::teardown_current(inner);
        Self::clear_preload(inner);

        let was_paused_or_stopped = if let Some(bass) = inner.bass.as_ref() {
            let active = bass.channel_is_active(inner.mixer_handle);
            active != bass::BASS_ACTIVE_PLAYING
        } else {
            false
        };

        let decode = Self::create_decode_source(inner, &playback.audio_path, playback.cue_start)?;
        let rate = inner.playback_rate;
        let pitch_enabled = inner.pitch_enabled;
        let volume = inner.volume;
        let (mixer_channel, tracked_decode) = {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Self::wrap_decode_for_rate(bass, decode, rate, pitch_enabled)?
        };

        Self::add_source_to_mixer(inner, mixer_channel)?;

        if let Some(bass) = inner.bass.as_ref() {
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, volume);
            if was_paused_or_stopped {
                // Hard restart to flush any old buffer from previous track/fade.
                let _ = bass.channel_play(inner.mixer_handle, true);
            } else {
                // Mixer is already playing — just ensure it keeps going.
                let _ = bass.channel_play(inner.mixer_handle, false);
            }
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_BUFFER, 0.2);
        }

        inner.current_source = mixer_channel;
        inner.current_decode = tracked_decode;
        inner.applied_playback_rate = rate;
        Self::cancel_rate_ramp(inner);
        Self::apply_segment_metadata(inner, track_path, playback);

        // Ensure EQ on mixer
        let eq_enabled = inner.eq_context.get_settings().enabled;
        if eq_enabled {
            let _ = Self::attach_dsp_to_mixer(inner);
        } else if inner.dsp_handle != 0 {
            Self::detach_dsp(inner);
        }
        Ok(())
    }

    fn preload_next(inner: &mut PlayerInner, next: &GaplessTrack) -> Result<(), String> {
        let playback = cue::resolve_playback(
            &next.track_path,
            Some(&next.audio_path),
            next.cue_start,
            next.cue_end,
        )?;

        if inner
            .current_audio_path
            .as_ref()
            .is_some_and(|current| Self::same_audio_path(current, &playback.audio_path))
        {
            return Ok(());
        }

        if inner
            .preloaded_audio_path
            .as_ref()
            .is_some_and(|path| Self::same_audio_path(path, &playback.audio_path))
            && inner.preloaded_source != 0
        {
            return Ok(());
        }

        Self::clear_preload(inner);

        let source = Self::create_decode_source(inner, &playback.audio_path, playback.cue_start)?;

        // Just create and position the next decode source.
        // We add it to mixer only near the end of current track (in activate or advance).
        // This keeps only 1 source active in mixer during long playback → more stable, less crackling.

        let rate = inner.playback_rate;
        let pitch_enabled = inner.pitch_enabled;
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let (mixer_channel, tracked_decode) =
            Self::wrap_decode_for_rate(bass, source, rate, pitch_enabled)?;

        inner.preloaded_source = mixer_channel;
        inner.preloaded_decode = tracked_decode;
        inner.preloaded_audio_path = Some(playback.audio_path);
        Ok(())
    }

    fn activate_preloaded(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) -> Result<(), String> {
        let preloaded = inner.preloaded_source;
        let matches = inner
            .preloaded_audio_path
            .as_ref()
            .is_some_and(|path| Self::same_audio_path(path, &playback.audio_path));

        if preloaded == 0 || !matches {
            Self::teardown_current(inner);
            return Self::open_stream(inner, track_path, playback);
        }

        // Remove old source (if still there). Add the preloaded now.
        // Since we add close to end (via poll), gap is tiny or none (inaudible).
        // NORAMPIN for clean join.
        if let Some(bass) = inner.bass.as_ref() {
            if inner.current_source != 0 && inner.current_source != preloaded {
                let _ = bass.mixer_channel_remove(inner.current_source);
                Self::free_playback_channel(
                    bass,
                    inner.current_source,
                    inner.current_decode,
                );
            }
        }

        Self::add_source_to_mixer(inner, preloaded)?;

        if let Some(bass) = inner.bass.as_ref() {
            let start = playback.cue_start.unwrap_or(0.0);
            if start > 0.0 {
                let byte = bass.channel_seconds2bytes(preloaded, start);
                let _ = bass.mixer_channel_set_position(preloaded, byte, bass::BASS_POS_BYTE);
            }
            if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
                let _ = bass.channel_play(inner.mixer_handle, false);
            }
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_BUFFER, 0.2);
        }

        inner.preloaded_source = 0;
        inner.preloaded_audio_path = None;
        inner.current_source = preloaded;
        inner.current_decode = inner.preloaded_decode;
        inner.preloaded_decode = 0;

        Self::apply_segment_metadata(inner, track_path, playback);
        Ok(())
    }

    fn try_advance_gapless(inner: &mut PlayerInner) -> Result<String, String> {
        let next_index = inner.gapless_queue_index + 1;
        let next = inner
            .gapless_queue
            .get(next_index)
            .cloned()
            .ok_or_else(|| "No next track in gapless queue".to_string())?;
        let playback = cue::resolve_playback(
            &next.track_path,
            Some(&next.audio_path),
            next.cue_start,
            next.cue_end,
        )?;

        let same_file = inner
            .current_audio_path
            .as_ref()
            .is_some_and(|current| Self::same_audio_path(current, &playback.audio_path));

        if same_file {
            // Continuous audio image (CUE): never seek on auto-advance — only update bounds.
            Self::apply_segment_metadata(inner, &next.track_path, &playback);
        } else {
            Self::activate_preloaded(inner, &next.track_path, &playback)?;
        }

        inner.gapless_queue_index = next_index;
        inner.user_paused = false;
        Self::refresh_pending_next(inner);
        Ok(next.track_path)
    }

    fn play_inner(
        inner: &mut PlayerInner,
        track_path: &str,
        audio_path: Option<&str>,
        cue_start: Option<f64>,
        cue_end: Option<f64>,
    ) -> Result<(), String> {
        let playback = cue::resolve_playback(track_path, audio_path, cue_start, cue_end)
            .map_err(|error| format!("{error} (track: {track_path})"))?;

        if Self::can_gapless_reuse(inner, &playback.audio_path) {
            // CUE same-file: seek within the already-open stream (instant, no reopening).
            Self::apply_segment(inner, track_path, &playback)?;
            // Always flush the mixer buffer on a manual track switch.
            // Even if the mixer is playing, it holds up to buffer_size ms of old audio.
            // For gapless auto-advance we skip this (seamless), but for manual switches
            // the user wants the new segment to start immediately.
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, inner.volume);
                let _ = bass.channel_play(inner.mixer_handle, true);
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_BUFFER, 0.2);
            }
        } else if Self::can_use_preloaded(inner, &playback.audio_path) {
            // Fast path for manual next/prev when the track was preloaded for gapless.
            // This makes "hand switching" to the next track nearly instant.
            Self::activate_preloaded(inner, track_path, &playback)?;
        } else {
            Self::teardown_current(inner);
            Self::open_stream(inner, track_path, &playback)?;
        }

        // Invalidate any pending scheduled pauses from previous pause() calls.
        inner.pause_generation = inner.pause_generation.wrapping_add(1);

        inner.user_paused = false;
        Self::refresh_pending_next(inner);
        Ok(())
    }


    fn cue_relative_position(inner: &PlayerInner, absolute_secs: f64) -> f64 {
        match inner.cue_start {
            Some(start) => (absolute_secs - start).max(0.0),
            None => absolute_secs,
        }
    }

    fn cue_segment_duration(inner: &PlayerInner, absolute_duration: f64) -> f64 {
        match (inner.cue_start, inner.cue_end) {
            (Some(start), Some(end)) => (end - start).max(0.0),
            (Some(start), None) => (absolute_duration - start).max(0.0),
            _ => absolute_duration,
        }
    }

    fn absolute_seek_position(inner: &PlayerInner, relative_secs: f64) -> f64 {
        let start = inner.cue_start.unwrap_or(0.0);
        let mut absolute = start + relative_secs.max(0.0);
        if let (Some(start), Some(end)) = (inner.cue_start, inner.cue_end) {
            absolute = absolute.clamp(start, end);
        }
        absolute
    }

    fn seek_channel_to_seconds(
        bass: &BassLibrary,
        handle: u32,
        seconds: f64,
    ) -> Result<(), String> {
        let byte_pos = bass.channel_seconds2bytes(handle, seconds);
        bass.channel_set_position(handle, byte_pos, bass::BASS_POS_BYTE)
    }

    fn free_playback_channel(bass: &BassLibrary, mixer_channel: u32, tracked_decode: u32) {
        if mixer_channel == 0 {
            return;
        }
        if tracked_decode != 0 && tracked_decode != mixer_channel {
            let _ = bass.channel_stop(mixer_channel);
            let _ = bass.channel_stop(tracked_decode);
            return;
        }
        let _ = bass.channel_stop(mixer_channel);
    }

    fn decode_handle_for_channel(
        bass: &BassLibrary,
        mixer_channel: u32,
        tracked_decode: u32,
    ) -> u32 {
        if tracked_decode != 0 {
            return tracked_decode;
        }
        let source = bass.fx_tempo_get_source(mixer_channel);
        if source != 0 {
            return source;
        }
        mixer_channel
    }

    fn is_tempo_wrapped(bass: &BassLibrary, mixer_channel: u32, tracked_decode: u32) -> bool {
        tracked_decode != 0 && tracked_decode != mixer_channel
            || bass.fx_tempo_get_source(mixer_channel) != 0
    }

    fn freq_rate_target(bass: &BassLibrary, handle: u32, rate: f32) -> f32 {
        if (rate - 1.0).abs() < 0.001 {
            return 0.0;
        }
        if let Ok(info) = bass.channel_get_info(handle) {
            if info.freq > 0 {
                return ((info.freq as f64) * (rate as f64)) as f32;
            }
        }
        (44100.0 * rate as f64) as f32
    }

    fn apply_freq_rate(bass: &BassLibrary, handle: u32, rate: f32) {
        let target = Self::freq_rate_target(bass, handle, rate);
        let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_FREQ, target);
    }

    fn smoothstep(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn cancel_rate_ramp(inner: &mut PlayerInner) {
        inner.rate_ramp_generation = inner.rate_ramp_generation.wrapping_add(1);
    }

    /// Build the channel that should be fed into the mixer for the given decode source.
    /// Returns `(mixer_channel, tracked_decode)` where `tracked_decode` is non-zero only
    /// when a tempo wrapper owns the underlying decode stream.
    fn wrap_decode_for_rate(
        bass: &BassLibrary,
        decode: u32,
        rate: f32,
        pitch_enabled: bool,
    ) -> Result<(u32, u32), String> {
        let _ = bass.channel_set_attribute(decode, bass::BASS_ATTRIB_FREQ, 0.0);
        let _ = bass.channel_set_attribute(decode, bass::BASS_ATTRIB_TEMPO, 0.0);

        if (rate - 1.0).abs() < 0.001 {
            return Ok((decode, 0));
        }

        if pitch_enabled || !bass.has_fx() {
            Self::apply_freq_rate(bass, decode, rate);
            return Ok((decode, 0));
        }

        let tempo = bass.fx_tempo_create(decode, bass::BASS_STREAM_DECODE)?;
        let tempo_pct = (rate - 1.0) * 100.0;
        bass.channel_set_attribute(tempo, bass::BASS_ATTRIB_TEMPO, tempo_pct)?;
        Ok((tempo, decode))
    }

    fn wants_tempo_wrap(bass: &BassLibrary, rate: f32, pitch_enabled: bool) -> bool {
        !pitch_enabled && bass.has_fx() && (rate - 1.0).abs() >= 0.001
    }

    fn now_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn apply_rate_in_place(
        bass: &BassLibrary,
        channel: u32,
        tracked_decode: u32,
        rate: f32,
        pitch_enabled: bool,
    ) -> Result<(u32, u32), String> {
        if channel == 0 {
            return Ok((0, 0));
        }

        let wrapped = Self::is_tempo_wrapped(bass, channel, tracked_decode);

        // Neutral speed on an existing tempo wrapper — avoid tearing down mid-ramp.
        if wrapped && (rate - 1.0).abs() < 0.001 {
            bass.channel_set_attribute(channel, bass::BASS_ATTRIB_TEMPO, 0.0)?;
            return Ok((channel, tracked_decode));
        }

        let wants_wrap = Self::wants_tempo_wrap(bass, rate, pitch_enabled);

        if wants_wrap == wrapped {
            if wants_wrap {
                let tempo_pct = (rate - 1.0) * 100.0;
                bass.channel_set_attribute(channel, bass::BASS_ATTRIB_TEMPO, tempo_pct)?;
                Ok((channel, tracked_decode))
            } else {
                let decode = Self::decode_handle_for_channel(bass, channel, tracked_decode);
                Self::apply_freq_rate(bass, decode, rate);
                Ok((channel, 0))
            }
        } else {
            Err("playback mode switch requires channel rebuild".to_string())
        }
    }

    fn refresh_playback_channel(
        bass: &BassLibrary,
        mixer_handle: u32,
        mixer_channel: u32,
        tracked_decode: u32,
        in_mixer: bool,
        rate: f32,
        pitch_enabled: bool,
    ) -> Result<(u32, u32), String> {
        if mixer_channel == 0 {
            return Ok((0, 0));
        }

        let decode = Self::decode_handle_for_channel(bass, mixer_channel, tracked_decode);
        let pos_bytes = if in_mixer {
            bass.mixer_channel_get_position(mixer_channel, bass::BASS_POS_BYTE)
        } else {
            bass.channel_get_position(mixer_channel, bass::BASS_POS_BYTE)
        };

        if in_mixer {
            let _ = bass.mixer_channel_remove(mixer_channel);
        }

        if Self::is_tempo_wrapped(bass, mixer_channel, tracked_decode) {
            let _ = bass.channel_stop(mixer_channel);
        } else {
            let _ = bass.channel_set_attribute(decode, bass::BASS_ATTRIB_FREQ, 0.0);
        }

        let (new_channel, new_decode) =
            Self::wrap_decode_for_rate(bass, decode, rate, pitch_enabled)?;

        if in_mixer {
            if mixer_handle == 0 {
                return Err("Mixer not created".to_string());
            }
            let add_flags = bass::BASS_MIXER_CHAN_NORAMPIN;
            bass.mixer_stream_add_channel(mixer_handle, new_channel, add_flags)?;
            if pos_bytes > 0 {
                let _ = bass.mixer_channel_set_position(new_channel, pos_bytes, bass::BASS_POS_BYTE);
            }
        }

        Ok((new_channel, new_decode))
    }

    fn reapply_at_rate(inner: &mut PlayerInner, rate: f32) {
        let pitch_enabled = inner.pitch_enabled;
        let mixer_handle = inner.mixer_handle;
        let current_source = inner.current_source;
        let current_decode = inner.current_decode;
        let preloaded_source = inner.preloaded_source;
        let preloaded_decode = inner.preloaded_decode;
        let Some(bass) = inner.bass.as_ref() else {
            return;
        };

        if current_source != 0 {
            match Self::apply_rate_in_place(
                bass,
                current_source,
                current_decode,
                rate,
                pitch_enabled,
            ) {
                Ok((channel, decode)) => {
                    inner.current_source = channel;
                    inner.current_decode = decode;
                }
                Err(_) => {
                    inner.suppress_gapless_until = Self::now_millis().saturating_add(800);
                    if let Ok((channel, decode)) = Self::refresh_playback_channel(
                        bass,
                        mixer_handle,
                        current_source,
                        current_decode,
                        true,
                        rate,
                        pitch_enabled,
                    ) {
                        inner.current_source = channel;
                        inner.current_decode = decode;
                    }
                }
            }
        }

        if preloaded_source != 0 {
            match Self::apply_rate_in_place(
                bass,
                preloaded_source,
                preloaded_decode,
                rate,
                pitch_enabled,
            ) {
                Ok((channel, decode)) => {
                    inner.preloaded_source = channel;
                    inner.preloaded_decode = decode;
                }
                Err(_) => {
                    if let Ok((channel, decode)) = Self::refresh_playback_channel(
                        bass,
                        mixer_handle,
                        preloaded_source,
                        preloaded_decode,
                        false,
                        rate,
                        pitch_enabled,
                    ) {
                        inner.preloaded_source = channel;
                        inner.preloaded_decode = decode;
                    }
                }
            }
        }
    }

    fn run_rate_ramp(player: Player, from: f32, to: f32, generation: u64) {
        let step_ms = (PLAYBACK_RATE_RAMP_MS / RATE_RAMP_STEPS).max(1);
        for step in 1..=RATE_RAMP_STEPS {
            thread::sleep(Duration::from_millis(step_ms as u64));
            let t = Self::smoothstep(step as f32 / RATE_RAMP_STEPS as f32);
            let current = from + (to - from) * t;
            let still_active = player
                .run_on_bass_thread(move |inner| {
                    if inner.rate_ramp_generation != generation {
                        return Ok(false);
                    }
                    Self::reapply_at_rate(inner, current);
                    inner.applied_playback_rate = current;
                    Ok(true)
                })
                .unwrap_or(false);
            if !still_active {
                return;
            }
        }

        let _ = player.run_on_bass_thread(move |inner| {
            if inner.rate_ramp_generation != generation {
                return Ok(());
            }
            Self::reapply_at_rate(inner, to);
            inner.applied_playback_rate = to;
            Ok(())
        });
    }

    fn stream_duration_secs(bass: &BassLibrary, handle: u32) -> f64 {
        let len_bytes = bass.channel_get_length(handle, bass::BASS_POS_BYTE);
        bass.channel_bytes2seconds(handle, len_bytes)
    }

    fn track_end_position(inner: &PlayerInner, bass: &BassLibrary, handle: u32) -> f64 {
        if let Some(end) = inner.cue_end {
            return end;
        }
        Self::stream_duration_secs(bass, handle)
    }

    fn track_ending(inner: &PlayerInner, bass: &BassLibrary, absolute_secs: f64) -> bool {
        if inner.current_source == 0 {
            return false;
        }
        let end = Self::track_end_position(inner, bass, inner.current_source);
        absolute_secs + GAPLESS_END_EPSILON_SECS >= end
    }

    pub fn pause(&self) -> Result<(), String> {
        // Start the fade-out immediately (Spotify-style smooth tail)
        let (fade_started, gen) = self.run_on_bass_thread(|inner| {
            if inner.mixer_handle != 0 {
                inner.user_paused = true;
                inner.pause_generation = inner.pause_generation.wrapping_add(1);
                let gen = inner.pause_generation;
                if let Some(bass) = inner.bass.as_ref() {
                    let _ = bass.channel_slide_attribute(
                        inner.mixer_handle,
                        bass::BASS_ATTRIB_VOL,
                        0.0,
                        PAUSE_FADE_MS,
                    );
                }
                Ok((true, gen))
            } else {
                Ok((false, 0))
            }
        })?;

        if fade_started {
            // Schedule the actual pause after the fade has played out.
            // This lets the full musical fade be heard (like Spotify).
            // We pass the generation so that if user plays a new track in the meantime,
            // the stale pause does nothing.
            let this = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(PAUSE_FADE_MS as u64 + 10));
                let _ = this.run_on_bass_thread(move |inner| {
                    if inner.mixer_handle != 0 && inner.pause_generation == gen {
                        if let Some(bass) = inner.bass.as_ref() {
                            let _ = bass.channel_pause(inner.mixer_handle);
                        }
                        inner.user_paused = true;
                    }
                    Ok(())
                });
            });
        }

        Ok(())
    }

    pub fn resume(&self) -> Result<(), String> {
        self.run_on_bass_thread(|inner| {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            if inner.mixer_handle != 0 {
                inner.user_paused = false;
                let target = inner.volume;
                // Force start from silence, play, then musical fade-in (Spotify style)
                let _ = bass.channel_set_attribute(
                    inner.mixer_handle,
                    bass::BASS_ATTRIB_VOL,
                    0.0,
                );
                bass.channel_play(inner.mixer_handle, false)?;
                bass.channel_slide_attribute(
                    inner.mixer_handle,
                    bass::BASS_ATTRIB_VOL,
                    target,
                    RESUME_FADE_MS,
                )
            } else {
                Err("Nothing is playing".into())
            }
        })
    }

    pub fn stop(&self) -> Result<(), String> {
        self.run_on_bass_thread(|inner| {
            Self::teardown_current(inner);
            Self::clear_preload(inner);
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    let _ = bass.channel_stop(inner.mixer_handle);
                }
            }
            inner.current_file = None;
            inner.gapless_queue.clear();
            inner.gapless_queue_index = 0;
            inner.pending_next = None;
            inner.user_paused = false;
            Ok(())
        })
    }

    /// Stop playback and free the BASS audio device.
    /// Called on window close so that audio does not continue playing after the app has exited.
    pub fn shutdown(&self) -> Result<(), String> {
        self.run_on_bass_thread(|inner| {
            Self::teardown_current(inner);
            Self::clear_preload(inner);
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    let _ = bass.channel_stop(inner.mixer_handle);
                }
                // Free releases the output device and stops any background audio threads.
                let _ = bass.free();
            }
            inner.mixer_handle = 0;
            inner.bass = None;
            inner.dsp_handle = 0;
            inner.current_file = None;
            inner.current_audio_path = None;
            inner.cue_start = None;
            inner.cue_end = None;
            inner.gapless_queue.clear();
            inner.gapless_queue_index = 0;
            inner.pending_next = None;
            Ok(())
        })
    }

    /// Seek to a position in seconds.
    /// "Наложение" (overlap) style, not full затухание:
    /// Quick shallow volume dip (not to zero), immediate seek, quick restore.
    /// Feels like a smooth blend/transition during scrub, similar to Spotify.
    /// No full silence, short times. Not used on track changes.
    pub fn seek(&self, position_secs: f64) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            if inner.current_source == 0 || inner.mixer_handle == 0 {
                return Err("Nothing is playing".into());
            }
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            let target = inner.volume;

            // Instant seek: hard jump + flush. No long animation.
            // Dip only for the flush moment to avoid click, then immediate restore.
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, SEEK_DIP_LEVEL);

            let absolute_secs = Self::absolute_seek_position(inner, position_secs);
            let byte = bass.channel_seconds2bytes(inner.current_source, absolute_secs);
            let _ = bass.mixer_channel_set_position(inner.current_source, byte, bass::BASS_POS_BYTE);

            // This flush makes the seek actually happen right now (discards stale buffer data).
            let _ = bass.channel_play(inner.mixer_handle, true);
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_BUFFER, 0.2);

            // Restore volume hard for maximum speed.
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, target);

            Ok(())
        })
    }

    /// Set volume (0.0 — 1.0).
    pub fn set_volume(&self, vol: f32) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            inner.volume = vol.clamp(0.0, 1.0);
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    bass.channel_set_attribute(
                        inner.mixer_handle,
                        bass::BASS_ATTRIB_VOL,
                        inner.volume,
                    )?;
                }
            }
            Ok(())
        })
    }

    /// Set playback rate (0.25 — 2.0).
    pub fn set_playback_rate(&self, rate: f32) -> Result<(), String> {
        let rate = rate.clamp(0.25, 2.0);
        let (from, generation) = self.run_on_bass_thread(move |inner| {
            inner.playback_rate = rate;
            Self::cancel_rate_ramp(inner);
            let generation = inner.rate_ramp_generation;
            Ok((inner.applied_playback_rate, generation))
        })?;

        if (from - rate).abs() < 0.001 {
            return Ok(());
        }

        let player = self.clone();
        thread::spawn(move || Self::run_rate_ramp(player, from, rate, generation));
        Ok(())
    }

    /// When enabled, playback speed also shifts pitch. When disabled, tempo FX preserves pitch.
    pub fn set_pitch_enabled(&self, enabled: bool) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            inner.pitch_enabled = enabled;
            Self::cancel_rate_ramp(inner);
            let rate = inner.playback_rate;
            Self::reapply_at_rate(inner, rate);
            inner.applied_playback_rate = rate;
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub fn get_playback_rate(&self) -> f32 {
        if self.on_bass_thread() {
            return self.inner.lock().playback_rate;
        }
        self.run_on_bass_thread(|inner| Ok(inner.playback_rate)).unwrap_or(1.0)
    }

    /// Release a stream that finished playing (must run before AUTOFREE-style invalidation).
    pub fn release_ended_stream(&self) {
        if self.on_bass_thread() {
            Self::release_ended_stream_inner(&mut self.inner.lock());
            return;
        }
        let _ = self.run_on_bass_thread(|inner| {
            Self::release_ended_stream_inner(inner);
            Ok(())
        });
    }

    fn release_ended_stream_inner(inner: &mut PlayerInner) {
        Self::teardown_current(inner);
        inner.user_paused = false;
        // Do not stop the mixer here — it allows smoother resume / next play.
        // Full stop() command will handle explicit stop.
    }

    /// Get a snapshot of the current player state.
    pub fn get_state(&self) -> PlayerStateSnapshot {
        if self.on_bass_thread() {
            return Self::get_state_inner(&mut self.inner.lock());
        }
        self.run_on_bass_thread(|inner| Ok(Self::get_state_inner(inner)))
            .unwrap_or(PlayerStateSnapshot {
                state: PlaybackState::Stopped,
                is_playing: false,
                is_paused: false,
                volume: 1.0,
                position: 0.0,
                duration: 0.0,
                current_file: None,
                current_file_name: None,
            })
    }

    fn get_state_inner(inner: &mut PlayerInner) -> PlayerStateSnapshot {
        if inner.current_source == 0 || inner.mixer_handle == 0 || inner.bass.is_none() {
            let file_name = inner.current_file.as_ref().map(|f| {
                std::path::Path::new(f)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            return PlayerStateSnapshot {
                state: PlaybackState::Stopped,
                is_playing: false,
                is_paused: false,
                volume: inner.volume,
                position: 0.0,
                duration: 0.0,
                current_file: inner.current_file.clone(),
                current_file_name: file_name,
            };
        }

        let bass = inner.bass.as_ref().unwrap();
        let src_active_raw = bass.channel_is_active(inner.current_source);
        let mixer_active_raw = bass.channel_is_active(inner.mixer_handle);
        let src_active: PlaybackState = src_active_raw.into();
        let mixer_active: PlaybackState = mixer_active_raw.into();

        // Mixer with NONSTOP keeps PLAYING (silence) after last source ends.
        // Report Stopped based on the *source* being gone or ended.
        let source_ended = src_active == PlaybackState::Stopped || inner.current_source == 0;
        if source_ended || mixer_active == PlaybackState::Stopped {
            let pos_bytes = bass.mixer_channel_get_position(inner.current_source, bass::BASS_POS_BYTE);
            let len_bytes = bass.channel_get_length(inner.current_source, bass::BASS_POS_BYTE);
            let absolute_position = bass.channel_bytes2seconds(inner.current_source, pos_bytes);
            let absolute_duration = bass.channel_bytes2seconds(inner.current_source, len_bytes);
            let position = Self::cue_relative_position(inner, absolute_position);
            let duration = Self::cue_segment_duration(inner, absolute_duration);
            let file_name = inner.current_file.as_ref().map(|f| {
                std::path::Path::new(f)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            return PlayerStateSnapshot {
                state: PlaybackState::Stopped,
                is_playing: false,
                is_paused: false,
                volume: inner.volume,
                position,
                duration,
                current_file: inner.current_file.clone(),
                current_file_name: file_name,
            };
        }

        let active_raw = mixer_active_raw;
        let active: PlaybackState = active_raw.into();

        let pos_bytes = bass.mixer_channel_get_position(inner.current_source, bass::BASS_POS_BYTE);
        let len_bytes = bass.channel_get_length(inner.current_source, bass::BASS_POS_BYTE);
        let absolute_position = bass.channel_bytes2seconds(inner.current_source, pos_bytes);
        let absolute_duration = bass.channel_bytes2seconds(inner.current_source, len_bytes);
        let position = Self::cue_relative_position(inner, absolute_position);
        let duration = Self::cue_segment_duration(inner, absolute_duration);

        let file_name = inner.current_file.as_ref().map(|f| {
            std::path::Path::new(f)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        });

        // Respect logical user_paused so that pause() reports paused state immediately
        // (for UI) even while the musical fade-out is still in progress on the mixer.
        let (report_playing, report_paused, report_state) = if inner.user_paused {
            (false, true, PlaybackState::Paused)
        } else {
            (
                active == PlaybackState::Playing,
                active == PlaybackState::Paused,
                active,
            )
        };

        PlayerStateSnapshot {
            state: report_state,
            is_playing: report_playing,
            is_paused: report_paused,
            volume: inner.volume,
            position,
            duration,
            current_file: inner.current_file.clone(),
            current_file_name: file_name,
        }
    }

    /// Load a BASS addon DLL by path (useful for additional tracker plugins).
    pub fn load_addon(&self, path: &str) -> Result<(), String> {
        let path = path.to_string();
        self.run_on_bass_thread(move |inner| {
            let addon_path = Path::new(&path);
            let full_path = if addon_path.is_absolute() {
                addon_path.to_path_buf()
            } else {
                inner.bass_dir.join(addon_path)
            };
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            let path_str = full_path.to_string_lossy().to_string();
            let handle = bass.plugin_load(&path_str)?;
            inner._plugin_handles.push(handle);
            Ok(())
        })
    }

    pub fn mark_bass_thread(&self) {
        *self.bass_thread.write() = Some(thread::current().id());
    }

    /// Poll playback on the main thread and emit position / track-ended events.
    ///
    /// BASS must only be called from the thread that invoked `BASS_Init`.
    pub fn start_position_emitter(&self, app: AppHandle) {
        let player = self.clone();
        let was_playing = Arc::new(StdMutex::new(false));
        let last_rpc_state = Arc::new(StdMutex::new(None::<PlaybackState>));
        Self::schedule_position_poll(app, player, was_playing, last_rpc_state, POLL_INTERVAL_MS);
    }

    fn schedule_position_poll(
        app: AppHandle,
        player: Player,
        was_playing: Arc<StdMutex<bool>>,
        last_rpc_state: Arc<StdMutex<Option<PlaybackState>>>,
        poll_ms: u64,
    ) {
        let app_for_sleep = app.clone();
        let player_for_main = player.clone();
        let was_for_main = was_playing.clone();
        let rpc_for_main = last_rpc_state.clone();
        let app_for_next = app.clone();
        let player_for_next = player.clone();
        let was_for_next = was_playing;
        let rpc_for_next = last_rpc_state;

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(poll_ms));
            let app_emit = app_for_sleep.clone();
            let _ = app_for_sleep.run_on_main_thread(move || {
                let poll_result = player_for_main.run_on_bass_thread(|inner| {
                    if inner.current_source == 0 || inner.mixer_handle == 0 || inner.bass.is_none() {
                        return Ok((None, POLL_INTERVAL_MS));
                    }
                    let bass = inner.bass.as_ref().unwrap();
                    let mixer_active = bass.channel_is_active(inner.mixer_handle);
                    let src_active = if inner.current_source != 0 {
                        bass.channel_is_active(inner.current_source)
                    } else {
                        bass::BASS_ACTIVE_STOPPED
                    };
                    let pos_bytes =
                        bass.mixer_channel_get_position(inner.current_source, bass::BASS_POS_BYTE);
                    let absolute =
                        bass.channel_bytes2seconds(inner.current_source, pos_bytes);

                    let playing = mixer_active == bass::BASS_ACTIVE_PLAYING;
                    let ending = playing && Self::track_ending(inner, bass, absolute);
                    // Source ended (or no source) means the track is done, even if mixer is still "playing" silence (NONSTOP)
                    let stream_done = src_active == bass::BASS_ACTIVE_STOPPED || mixer_active == bass::BASS_ACTIVE_STOPPED;

                    let now = Self::now_millis();
                    if Self::has_next_in_gapless_queue(inner)
                        && (ending || stream_done)
                        && now >= inner.suppress_gapless_until
                    {
                        // Guard against spurious early advance after manual click in que (new track can briefly appear done).
                        // Use time since this track started (set in play()), not just pos (which can be small at legit end in recovery).
                        if now.saturating_sub(inner.current_track_start_time) > 1500 {
                            return Self::try_advance_gapless(inner)
                                .map(|path| (Some(path), POLL_INTERVAL_MS))
                                .map_err(|error| {
                                    eprintln!("Gapless advance failed: {error}");
                                    error
                                });
                        }
                    }

                    if (ending || stream_done) && !Self::has_next_in_gapless_queue(inner) {
                        // End of playlist: ensure source is gone (AUTOFREE usually handles it).
                        // Do not stop the mixer — keep it running (silent) so next playback starts without device hiccup.
                        if inner.current_source != 0 {
                            let _ = bass.mixer_channel_remove(inner.current_source);
                            Self::free_playback_channel(
                                bass,
                                inner.current_source,
                                inner.current_decode,
                            );
                            inner.current_source = 0;
                            inner.current_decode = 0;
                        }
                    }

                    Ok((None, POLL_INTERVAL_MS))
                });

                let (advanced_path, next_poll_ms) = match poll_result {
                    Ok(result) => result,
                    Err(error) => {
                        eprintln!("Gapless poll failed: {error}");
                        (None, POLL_INTERVAL_MS)
                    }
                };

                if let Some(path) = advanced_path {
                    let mut was = was_for_main.lock().unwrap_or_else(|e| e.into_inner());
                    *was = true;
                    let snapshot = player_for_main.get_state();
                    let _ = app_emit.emit(
                        "player:track-changed",
                        TrackChangedPayload { path: path.clone() },
                    );
                    let _ = app_emit.emit(
                        "player:position",
                        PositionPayload {
                            position: snapshot.position,
                            duration: snapshot.duration,
                            state: snapshot.state,
                        },
                    );
                    player_for_main.sync_discord_presence();
                    if let Ok(mut rpc_state) = rpc_for_main.lock() {
                        *rpc_state = Some(snapshot.state);
                    }
                    Self::schedule_position_poll(
                        app_for_next,
                        player_for_next,
                        was_for_next,
                        rpc_for_next,
                        POLL_INTERVAL_MS,
                    );
                    return;
                }

                let snapshot = player_for_main.get_state();

                let mut was = was_for_main.lock().unwrap_or_else(|e| e.into_inner());
                match snapshot.state {
                    PlaybackState::Playing => {
                        *was = true;
                        let payload = PositionPayload {
                            position: snapshot.position,
                            duration: snapshot.duration,
                            state: snapshot.state,
                        };
                        let _ = app_emit.emit("player:position", &payload);
                        // Note: do not emit player:state on every poll tick.
                        // The position event already carries the state, and the
                        // frontend position listener applies it (with pause guard).
                        // Frequent player:state was causing extra clobbering of isPlaying.
                    }
                    PlaybackState::Paused => {
                        *was = false;
                        let payload = PositionPayload {
                            position: snapshot.position,
                            duration: snapshot.duration,
                            state: snapshot.state,
                        };
                        let _ = app_emit.emit("player:position", &payload);
                    }
                    PlaybackState::Stopped if *was => {
                        let recovered = player_for_main
                            .run_on_bass_thread(|inner| {
                                if !Self::has_next_in_gapless_queue(inner) {
                                    return Ok(None);
                                }
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                if now.saturating_sub(inner.current_track_start_time) > 1500 {
                                    Self::try_advance_gapless(inner).map(Some).map_err(|error| {
                                        eprintln!("Gapless recovery failed: {error}");
                                        error
                                    })
                                } else {
                                    Ok(None)
                                }
                            })
                            .ok()
                            .flatten();

                        if let Some(path) = recovered {
                            *was = true;
                            let snapshot = player_for_main.get_state();
                            let _ = app_emit.emit(
                                "player:track-changed",
                                TrackChangedPayload { path },
                            );
                            let _ = app_emit.emit(
                                "player:position",
                                PositionPayload {
                                    position: snapshot.position,
                                    duration: snapshot.duration,
                                    state: snapshot.state,
                                },
                            );
                            player_for_main.sync_discord_presence();
                            if let Ok(mut rpc_state) = rpc_for_main.lock() {
                                *rpc_state = Some(snapshot.state);
                            }
                            Self::schedule_position_poll(
                                app_for_next,
                                player_for_next,
                                was_for_next,
                                rpc_for_next,
                                POLL_INTERVAL_MS,
                            );
                            return;
                        }

                        player_for_main.release_ended_stream();
                        *was = false;
                        let _ = app_emit.emit("player:track-ended", serde_json::json!({}));
                        player_for_main.sync_discord_presence();
                        if let Ok(mut rpc_state) = rpc_for_main.lock() {
                            *rpc_state = Some(PlaybackState::Stopped);
                        }
                    }
                    _ => {
                        if snapshot.state != PlaybackState::Paused {
                            *was = false;
                        }
                    }
                }

                if let Ok(mut rpc_state) = rpc_for_main.lock() {
                    if *rpc_state != Some(snapshot.state) {
                        if snapshot.state == PlaybackState::Paused
                            || snapshot.state == PlaybackState::Stopped
                            || *rpc_state == Some(PlaybackState::Paused)
                        {
                            player_for_main.sync_discord_presence();
                        }
                        *rpc_state = Some(snapshot.state);
                    }
                }

                Self::schedule_position_poll(
                    app_for_next,
                    player_for_next,
                    was_for_next,
                    rpc_for_next,
                    next_poll_ms,
                );
            });
        });
    }
}
