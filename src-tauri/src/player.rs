// Player state management
//
// Wraps BASS in a higher-level API that tracks the current track, volume,
// playback state, and emits Tauri events for position updates.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex as StdMutex};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::bass::{self, BassLibrary};
use crate::cue::{self, PlaybackTarget};
use crate::discord_rpc::DiscordPresence;
use crate::equalizer::{eq_dsp_callback, EqDspContext, EqualizerSettings};
use crate::mix_filter::{self, MixFilterCtx};

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
/// Duration of a smooth playback-rate transition (presets, slider settle, etc.).
const PLAYBACK_RATE_RAMP_MS: u32 = 1000;

// For seek: mute during flush (instant), then short fade-in so the first post-restart
// sample isn't slammed in at full amplitude.
const SEEK_DIP_LEVEL: f32 = 0.0;
/// Fade-in after seek flush — long enough to de-click, short enough to feel instant.
const SEEK_FADE_IN_MS: u32 = 28;
/// Manual next/prev / track click: fade OUT the old track before teardown, then fade IN.
/// Hard mute of a full-volume waveform is the classic switch click; seek doesn't need
/// this because content continues and the dip is very short.
const MANUAL_SWITCH_FADE_OUT_MS: u32 = 55;
const MANUAL_SWITCH_FADE_IN_MS: u32 = 50;
/// Mixer playback buffer. Slightly larger than the device period stack so brief
/// main-thread stalls (UI reload, SQLite, Discord) do not underrun into silence.
const MIXER_BUFFER_SECS: f32 = 0.35;

// Gapless: treat as ended this far before INDEX/file end so the next source is
// already feeding the mixer while the last buffer of the current track plays out.
// Keep tight — large values cut multi-file CUE tracks early and feel like "wrong skip".
const GAPLESS_END_EPSILON_SECS: f64 = 0.025;
/// Same-image CUE segments may continue without seeking only at the same boundary.
const CUE_CONTIGUOUS_TOLERANCE_SECS: f64 = 0.075;
/// How often BASS is polled for gapless / end detection while a source is active.
const POLL_INTERVAL_MS: u64 = 50;
/// When nothing is loaded, the poll thread sleeps this long and skips main-thread hops.
const POLL_IDLE_MS: u64 = 750;
/// After a manual CUE seek / segment switch, ignore end/stop signals briefly.
const MANUAL_SEGMENT_SUPPRESS_MS: u64 = 400;
/// UI position events while the main window is focused (smooth seekbar).
/// Deliberately slower than POLL_INTERVAL_MS — gapless needs 50ms BASS checks,
/// but WebView IPC + Svelte updates do not (MediaSlider CSS eases the bar).
const UI_EMIT_HOT_MS: u64 = 150;
/// UI position events while unfocused — keeps WebView/GPU quiet during games.
const UI_EMIT_COLD_MS: u64 = 500;
/// Re-push Discord RPC timestamps while playing so progress stays in sync.
const RPC_POSITION_SYNC_MS: u64 = 5000;
/// After a manual play/seek, a new stream can briefly look "ended". Ignore end
/// detection for this long on normal-length tracks. Short tracks cap the guard
/// by their own duration so they can still auto-advance.
const SPURIOUS_END_GUARD_MS: u64 = 1500;

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
    /// Rate last requested on the active decode/tempo channel (BASS may still be sliding to it).
    applied_playback_rate: f32,
    /// When true, speed changes also shift pitch (BASS_ATTRIB_FREQ). When false, tempo FX preserves pitch.
    pitch_enabled: bool,
    /// Raw decode handle when `current_source` is a tempo wrapper (otherwise 0).
    current_decode: u32,
    preloaded_decode: u32,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
    /// After a CUE seek, some BASS/mixer paths report 0-based position (relative to
    /// the seek point) instead of absolute file time. INDEX end checks must then
    /// add `cue_start`. Detected right after each segment open/seek.
    cue_pos_relative: bool,
    eq_context: &'static EqDspContext,
    /// Handles returned by BASS_PluginLoad — keep plugins registered.
    _plugin_handles: Vec<u32>,
    /// Full play-order queue; index points at the track currently playing.
    gapless_queue: Vec<GaplessTrack>,
    gapless_queue_index: usize,
    pending_next: Option<GaplessTrack>,
    preloaded_source: u32,
    preloaded_audio_path: Option<String>,
    /// Playlist/virtual path the preload was built for (match this on activate — not only audio path).
    preloaded_track_path: Option<String>,
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
    /// Two-deck mix: outgoing still in mixer until `fade_end_ms`.
    mix_crossfade: Option<MixCrossfadeState>,
    /// Volume automation on the surviving next deck after dual-deck handoff.
    mix_vol_follow: Option<MixVolFollow>,
    /// True while Mix Transition window owns the shared BASS device for preview.
    /// Main UI should ignore player events so library transport doesn't "join" the mix.
    mix_preview_active: bool,
}

/// Automation point: `t` in 0..1 of the segment, `v` gain 0..1.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixVolPoint {
    pub t: f64,
    pub v: f64,
}

/// Envelope interpolation: straight segments or Catmull-Rom smooth.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MixEnvCurve {
    #[default]
    Linear,
    Smooth,
}

/// One automation block on the mix timeline (seconds from preview start).
/// Used for volume (gain) and filter (cutoff) envelopes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixVolSegment {
    pub start_secs: f64,
    pub duration_secs: f64,
    pub points: Vec<MixVolPoint>,
    #[serde(default)]
    pub curve: MixEnvCurve,
}

/// LP/HP DSP attached to one mix deck source.
struct MixDeckFilter {
    source: u32,
    dsp: u32,
    ctx: Box<MixFilterCtx>,
}

/// Layered two-deck mix preview — plays exactly as aligned on the timeline.
struct MixCrossfadeState {
    from_source: u32,
    from_decode: u32,
    to_source: u32,
    to_decode: u32,
    /// How long the previous deck should run from its cue start (seconds of audio).
    /// 0 = no previous / already dropped.
    from_duration_secs: f64,
    /// Inject next when previous has played this many seconds (graph delay). 0 = already in.
    to_delay_secs: f64,
    /// Original graph delay of next (kept after inject for mix-clock on to).
    to_graph_delay_secs: f64,
    from_cue_start: f64,
    /// Wall-clock start of this mix session (for silence gaps after prev ends).
    mix_timeline_start_ms: u64,
    /// When prev was dropped before next's graph time: mix_secs at that moment.
    /// 0 = prev still alive (or never started).
    from_ended_mix_secs: f64,
    /// Wall ms when prev was dropped early (0 = n/a).
    from_ended_at_ms: u64,
    /// Pending next deck not yet in the mixer (to_source==0 until inject).
    pending_to: Option<PendingMixDeck>,
    next_path: String,
    next_audio_path: String,
    next_cue_start: Option<f64>,
    next_cue_end: Option<f64>,
    /// Volume envelopes for previous / next decks (mix timeline).
    from_vol: Vec<MixVolSegment>,
    to_vol: Vec<MixVolSegment>,
    /// Low-pass / high-pass cutoff envelopes (normalized v → Hz in apply).
    from_lp: Vec<MixVolSegment>,
    from_hp: Vec<MixVolSegment>,
    to_lp: Vec<MixVolSegment>,
    to_hp: Vec<MixVolSegment>,
    /// Speed / rate envelopes (normalized v → 0.5×..2×).
    from_speed: Vec<MixVolSegment>,
    to_speed: Vec<MixVolSegment>,
    from_filter: Option<MixDeckFilter>,
    to_filter: Option<MixDeckFilter>,
}

/// After previous deck is dropped, keep driving next-deck automation.
struct MixVolFollow {
    source: u32,
    decode: u32,
    to_graph_delay_secs: f64,
    to_cue_start: f64,
    to_vol: Vec<MixVolSegment>,
    to_lp: Vec<MixVolSegment>,
    to_hp: Vec<MixVolSegment>,
    to_speed: Vec<MixVolSegment>,
    filter: Option<MixDeckFilter>,
}

struct PendingMixDeck {
    source: u32,
    decode: u32,
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
    /// Serializes play/pause/resume/seek so concurrent IPC calls cannot deadlock the main thread.
    ops: Arc<Mutex<()>>,
    app: Arc<RwLock<Option<AppHandle>>>,
    bass_thread: Arc<RwLock<Option<thread::ThreadId>>>,
    discord: Arc<RwLock<Option<DiscordPresence>>>,
    /// True while the main window is focused — drives UI event rate (games-friendly when false).
    ui_hot: Arc<AtomicBool>,
    /// True while a decode source is loaded. Position-poll sleeps long and skips
    /// main-thread hops when false (idle app / stopped).
    source_active: Arc<AtomicBool>,
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
                pitch_enabled: true,
                current_decode: 0,
                preloaded_decode: 0,
                cue_start: None,
                cue_end: None,
                cue_pos_relative: false,
                eq_context: Box::leak(Box::new(EqDspContext::new())),
                _plugin_handles: Vec::new(),
                gapless_queue: Vec::new(),
                gapless_queue_index: 0,
                pending_next: None,
                preloaded_source: 0,
                preloaded_audio_path: None,
                preloaded_track_path: None,
                pause_generation: 0,
                current_track_start_time: 0,
                user_paused: false,
                suppress_gapless_until: 0,
                mix_crossfade: None,
                mix_vol_follow: None,
                mix_preview_active: false,
            })),
            ops: Arc::new(Mutex::new(())),
            app: Arc::new(RwLock::new(None)),
            bass_thread: Arc::new(RwLock::new(None)),
            discord: Arc::new(RwLock::new(None)),
            ui_hot: Arc::new(AtomicBool::new(true)),
            source_active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Focused main window → hot UI updates; unfocused (game in foreground) → cold.
    pub fn set_ui_hot(&self, hot: bool) {
        self.ui_hot.store(hot, Ordering::Relaxed);
    }

    pub fn ui_is_hot(&self) -> bool {
        self.ui_hot.load(Ordering::Relaxed)
    }

    fn set_source_active(&self, active: bool) {
        self.source_active.store(active, Ordering::Release);
    }

    fn is_source_active(&self) -> bool {
        self.source_active.load(Ordering::Acquire)
    }

    fn cancel_pending_pause(inner: &mut PlayerInner) {
        inner.pause_generation = inner.pause_generation.wrapping_add(1);
        inner.user_paused = false;
        if let Some(bass) = inner.bass.as_ref() {
            if inner.mixer_handle != 0 {
                // Cancel the pause fade and stay silent until the new stream is ready.
                // Restoring volume here leaked ~0.5s of the previous track from the mixer buffer.
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
            }
        }
    }

    /// Drop buffered audio from the mixer output after removing sources.
    fn flush_mixer_hard(inner: &PlayerInner) {
        if let Some(bass) = inner.bass.as_ref() {
            if inner.mixer_handle != 0 {
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
                Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
            }
        }
    }

    /// Flush queued samples without leaving playback buffering disabled globally.
    fn restart_mixer_with_buffer(bass: &BassLibrary, mixer: u32) {
        let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_BUFFER, 0.0);
        let _ = bass.channel_play(mixer, true);
        let _ = bass.channel_set_attribute(
            mixer,
            bass::BASS_ATTRIB_BUFFER,
            MIXER_BUFFER_SECS,
        );
    }

    /// Start mixer volume from silence. Optional short slide prevents edge clicks
    /// after a hard mute + buffer restart (seek / manual track switch).
    fn set_mixer_volume_from_silence(
        bass: &BassLibrary,
        mixer: u32,
        volume: f32,
        fade_in_ms: u32,
    ) {
        if mixer == 0 {
            return;
        }
        let vol = volume.clamp(0.0, 1.0);
        // Cancels any in-flight VOL slide from pause/resume/previous seek.
        let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, 0.0);
        if vol <= 0.0001 || fade_in_ms == 0 {
            let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, vol);
            return;
        }
        let _ = bass.channel_slide_attribute(mixer, bass::BASS_ATTRIB_VOL, vol, fade_in_ms);
    }

    /// Soft-cut currently audible output before tearing sources down.
    /// Without this, VOL→0 or channel remove mid-waveform is a sharp click.
    fn fade_out_for_manual_switch(inner: &PlayerInner) {
        let Some(bass) = inner.bass.as_ref() else {
            return;
        };
        let mixer = inner.mixer_handle;
        if mixer == 0 {
            return;
        }
        // Already silent (paused after fade, or never started).
        if inner.user_paused || inner.current_source == 0 {
            let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, 0.0);
            return;
        }
        let current = bass
            .channel_get_attribute(mixer, bass::BASS_ATTRIB_VOL)
            .unwrap_or(inner.volume);
        if current <= 0.001 {
            let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, 0.0);
            return;
        }
        // Keep the mixer playing so BASS can process the slide on its update thread.
        if bass.channel_is_active(mixer) != bass::BASS_ACTIVE_PLAYING {
            let _ = bass.channel_play(mixer, false);
        }
        let _ = bass.channel_slide_attribute(
            mixer,
            bass::BASS_ATTRIB_VOL,
            0.0,
            MANUAL_SWITCH_FADE_OUT_MS,
        );
        // BASS slides on the audio thread; wait for the ramp (+ one update period).
        std::thread::sleep(Duration::from_millis(MANUAL_SWITCH_FADE_OUT_MS as u64 + 15));
        let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, 0.0);
    }

    fn begin_manual_track_switch(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) {
        if Self::can_gapless_reuse(inner, &playback.audio_path) {
            return;
        }

        if Self::can_use_preloaded(inner, track_path, &playback.audio_path) {
            if inner.current_source != 0 && inner.current_source != inner.preloaded_source {
                if let Some(bass) = inner.bass.as_ref() {
                    let _ = bass.mixer_channel_remove(inner.current_source);
                    Self::free_playback_channel(
                        bass,
                        inner.current_source,
                        inner.current_decode,
                    );
                }
                inner.current_source = 0;
                inner.current_decode = 0;
                inner.current_audio_path = None;
                inner.cue_start = None;
                inner.cue_end = None;
                inner.cue_pos_relative = false;
            }
        } else {
            Self::teardown_current(inner);
            Self::clear_preload(inner);
        }

        Self::flush_mixer_hard(inner);
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
        self.run_on_bass_thread(Self::init_inner)
    }

    /// Run a closure with the live `BassLibrary` on the BASS thread (for analysis etc.).
    pub fn with_bass<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&BassLibrary) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        self.run_on_bass_thread(move |inner| {
            let bass = inner
                .bass
                .as_ref()
                .ok_or_else(|| "BASS is not initialized".to_string())?;
            f(bass)
        })
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
                if bass.last_error() != bass::BassError::Already {
                    return Err(e);
                }
            }
        }

        // Device playback buffer. 300ms absorbs short main-thread / decode stalls
        // (Ctrl+R bootstrap, metadata, Discord) without audible dropouts. Was 200ms
        // and could dip into silence under load. Update period 15ms keeps latency OK.
        let _ = bass.set_config(bass::BASS_CONFIG_BUFFER, 300.0);
        let _ = bass.set_config(bass::BASS_CONFIG_UPDATEPERIOD, 15.0);

        let fx_dll = inner.bass_dir.join("bass_fx.dll");
        if fx_dll.is_file() {
            let path_str = fx_dll.to_string_lossy().to_string();
            // Register bass_fx as a BASS plugin (for format support).
            match bass.plugin_load(&path_str) {
                Ok(handle) => {
                    inner._plugin_handles.push(handle);
                }
                Err(error) => eprintln!("BASS FX plugin_load: {error}"),
            }
            // Load bass_fx.dll directly to resolve tempo FX entry points.
            if bass.enable_fx_from_plugin(&fx_dll) {
                eprintln!("BASS FX loaded (pitch-preserving tempo available)");
            } else {
                eprintln!("bass_fx.dll loaded but FX entry points were not found");
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
        let _ = bass.channel_set_attribute(
            mixer,
            bass::BASS_ATTRIB_BUFFER,
            MIXER_BUFFER_SECS,
        );
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

    pub fn prepare_next(
        &self,
        expected_current: Option<&str>,
        queue: Vec<GaplessTrack>,
    ) -> Result<(), String> {
        let expected_current = expected_current.map(str::to_string);
        self.run_on_bass_thread(move |inner| {
            // Mix Transition preview owns the device — don't let main library
            // gapless queue rebuilds stomp the two-deck session.
            if inner.mix_preview_active {
                return Ok(());
            }
            // Queue refreshes are asynchronous. A refresh created for track N must
            // never replace the queue after playback has already moved to N+1.
            if !Self::queue_refresh_matches(
                inner.current_file.as_deref(),
                expected_current.as_deref(),
                &queue,
            ) {
                return Ok(());
            }

            inner.gapless_queue = queue;
            inner.gapless_queue_index = 0;
            Self::refresh_pending_next(inner);
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
        let _ops = self.ops.lock();
        let track_path = track_path.to_string();
        let track_path_for_stats = track_path.clone();
        let audio_path = audio_path.map(str::to_string);
        let cleared_mix = self.run_on_bass_thread(move |inner| {
            let was_mix = inner.mix_preview_active;
            inner.mix_preview_active = false;
            // Install the queue/index only — do NOT preload the next track yet.
            // Preloading before the current segment is open is wrong for single-image CUE:
            // the next entry is the same audio file seeked to track 2's offset, and
            // play_inner's can_use_preloaded (audio path match) would activate that
            // source for track 1. With cue_start=0 activate_preloaded used to skip
            // re-seek, so the first track immediately looked "ended" and gapless
            // advanced to track 2. play_inner refreshes the preload after open.
            inner.gapless_queue = queue;
            inner.gapless_queue_index = 0;
            if let Some(index) = Self::find_gapless_queue_index(inner, &track_path) {
                inner.gapless_queue_index = index;
            }
            Self::play_inner(inner, &track_path, audio_path.as_deref(), cue_start, cue_end)?;
            inner.user_paused = false;
            // Record start time for this track (used to guard against early advance after manual que click)
            inner.current_track_start_time = Self::now_millis();
            // Manual play: don't treat residual BASS "ended" frames as gapless advance.
            inner.suppress_gapless_until = inner
                .current_track_start_time
                .saturating_add(MANUAL_SEGMENT_SUPPRESS_MS);
            Ok(was_mix)
        })?;
        if cleared_mix {
            self.emit_mix_preview_active(false);
        }
        // Wake the position poller out of idle sleep for gapless + seekbar.
        self.set_source_active(true);
        // Count after BASS open succeeds (manual play / remote / next-prev).
        self.record_play_stat(&track_path_for_stats);
        Ok(())
    }

    /// Persist foobar-style play count for `track_path` (best-effort).
    fn record_play_stat(&self, track_path: &str) {
        let path = track_path.trim();
        if path.is_empty() {
            return;
        }
        let Some(app) = self.app.read().clone() else {
            return;
        };
        if let Some(db) = app.try_state::<crate::playlists::LibraryDatabase>() {
            if let Err(e) = db.record_play(path) {
                eprintln!("[playback_stats] record_play failed: {e}");
            }
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
        let p = path.trim();
        // Windows extended-length prefix breaks naive equality with normal paths.
        let p = p
            .strip_prefix(r"\\?\")
            .or_else(|| p.strip_prefix("//?/"))
            .unwrap_or(p);
        #[cfg(windows)]
        {
            p.replace('/', "\\").to_lowercase()
        }
        #[cfg(not(windows))]
        {
            p.to_string()
        }
    }

    fn same_audio_path(a: &str, b: &str) -> bool {
        Self::audio_path_key(a) == Self::audio_path_key(b)
    }

    /// Virtual CUE paths (`file#cue:N`) and playlist paths — case/slash-normalized.
    fn same_track_path(a: &str, b: &str) -> bool {
        Self::same_audio_path(a, b)
    }

    fn cue_segments_are_contiguous(
        current_end: Option<f64>,
        next_start: Option<f64>,
    ) -> bool {
        let (Some(current_end), Some(next_start)) = (current_end, next_start) else {
            return false;
        };
        current_end.is_finite()
            && next_start.is_finite()
            && (current_end - next_start).abs() <= CUE_CONTIGUOUS_TOLERANCE_SECS
    }

    fn queue_refresh_matches(
        current: Option<&str>,
        expected: Option<&str>,
        queue: &[GaplessTrack],
    ) -> bool {
        let (Some(current), Some(expected), Some(first)) = (current, expected, queue.first()) else {
            return false;
        };
        Self::same_track_path(current, expected)
            && Self::same_track_path(&first.track_path, expected)
    }

    /// Locate the playing entry in the gapless queue.
    ///
    /// Never match by bare audio path alone when several CUE tracks share one image —
    /// that always resolved to index 0 and made gapless "jump" to the wrong track.
    fn find_gapless_queue_index(inner: &PlayerInner, track_path: &str) -> Option<usize> {
        if let Some(index) = inner
            .gapless_queue
            .iter()
            .position(|track| Self::same_track_path(&track.track_path, track_path))
        {
            return Some(index);
        }

        // Non-virtual path: allow a unique audio_path / track_path fallback.
        if cue::is_cue_track_path(track_path) {
            return None;
        }

        let mut matches = inner.gapless_queue.iter().enumerate().filter(|(_, track)| {
            Self::same_track_path(&track.track_path, track_path)
                || Self::same_audio_path(&track.audio_path, track_path)
        });
        let first = matches.next().map(|(i, _)| i)?;
        if matches.next().is_some() {
            None
        } else {
            Some(first)
        }
    }

    fn can_gapless_reuse(inner: &PlayerInner, audio_path: &str) -> bool {
        inner.current_source != 0
            && inner
                .current_audio_path
                .as_ref()
                .is_some_and(|current| Self::same_audio_path(current, audio_path))
    }

    /// Preload is only valid for the exact playlist/virtual track it was built for.
    /// Matching by audio path alone is wrong for single-image CUE albums (all tracks
    /// share one file) — it activated track N's preload while playing track 1 and
    /// immediately looked "ended".
    fn can_use_preloaded(inner: &PlayerInner, track_path: &str, _audio_path: &str) -> bool {
        if inner.preloaded_source == 0 {
            return false;
        }
        if let Some(preloaded_track) = inner.preloaded_track_path.as_ref() {
            return Self::same_track_path(preloaded_track, track_path);
        }
        // Legacy preload without track path: only safe for non-CUE unique files.
        if cue::is_cue_track_path(track_path) {
            return false;
        }
        inner
            .preloaded_audio_path
            .as_ref()
            .is_some_and(|p| Self::same_audio_path(p, track_path) || Self::same_audio_path(p, _audio_path))
    }

    /// AAC/ALAC in MP4 often has encoder delay at the start (~40–50ms). Skipping it
    /// on gapless *joins* removes a common hole between multi-file CUE tracks.
    fn gapless_join_start_secs(audio_path: &str, cue_start: Option<f64>) -> f64 {
        let start = cue_start.unwrap_or(0.0).max(0.0);
        if start > 0.001 {
            return start;
        }
        let ext = std::path::Path::new(audio_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if matches!(ext.as_str(), "m4a" | "mp4" | "aac" | "alac" | "m4b") {
            // ~2112 samples @ 44.1 kHz — typical AAC priming.
            return 0.048;
        }
        0.0
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
        inner.preloaded_track_path = None;
        inner.preloaded_decode = 0;
    }

    fn teardown_current(inner: &mut PlayerInner) {
        Self::clear_mix_crossfade(inner);
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
        inner.cue_pos_relative = false;
        inner.current_decode = 0;
    }

    fn emit_mix_preview_active(&self, active: bool) {
        if let Some(app) = self.app.read().clone() {
            #[derive(Clone, Serialize)]
            struct MixPreviewPayload {
                active: bool,
            }
            let _ = app.emit("player:mix-preview", MixPreviewPayload { active });
        }
    }

    fn detach_mix_deck_filter(bass: Option<&BassLibrary>, slot: Option<MixDeckFilter>) {
        let Some(slot) = slot else {
            return;
        };
        if let Some(bass) = bass {
            mix_filter::detach_mix_filter(bass, slot.source, slot.dsp, slot.ctx);
        }
        // else ctx dropped with slot
    }

    fn clear_mix_crossfade(inner: &mut PlayerInner) {
        if let Some(follow) = inner.mix_vol_follow.take() {
            Self::detach_mix_deck_filter(inner.bass.as_ref(), follow.filter);
        }
        let Some(mix) = inner.mix_crossfade.take() else {
            return;
        };
        if let Some(bass) = inner.bass.as_ref() {
            Self::detach_mix_deck_filter(Some(bass), mix.from_filter);
            Self::detach_mix_deck_filter(Some(bass), mix.to_filter);
            let mut pairs = vec![
                (mix.from_source, mix.from_decode),
                (mix.to_source, mix.to_decode),
            ];
            if let Some(p) = mix.pending_to {
                pairs.push((p.source, p.decode));
            }
            for (src, dec) in pairs {
                if src == 0 {
                    continue;
                }
                let _ = bass.mixer_channel_remove(src);
                Self::free_playback_channel(bass, src, dec);
            }
        } else {
            // Drop filter boxes without BASS (process exit).
            drop(mix.from_filter);
            drop(mix.to_filter);
        }
        if inner.current_source == mix.from_source || inner.current_source == mix.to_source {
            inner.current_source = 0;
            inner.current_decode = 0;
        }
    }

    fn attach_deck_filter(bass: &BassLibrary, source: u32) -> Option<MixDeckFilter> {
        if source == 0 {
            return None;
        }
        match mix_filter::attach_mix_filter(bass, source) {
            Ok((dsp, ctx)) => Some(MixDeckFilter { source, dsp, ctx }),
            Err(e) => {
                eprintln!("[mix] filter DSP attach failed: {e}");
                None
            }
        }
    }

    /// Sample gain 0..1 from automation segments at mix-timeline time `mix_secs`.
    /// Outside all segments → unity (1.0). Overlapping segments multiply.
    fn mix_gain_at(segments: &[MixVolSegment], mix_secs: f64) -> f32 {
        if segments.is_empty() {
            return 1.0;
        }
        let mut gain = 1.0f32;
        let mut any = false;
        for seg in segments {
            let dur = seg.duration_secs.max(1e-6);
            if mix_secs + 1e-4 < seg.start_secs {
                continue;
            }
            if mix_secs > seg.start_secs + dur + 1e-4 {
                continue;
            }
            let u = ((mix_secs - seg.start_secs) / dur).clamp(0.0, 1.0);
            gain *= Self::sample_mix_envelope(&seg.points, u, seg.curve);
            any = true;
        }
        if !any {
            1.0
        } else {
            gain.clamp(0.0, 1.0)
        }
    }

    /// Envelope v 0..1 → rate 0.5×..2× (v=0.5 → 1×). Matches frontend `envelopeVToRate`.
    fn envelope_v_to_rate(v: f32) -> f32 {
        let x = v.clamp(0.0, 1.0) as f64;
        (2f64.powf((x - 0.5) * 2.0) as f32).clamp(0.25, 2.0)
    }

    /// Speed multiplier from blocks (outside blocks → 1.0). Overlapping multiply.
    fn mix_speed_at(segments: &[MixVolSegment], mix_secs: f64) -> f32 {
        if segments.is_empty() {
            return 1.0;
        }
        let mut rate = 1.0f32;
        let mut any = false;
        for seg in segments {
            let dur = seg.duration_secs.max(1e-6);
            if mix_secs + 1e-4 < seg.start_secs {
                continue;
            }
            if mix_secs > seg.start_secs + dur + 1e-4 {
                continue;
            }
            let u = ((mix_secs - seg.start_secs) / dur).clamp(0.0, 1.0);
            let v = Self::sample_mix_envelope(&seg.points, u, seg.curve);
            rate *= Self::envelope_v_to_rate(v);
            any = true;
        }
        if !any {
            1.0
        } else {
            rate.clamp(0.25, 2.0)
        }
    }

    fn apply_mix_deck_rate(
        bass: &BassLibrary,
        source: u32,
        decode: u32,
        rate: f32,
        _pitch_enabled: bool,
    ) {
        if source == 0 {
            return;
        }
        let r = rate.clamp(0.25, 2.0);
        // Match normal transport: tempo wrapper → TEMPO %; plain decode → FREQ.
        // Always target the decode handle for FREQ (never the mixer wrapper).
        if Self::is_tempo_wrapped(bass, source, decode) {
            let _ = Self::slide_tempo_pct(bass, source, r, 0);
        } else {
            let handle = if decode != 0 { decode } else { source };
            Self::apply_freq_rate(bass, handle, r);
        }
    }

    fn catmull_rom(p0: f64, p1: f64, p2: f64, p3: f64, u: f64) -> f64 {
        let u2 = u * u;
        let u3 = u2 * u;
        0.5 * (2.0 * p1
            + (-p0 + p2) * u
            + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u2
            + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * u3)
    }

    fn sample_mix_envelope(points: &[MixVolPoint], t: f64, curve: MixEnvCurve) -> f32 {
        if points.is_empty() {
            return 1.0;
        }
        let x = t.clamp(0.0, 1.0);
        // Points expected sorted; tolerate unsorted.
        let mut pts: Vec<(f64, f64)> = points
            .iter()
            .map(|p| (p.t.clamp(0.0, 1.0), p.v.clamp(0.0, 1.0)))
            .collect();
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        if x <= pts[0].0 {
            return pts[0].1 as f32;
        }
        for i in 1..pts.len() {
            let (t0, v0) = pts[i - 1];
            let (t1, v1) = pts[i];
            if x <= t1 + 1e-12 {
                let span = (t1 - t0).max(1e-9);
                let u = (x - t0) / span;
                if curve == MixEnvCurve::Smooth {
                    let p0 = pts[i.saturating_sub(2)].1;
                    let p1 = v0;
                    let p2 = v1;
                    let p3 = pts[(i + 1).min(pts.len() - 1)].1;
                    return Self::catmull_rom(p0, p1, p2, p3, u).clamp(0.0, 1.0) as f32;
                }
                return (v0 + (v1 - v0) * u) as f32;
            }
        }
        pts[pts.len() - 1].1 as f32
    }

    /// Active filter cutoff at mix time. LP → min of active segments; HP → max.
    /// `None` = bypass (no block covers this time).
    fn mix_filter_hz_at(
        segments: &[MixVolSegment],
        mix_secs: f64,
        map_v: fn(f64) -> f64,
        prefer_min: bool,
    ) -> Option<f32> {
        if segments.is_empty() {
            return None;
        }
        let mut any = false;
        let mut val = if prefer_min { f64::MAX } else { 0.0 };
        for seg in segments {
            let dur = seg.duration_secs.max(1e-6);
            if mix_secs + 1e-4 < seg.start_secs {
                continue;
            }
            if mix_secs > seg.start_secs + dur + 1e-4 {
                continue;
            }
            let u = ((mix_secs - seg.start_secs) / dur).clamp(0.0, 1.0);
            let v = Self::sample_mix_envelope(&seg.points, u, seg.curve) as f64;
            let hz = map_v(v);
            any = true;
            if prefer_min {
                val = val.min(hz);
            } else {
                val = val.max(hz);
            }
        }
        if any {
            Some(val as f32)
        } else {
            None
        }
    }

    /// Drive per-deck volume + LP/HP + speed from mix-timeline automation blocks.
    fn apply_mix_volume_automation(inner: &mut PlayerInner) {
        if inner.bass.is_none() {
            return;
        }

        if inner.mix_crossfade.is_some() {
            let (
                from_src,
                from_dec,
                to_src,
                to_dec,
                from_g,
                to_g,
                from_lp,
                from_hp,
                to_lp,
                to_hp,
                from_rate,
                to_rate,
                pitch,
            ) = {
                let mix = inner.mix_crossfade.as_ref().unwrap();
                let bass = inner.bass.as_ref().unwrap();
                let now = Self::now_millis();
                // Includes silence-gap wall clock after prev ends early.
                let mix_secs = Self::mix_timeline_secs(mix, bass, now);
                let base = inner.playback_rate.clamp(0.25, 2.0);
                let from_g = Self::mix_gain_at(&mix.from_vol, mix_secs);
                let to_g = Self::mix_gain_at(&mix.to_vol, mix_secs);
                let from_lp = Self::mix_filter_hz_at(
                    &mix.from_lp,
                    mix_secs,
                    mix_filter::envelope_v_to_lp_hz,
                    true,
                );
                let from_hp = Self::mix_filter_hz_at(
                    &mix.from_hp,
                    mix_secs,
                    mix_filter::envelope_v_to_hp_hz,
                    false,
                );
                let to_lp = Self::mix_filter_hz_at(
                    &mix.to_lp,
                    mix_secs,
                    mix_filter::envelope_v_to_lp_hz,
                    true,
                );
                let to_hp = Self::mix_filter_hz_at(
                    &mix.to_hp,
                    mix_secs,
                    mix_filter::envelope_v_to_hp_hz,
                    false,
                );
                let from_rate = base * Self::mix_speed_at(&mix.from_speed, mix_secs);
                let to_rate = base * Self::mix_speed_at(&mix.to_speed, mix_secs);
                (
                    mix.from_source,
                    mix.from_decode,
                    mix.to_source,
                    mix.to_decode,
                    from_g,
                    to_g,
                    from_lp,
                    from_hp,
                    to_lp,
                    to_hp,
                    from_rate,
                    to_rate,
                    inner.pitch_enabled,
                )
            };
            let bass = inner.bass.as_ref().unwrap();
            if from_src != 0 {
                let _ = bass.channel_set_attribute(from_src, bass::BASS_ATTRIB_VOL, from_g);
                Self::apply_mix_deck_rate(bass, from_src, from_dec, from_rate, pitch);
            }
            if to_src != 0 {
                let _ = bass.channel_set_attribute(to_src, bass::BASS_ATTRIB_VOL, to_g);
                Self::apply_mix_deck_rate(bass, to_src, to_dec, to_rate, pitch);
            }
            if let Some(mix) = inner.mix_crossfade.as_ref() {
                if let Some(f) = mix.from_filter.as_ref() {
                    f.ctx.set_targets(from_lp, from_hp);
                }
                if let Some(f) = mix.to_filter.as_ref() {
                    f.ctx.set_targets(to_lp, to_hp);
                }
            }
            return;
        }

        if let Some(follow) = inner.mix_vol_follow.as_ref() {
            if follow.source == 0 {
                return;
            }
            let bass = inner.bass.as_ref().unwrap();
            let abs =
                Self::content_absolute_secs(bass, follow.source, follow.decode, true);
            let mix_secs =
                follow.to_graph_delay_secs + (abs - follow.to_cue_start).max(0.0);
            let g = Self::mix_gain_at(&follow.to_vol, mix_secs);
            let lp = Self::mix_filter_hz_at(
                &follow.to_lp,
                mix_secs,
                mix_filter::envelope_v_to_lp_hz,
                true,
            );
            let hp = Self::mix_filter_hz_at(
                &follow.to_hp,
                mix_secs,
                mix_filter::envelope_v_to_hp_hz,
                false,
            );
            let base = inner.playback_rate.clamp(0.25, 2.0);
            let rate = base * Self::mix_speed_at(&follow.to_speed, mix_secs);
            let src = follow.source;
            let dec = follow.decode;
            let pitch = inner.pitch_enabled;
            let _ = bass.channel_set_attribute(src, bass::BASS_ATTRIB_VOL, g);
            Self::apply_mix_deck_rate(bass, src, dec, rate, pitch);
            if let Some(f) = follow.filter.as_ref() {
                f.ctx.set_targets(lp, hp);
            }
        }
    }

    fn inject_pending_to_deck(inner: &mut PlayerInner) {
        let (pending, to_cue) = {
            let Some(mix) = inner.mix_crossfade.as_mut() else {
                return;
            };
            (mix.pending_to.take(), mix.next_cue_start.unwrap_or(0.0).max(0.0))
        };
        let Some(pending) = pending else {
            return;
        };
        if pending.source == 0 {
            return;
        }
        // Park at the exact graph time before the mixer eats samples.
        if let Some(bass) = inner.bass.as_ref() {
            Self::seek_content_absolute(bass, pending.source, pending.decode, false, to_cue);
        }
        if Self::add_source_to_mixer(inner, pending.source, true).is_ok() {
            // Gain at inject time (mix clock ≈ to_delay).
            let g = inner
                .mix_crossfade
                .as_ref()
                .map(|m| {
                    let t = m.to_graph_delay_secs.max(m.to_delay_secs);
                    Self::mix_gain_at(&m.to_vol, t)
                })
                .unwrap_or(1.0);
            let need_filter = inner.mix_crossfade.as_ref().is_some_and(|m| {
                !m.to_lp.is_empty() || !m.to_hp.is_empty()
            });
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_set_attribute(pending.source, bass::BASS_ATTRIB_VOL, g);
                // Re-snap after plug-in (mixer may have advanced a buffer).
                Self::seek_content_absolute(bass, pending.source, pending.decode, true, to_cue);
            }
            let filter = if need_filter {
                inner
                    .bass
                    .as_ref()
                    .and_then(|b| Self::attach_deck_filter(b, pending.source))
            } else {
                None
            };
            if let Some(mix) = inner.mix_crossfade.as_mut() {
                // Drop any stale to_filter from a prior inject.
                if let Some(old) = mix.to_filter.take() {
                    if let Some(bass) = inner.bass.as_ref() {
                        mix_filter::detach_mix_filter(bass, old.source, old.dsp, old.ctx);
                    }
                }
                mix.to_source = pending.source;
                mix.to_decode = pending.decode;
                mix.to_filter = filter;
            }
        } else if let Some(bass) = inner.bass.as_ref() {
            Self::free_playback_channel(bass, pending.source, pending.decode);
        }
    }

    /// Mix-timeline seconds since preview start = **wall clock** (1× graph / playhead).
    ///
    /// Speed blocks retune audio only so one deck can match the other; automation
    /// and inject delays stay locked to the layout clock, not content time
    /// (which would race ahead when rate ≠ 1).
    fn mix_timeline_secs(mix: &MixCrossfadeState, _bass: &BassLibrary, now_ms: u64) -> f64 {
        (now_ms.saturating_sub(mix.mix_timeline_start_ms) as f64 / 1000.0).max(0.0)
    }

    /// Drop previous only; keep pending next so a graph gap (silence) can pass.
    fn drop_mix_from_only(inner: &mut PlayerInner, mix_secs_at_drop: f64) {
        let Some(mix) = inner.mix_crossfade.as_mut() else {
            return;
        };
        if mix.from_source == 0 {
            return;
        }
        let from_src = mix.from_source;
        let from_dec = mix.from_decode;
        let from_filter = mix.from_filter.take();
        mix.from_source = 0;
        mix.from_decode = 0;
        // Record wall mix-clock at drop (for diagnostics / future use).
        mix.from_ended_mix_secs = mix_secs_at_drop.max(0.0);
        mix.from_ended_at_ms = Self::now_millis();

        Self::detach_mix_deck_filter(inner.bass.as_ref(), from_filter);
        if let Some(bass) = inner.bass.as_ref() {
            let _ = bass.mixer_channel_remove(from_src);
            Self::free_playback_channel(bass, from_src, from_dec);
        }
        if inner.current_source == from_src {
            // Silence until next inject — mixer stays NONSTOP.
            if mix.to_source != 0 {
                inner.current_source = mix.to_source;
                inner.current_decode = mix.to_decode;
            } else {
                inner.current_source = 0;
                inner.current_decode = 0;
            }
        }
    }

    /// Previous deck finished: drop it, keep next if already playing.
    /// Does NOT force-inject a pending next before its graph delay — that would
    /// kill intentional pauses between tracks on the timeline.
    /// Returns the surviving next path when handoff succeeds.
    fn finish_mix_from_deck(inner: &mut PlayerInner) -> Option<String> {
        // Only inject pending if its graph time has already arrived.
        let should_inject = inner.mix_crossfade.as_ref().is_some_and(|m| {
            m.pending_to.is_some() && {
                let now = Self::now_millis();
                let bass = inner.bass.as_ref();
                let secs = bass
                    .map(|b| Self::mix_timeline_secs(m, b, now))
                    .unwrap_or(m.to_graph_delay_secs);
                secs + 0.002 >= m.to_graph_delay_secs
            }
        });
        if should_inject {
            Self::inject_pending_to_deck(inner);
        }
        let mut mix = inner.mix_crossfade.take()?;

        // Detach prev filter before freeing the channel.
        let from_filter = mix.from_filter.take();
        Self::detach_mix_deck_filter(inner.bass.as_ref(), from_filter);

        if let Some(bass) = inner.bass.as_ref() {
            if mix.from_source != 0 {
                let _ = bass.mixer_channel_remove(mix.from_source);
                Self::free_playback_channel(bass, mix.from_source, mix.from_decode);
            }
        }
        mix.from_source = 0;
        mix.from_decode = 0;

        // Still waiting for next's graph time — put mix state back without from.
        if mix.pending_to.is_some() && mix.to_source == 0 {
            if mix.from_ended_at_ms == 0 {
                mix.from_ended_mix_secs = mix.from_duration_secs.max(0.0);
                mix.from_ended_at_ms = Self::now_millis();
            }
            inner.current_source = 0;
            inner.current_decode = 0;
            inner.mix_crossfade = Some(mix);
            return None;
        }

        if mix.to_source != 0 {
            let to_src = mix.to_source;
            let to_dec = mix.to_decode;
            let to_live = inner.bass.as_ref().is_some_and(|bass| {
                bass.channel_is_active(to_src) != bass::BASS_ACTIVE_STOPPED
            });
            if to_live {
                let next_path = mix.next_path;
                // Prefer the still-playing next deck as the sole current source so
                // get_state / end detection no longer key off the dead previous.
                inner.current_source = to_src;
                inner.current_decode = to_dec;
                inner.current_file = Some(next_path.clone());
                inner.current_audio_path = Some(mix.next_audio_path.clone());
                inner.cue_start = mix.next_cue_start;
                inner.cue_end = mix.next_cue_end;
                inner.gapless_queue = vec![GaplessTrack {
                    track_path: next_path.clone(),
                    audio_path: mix.next_audio_path,
                    cue_start: mix.next_cue_start,
                    cue_end: mix.next_cue_end,
                }];
                inner.gapless_queue_index = 0;
                // Keep automation on next until the track ends.
                let keep_fx = !mix.to_vol.is_empty()
                    || !mix.to_lp.is_empty()
                    || !mix.to_hp.is_empty()
                    || !mix.to_speed.is_empty()
                    || mix.to_filter.is_some();
                if keep_fx {
                    inner.mix_vol_follow = Some(MixVolFollow {
                        source: to_src,
                        decode: to_dec,
                        to_graph_delay_secs: mix.to_graph_delay_secs,
                        to_cue_start: mix.next_cue_start.unwrap_or(0.0).max(0.0),
                        to_vol: mix.to_vol,
                        to_lp: mix.to_lp,
                        to_hp: mix.to_hp,
                        to_speed: mix.to_speed,
                        filter: mix.to_filter,
                    });
                } else {
                    Self::detach_mix_deck_filter(inner.bass.as_ref(), mix.to_filter);
                    inner.mix_vol_follow = None;
                }
                // Fresh start clock so the next deck isn't treated as "already
                // past the spurious-end guard" from the mix open time.
                let now = Self::now_millis();
                inner.current_track_start_time = now;
                inner.suppress_gapless_until = now.saturating_add(MANUAL_SEGMENT_SUPPRESS_MS);
                Self::detect_cue_position_mode(inner);
                return Some(next_path);
            }
            // Next already finished — free it and fall through to full stop.
            Self::detach_mix_deck_filter(inner.bass.as_ref(), mix.to_filter);
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.mixer_channel_remove(to_src);
                Self::free_playback_channel(bass, to_src, to_dec);
            }
        }

        // Free pending if still held (shouldn't after inject).
        if let Some(p) = mix.pending_to {
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.mixer_channel_remove(p.source);
                Self::free_playback_channel(bass, p.source, p.decode);
            }
        }
        inner.mix_vol_follow = None;
        inner.current_source = 0;
        inner.current_decode = 0;
        None
    }

    /// True if the mix session is still live (playing or intentional silence gap).
    fn mix_has_active_audio(inner: &PlayerInner) -> bool {
        let Some(mix) = inner.mix_crossfade.as_ref() else {
            return false;
        };
        // Waiting for delayed next = session still active (silence is intentional).
        if mix.pending_to.is_some() {
            return true;
        }
        let Some(bass) = inner.bass.as_ref() else {
            return false;
        };
        let live = |src: u32| {
            src != 0 && bass.channel_is_active(src) != bass::BASS_ACTIVE_STOPPED
        };
        live(mix.from_source) || live(mix.to_source)
    }

    /// Two-deck mix: play as laid out on the timeline.
    ///
    /// - `to_delay_secs > 0`: next is later on the mix timeline — start it after that delay at `to_cue_start`.
    /// - `from_duration_secs`: how long previous keeps playing from its cue start (to end of track).
    /// - `from_vol` / `to_vol`: optional gain envelopes (mix timeline from preview start).
    /// - `from_lp` / `from_hp` / `to_lp` / `to_hp`: cutoff envelopes (normalized v → Hz).
    pub fn play_mix_crossfade(
        &self,
        from_path: &str,
        from_audio: Option<&str>,
        from_cue_start: Option<f64>,
        from_cue_end: Option<f64>,
        to_path: &str,
        to_audio: Option<&str>,
        to_cue_start: Option<f64>,
        to_cue_end: Option<f64>,
        // Delay before next deck (later on mix timeline). from_duration: how long prev runs.
        to_delay_secs: f64,
        from_duration_secs: f64,
        from_vol: Vec<MixVolSegment>,
        to_vol: Vec<MixVolSegment>,
        from_lp: Vec<MixVolSegment>,
        from_hp: Vec<MixVolSegment>,
        to_lp: Vec<MixVolSegment>,
        to_hp: Vec<MixVolSegment>,
        from_speed: Vec<MixVolSegment>,
        to_speed: Vec<MixVolSegment>,
    ) -> Result<(), String> {
        let _ops = self.ops.lock();
        let from_path_owned = from_path.to_string();
        let to_path = to_path.to_string();
        let from_audio = from_audio.map(str::to_string);
        let to_audio = to_audio.map(str::to_string);
        let to_delay = to_delay_secs.max(0.0);
        let from_dur = from_duration_secs.max(0.0);
        let from_path_for_stats = from_path_owned.clone();
        // Frontend passes cue_start only when that deck should play from t=0 of preview.
        let enable_from = from_cue_start.is_some();
        let enable_to = to_cue_start.is_some() || to_delay > 0.0;

        self.run_on_bass_thread(move |inner| {
            let from_path = from_path_owned;
            let from_pb = if enable_from {
                Some(cue::resolve_playback(
                    &from_path,
                    from_audio.as_deref(),
                    from_cue_start,
                    from_cue_end,
                )?)
            } else {
                None
            };
            let to_pb = if enable_to {
                Some(cue::resolve_playback(
                    &to_path,
                    to_audio.as_deref(),
                    // When delayed, start next at the beginning of its audible range
                    // (caller already set cue_start to 0 or the mapped in-point).
                    to_cue_start,
                    to_cue_end,
                )?)
            } else {
                None
            };

            inner.gapless_queue.clear();
            inner.gapless_queue_index = 0;
            inner.pending_next = None;
            Self::cancel_pending_pause(inner);
            Self::clear_preload(inner);
            Self::clear_mix_crossfade(inner);
            inner.mix_vol_follow = None;
            if inner.current_source != 0 {
                if let Some(bass) = inner.bass.as_ref() {
                    let _ = bass.mixer_channel_remove(inner.current_source);
                    Self::free_playback_channel(
                        bass,
                        inner.current_source,
                        inner.current_decode,
                    );
                }
                inner.current_source = 0;
                inner.current_decode = 0;
            }
            inner.current_file = None;
            inner.current_audio_path = None;
            inner.cue_start = None;
            inner.cue_end = None;
            inner.cue_pos_relative = false;

            // Keep mixer STOPPED while we open both decks — if we restart/play
            // early, prev starts alone and next always sounds late vs the graph.
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    let _ =
                        bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
                    let _ = bass.channel_pause(inner.mixer_handle);
                    let _ = bass.channel_set_attribute(
                        inner.mixer_handle,
                        bass::BASS_ATTRIB_BUFFER,
                        0.0,
                    );
                }
            }

            let rate = inner.playback_rate;
            let pitch = inner.pitch_enabled;
            let volume = inner.volume;
            let now = Self::now_millis();

            // 1) Open + wrap BOTH sources fully (no mixer yet).
            let mut from_src = 0u32;
            let mut from_tracked = 0u32;
            let mut from_start = 0.0f64;
            if let Some(ref pb) = from_pb {
                from_start = pb.cue_start.unwrap_or(0.0).max(0.0);
                let decode = Self::create_decode_source(inner, &pb.audio_path, pb.cue_start)?;
                let (src, tracked) = {
                    let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
                    Self::wrap_decode_for_rate(bass, decode, rate, pitch)?
                };
                // Re-seek after tempo wrap so both decks share a clean start point.
                if let Some(bass) = inner.bass.as_ref() {
                    Self::seek_content_absolute(bass, src, tracked, false, from_start);
                }
                from_src = src;
                from_tracked = tracked;
                Self::apply_segment_metadata(inner, &from_path, pb);
            }

            let mut to_src = 0u32;
            let mut to_tracked = 0u32;
            let mut pending_to = None;
            let mut to_start = 0.0f64;
            if let Some(ref pb) = to_pb {
                to_start = pb.cue_start.unwrap_or(0.0).max(0.0);
                let decode = Self::create_decode_source(inner, &pb.audio_path, pb.cue_start)?;
                let (src, tracked) = {
                    let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
                    Self::wrap_decode_for_rate(bass, decode, rate, pitch)?
                };
                if let Some(bass) = inner.bass.as_ref() {
                    Self::seek_content_absolute(bass, src, tracked, false, to_start);
                }
                if to_delay <= 0.001 {
                    to_src = src;
                    to_tracked = tracked;
                } else {
                    pending_to = Some(PendingMixDeck {
                        source: src,
                        decode: tracked,
                    });
                }
            }

            // Initial gains at mix t=0 (envelope start).
            let from_g0 = Self::mix_gain_at(&from_vol, 0.0);
            let to_g0 = Self::mix_gain_at(&to_vol, 0.0);

            // 2) Add active decks while mixer is still paused, then one synchronized start.
            if from_src != 0 {
                Self::add_source_to_mixer(inner, from_src, true)?;
            }
            if to_src != 0 {
                Self::add_source_to_mixer(inner, to_src, true)?;
            }
            {
                let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
                if from_src != 0 {
                    let _ = bass.channel_set_attribute(from_src, bass::BASS_ATTRIB_VOL, from_g0);
                    Self::seek_content_absolute(bass, from_src, from_tracked, true, from_start);
                }
                if to_src != 0 {
                    let _ = bass.channel_set_attribute(to_src, bass::BASS_ATTRIB_VOL, to_g0);
                    Self::seek_content_absolute(bass, to_src, to_tracked, true, to_start);
                }
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, volume);
                // restart=true: both plugged sources begin the same output clock tick.
                let _ = bass.channel_play(inner.mixer_handle, true);
            }

            // UI position follows previous while it lives, else next.
            if from_src != 0 {
                inner.current_source = from_src;
                inner.current_decode = from_tracked;
            } else if to_src != 0 {
                inner.current_source = to_src;
                inner.current_decode = to_tracked;
                if let Some(ref pb) = to_pb {
                    Self::apply_segment_metadata(inner, &to_path, pb);
                }
            }

            inner.applied_playback_rate = rate;
            Self::detect_cue_position_mode(inner);
            inner.user_paused = false;
            inner.current_track_start_time = now;
            inner.suppress_gapless_until = now.saturating_add(MANUAL_SEGMENT_SUPPRESS_MS);

            let (next_audio, next_cs, next_ce) = to_pb
                .as_ref()
                .map(|p| (p.audio_path.clone(), p.cue_start, p.cue_end))
                .unwrap_or_default();

            // Graph delay of next is always the original to_delay (even if already injected).
            let graph_delay = if from_src != 0 { to_delay } else { 0.0 };

            // Attach LP/HP DSP only when that deck has filter blocks.
            let from_filter = if from_src != 0 && (!from_lp.is_empty() || !from_hp.is_empty()) {
                inner
                    .bass
                    .as_ref()
                    .and_then(|b| Self::attach_deck_filter(b, from_src))
            } else {
                None
            };
            let to_filter = if to_src != 0 && (!to_lp.is_empty() || !to_hp.is_empty()) {
                inner
                    .bass
                    .as_ref()
                    .and_then(|b| Self::attach_deck_filter(b, to_src))
            } else {
                None
            };
            // Seed cutoffs at t=0 before first poll.
            if let Some(ref f) = from_filter {
                f.ctx.set_targets(
                    Self::mix_filter_hz_at(&from_lp, 0.0, mix_filter::envelope_v_to_lp_hz, true),
                    Self::mix_filter_hz_at(&from_hp, 0.0, mix_filter::envelope_v_to_hp_hz, false),
                );
            }
            if let Some(ref f) = to_filter {
                f.ctx.set_targets(
                    Self::mix_filter_hz_at(&to_lp, 0.0, mix_filter::envelope_v_to_lp_hz, true),
                    Self::mix_filter_hz_at(&to_hp, 0.0, mix_filter::envelope_v_to_hp_hz, false),
                );
            }

            inner.mix_crossfade = Some(MixCrossfadeState {
                from_source: from_src,
                from_decode: from_tracked,
                to_source: to_src,
                to_decode: to_tracked,
                // Drop previous after this much *audio* time (not wall clock).
                from_duration_secs: if from_src != 0 { from_dur } else { 0.0 },
                // Use prev deck audio time for inject (not wall clock) so next
                // lands on the same mix timeline as the waveform.
                to_delay_secs: if pending_to.is_some() { to_delay } else { 0.0 },
                to_graph_delay_secs: graph_delay,
                from_cue_start: from_start,
                mix_timeline_start_ms: now,
                from_ended_mix_secs: 0.0,
                from_ended_at_ms: 0,
                pending_to,
                next_path: to_path.clone(),
                next_audio_path: next_audio,
                next_cue_start: next_cs,
                next_cue_end: next_ce,
                from_vol,
                to_vol,
                from_lp,
                from_hp,
                to_lp,
                to_hp,
                from_speed,
                to_speed,
                from_filter,
                to_filter,
            });
            // Only-to preview: mix_crossfade owns vol + filters for the whole session.
            inner.gapless_queue.clear();
            inner.gapless_queue_index = 0;

            // Seed vol / filter / speed automation at t=0 (don't wait for first poll).
            Self::apply_mix_volume_automation(inner);

            inner.mix_preview_active = true;
            Ok(())
        })?;
        self.emit_mix_preview_active(true);
        self.set_source_active(true);
        self.record_play_stat(&from_path_for_stats);
        Ok(())
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

    /// Map a raw BASS position read onto the absolute file timeline (CUE INDEX space).
    fn content_timeline_secs(inner: &PlayerInner, reported_secs: f64) -> f64 {
        let start = inner.cue_start.unwrap_or(0.0);
        let reported = reported_secs.max(0.0);
        // Prefer the flag when set; also recover if detection lagged after a seek.
        if inner.cue_pos_relative {
            return reported + start;
        }
        if start > 0.05 && reported + 0.35 < start {
            // Looks segment-relative even if the flag is stale.
            return reported + start;
        }
        reported
    }

    /// After open/seek to a CUE segment, detect whether BASS reports absolute file
    /// time or 0-based time from the seek point (common with mixer+decode).
    fn detect_cue_position_mode(inner: &mut PlayerInner) {
        let start = inner.cue_start.unwrap_or(0.0);
        if start <= 0.05 || inner.current_source == 0 {
            inner.cue_pos_relative = false;
            return;
        }
        let Some(bass) = inner.bass.as_ref() else {
            inner.cue_pos_relative = false;
            return;
        };
        // Prefer decode-handle read (true content timeline).
        let reported = Self::content_absolute_secs(
            bass,
            inner.current_source,
            inner.current_decode,
            true,
        );
        // Seek target was `start`; if we read ~0, channel is segment-relative.
        // Allow a bit of slack for encoder-delay skips (~50ms) and mix latency.
        inner.cue_pos_relative = reported + 0.75 < start;
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
        let target = playback.cue_start.unwrap_or(0.0).max(0.0);
        {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            // Content-timeline seek (handles tempo wrappers correctly).
            Self::seek_content_absolute(
                bass,
                source,
                inner.current_decode,
                true,
                target,
            );
            // Make sure mixer is running
            if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
                let _ = bass.channel_play(inner.mixer_handle, false);
            }
        }
        Self::apply_segment_metadata(inner, track_path, playback);
        Self::detect_cue_position_mode(inner);
        // Manual CUE seeks can briefly report the old absolute position → false end.
        inner.suppress_gapless_until = Self::now_millis().saturating_add(MANUAL_SEGMENT_SUPPRESS_MS);
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

    /// Plug a decode/tempo channel into the mixer.
    /// `norampin = true` for gapless joins (instant); `false` lets BASS ramp the channel
    /// in (manual track switches — less clicky with non-zero first samples).
    fn add_source_to_mixer(
        inner: &mut PlayerInner,
        source: u32,
        norampin: bool,
    ) -> Result<(), String> {
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        if inner.mixer_handle == 0 {
            return Err("Mixer not created".to_string());
        }
        // No AUTOFREE: rate/pitch rebuilds re-add the channel; AUTOFREE would free decode
        // and false-trigger end detection.
        let add_flags = if norampin {
            bass::BASS_MIXER_CHAN_NORAMPIN
        } else {
            0
        };
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

        let decode = Self::create_decode_source(inner, &playback.audio_path, playback.cue_start)?;
        let rate = inner.playback_rate;
        let pitch_enabled = inner.pitch_enabled;
        let volume = inner.volume;
        let (mixer_channel, tracked_decode) = {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Self::wrap_decode_for_rate(bass, decode, rate, pitch_enabled)?
        };

        // Soft channel ramp — manual open only (gapless uses activate_preloaded).
        Self::add_source_to_mixer(inner, mixer_channel, false)?;

        if let Some(bass) = inner.bass.as_ref() {
            // Stay silent through the flush, then ramp in — hard VOL restore after
            // restart is the classic manual-switch click.
            let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
            Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
            Self::set_mixer_volume_from_silence(
                bass,
                inner.mixer_handle,
                volume,
                MANUAL_SWITCH_FADE_IN_MS,
            );
        }

        inner.current_source = mixer_channel;
        inner.current_decode = tracked_decode;
        inner.applied_playback_rate = rate;
        Self::apply_segment_metadata(inner, track_path, playback);
        Self::detect_cue_position_mode(inner);

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
            // Same-image CUE transitions reuse the active source. Do not leave a
            // preload from an older queue around after drag-reordering.
            Self::clear_preload(inner);
            return Ok(());
        }

        if Self::can_use_preloaded(inner, &next.track_path, &playback.audio_path) {
            return Ok(());
        }

        Self::clear_preload(inner);

        // For gapless joins into AAC/M4A, skip encoder delay so the first samples
        // aren't silent padding (big contributor to multi-file CUE holes).
        let join_start =
            Self::gapless_join_start_secs(&playback.audio_path, playback.cue_start);
        let source =
            Self::create_decode_source(inner, &playback.audio_path, Some(join_start))?;

        // Decode+position only — not in the mixer until the cut.
        let rate = inner.playback_rate;
        let pitch_enabled = inner.pitch_enabled;
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let (mixer_channel, tracked_decode) =
            Self::wrap_decode_for_rate(bass, source, rate, pitch_enabled)?;

        inner.preloaded_source = mixer_channel;
        inner.preloaded_decode = tracked_decode;
        inner.preloaded_audio_path = Some(playback.audio_path);
        inner.preloaded_track_path = Some(next.track_path.clone());
        Ok(())
    }

    /// Open next file and swap into the mixer without a full open_stream hard-restart.
    /// Used when preload missed (first join) so WAV/CUE still gapless-ish.
    fn open_next_seamless(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) -> Result<(), String> {
        let join_start =
            Self::gapless_join_start_secs(&playback.audio_path, playback.cue_start);
        let decode =
            Self::create_decode_source(inner, &playback.audio_path, Some(join_start))?;
        let rate = inner.playback_rate;
        let pitch_enabled = inner.pitch_enabled;
        let (mixer_channel, tracked_decode) = {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Self::wrap_decode_for_rate(bass, decode, rate, pitch_enabled)?
        };

        let old_source = inner.current_source;
        let old_decode = inner.current_decode;

        // Gapless: NORAMPIN for seamless file→file joins.
        Self::add_source_to_mixer(inner, mixer_channel, true)?;

        if let Some(bass) = inner.bass.as_ref() {
            if old_source != 0 && old_source != mixer_channel {
                let _ = bass.mixer_channel_remove(old_source);
                Self::free_playback_channel(bass, old_source, old_decode);
            }
            // Leave mixer volume alone: gapless must not re-slam VOL (clicks).
            // Manual callers re-apply volume with a short fade after this.
            if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
                let _ = bass.channel_play(inner.mixer_handle, false);
            }
        }

        inner.current_source = mixer_channel;
        inner.current_decode = tracked_decode;
        inner.applied_playback_rate = rate;
        Self::apply_segment_metadata(inner, track_path, playback);
        Self::detect_cue_position_mode(inner);
        Ok(())
    }

    /// Gapless file→file join: add preloaded next, drop current, never restart mixer.
    fn activate_preloaded(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) -> Result<(), String> {
        let preloaded = inner.preloaded_source;
        let matches = Self::can_use_preloaded(inner, track_path, &playback.audio_path);

        if preloaded == 0 || !matches {
            // First join often has no warm preload — still avoid open_stream hard restart.
            return Self::open_next_seamless(inner, track_path, playback);
        }

        let old_source = inner.current_source;
        let old_decode = inner.current_decode;
        let start =
            Self::gapless_join_start_secs(&playback.audio_path, playback.cue_start);
        let preloaded_decode = inner.preloaded_decode;

        // Snap to segment start before plugging in (content timeline).
        {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Self::seek_content_absolute(bass, preloaded, preloaded_decode, false, start.max(0.0));
        }

        // Order: add next (playing) → remove old. Mixer stays running (no play restart).
        // NORAMPIN: gapless must not ramp; manual path fades mixer VOL around this.
        Self::add_source_to_mixer(inner, preloaded, true)?;

        if let Some(bass) = inner.bass.as_ref() {
            if old_source != 0 && old_source != preloaded {
                let _ = bass.mixer_channel_remove(old_source);
                Self::free_playback_channel(bass, old_source, old_decode);
            }
            // Leave mixer volume alone: gapless must not re-slam VOL (clicks).
            // Manual callers re-apply volume with a short fade after this.
            if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
                let _ = bass.channel_play(inner.mixer_handle, false);
            }
        }

        inner.preloaded_source = 0;
        inner.preloaded_audio_path = None;
        inner.preloaded_track_path = None;
        inner.current_source = preloaded;
        inner.current_decode = preloaded_decode;
        inner.preloaded_decode = 0;

        Self::apply_segment_metadata(inner, track_path, playback);
        Self::detect_cue_position_mode(inner);
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

        let continuous_segment = same_file
            && Self::cue_segments_are_contiguous(inner.cue_end, playback.cue_start);

        if continuous_segment {
            // Physically adjacent segments in one image stay on the open timeline.
            Self::apply_segment_metadata(inner, &next.track_path, &playback);
            inner.cue_pos_relative = false;
        } else if same_file {
            // Reordered CUE queues can jump 3 -> 5 or 5 -> 4 inside one image.
            // Mute and flush before seeking: the mixer can already contain ~0.5s
            // of the physical neighbour at the old volume.
            Self::flush_mixer_hard(inner);
            Self::apply_segment(inner, &next.track_path, &playback)?;
            if let Some(bass) = inner.bass.as_ref() {
                // Flush any silence produced between the first reset and the seek.
                Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
                Self::set_mixer_volume_from_silence(
                    bass,
                    inner.mixer_handle,
                    inner.volume,
                    MANUAL_SWITCH_FADE_IN_MS,
                );
            }
            Self::detect_cue_position_mode(inner);
        } else {
            Self::activate_preloaded(inner, &next.track_path, &playback)?;
        }

        inner.gapless_queue_index = next_index;
        inner.user_paused = false;
        // Reset so the next track gets its own anti-spurious end window
        // (critical for chains of sub-second tracks).
        inner.current_track_start_time = Self::now_millis();
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
        // Soft-cut the old track first. Hard VOL=0 / channel remove while still
        // audible is the click on manual next/prev — seek doesn't hit this path
        // the same way because content continues under a brief dip.
        Self::fade_out_for_manual_switch(inner);
        Self::cancel_pending_pause(inner);

        let playback = cue::resolve_playback(track_path, audio_path, cue_start, cue_end)
            .map_err(|error| format!("{error} (track: {track_path})"))?;

        Self::begin_manual_track_switch(inner, track_path, &playback);

        if Self::can_gapless_reuse(inner, &playback.audio_path) {
            // CUE same-file: seek within the already-open stream (instant, no reopening).
            // Already faded to silence above.
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    let _ =
                        bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
                }
            }
            Self::apply_segment(inner, track_path, &playback)?;
            // Always flush the mixer buffer on a manual track switch.
            // Even if the mixer is playing, it holds up to buffer_size ms of old audio.
            // For gapless auto-advance we skip this (seamless), but for manual switches
            // the user wants the new segment to start immediately.
            if let Some(bass) = inner.bass.as_ref() {
                Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
                Self::set_mixer_volume_from_silence(
                    bass,
                    inner.mixer_handle,
                    inner.volume,
                    MANUAL_SWITCH_FADE_IN_MS,
                );
            }
            // Re-detect after the hard flush — position can look 0-based for a frame.
            Self::detect_cue_position_mode(inner);
        } else if Self::can_use_preloaded(inner, track_path, &playback.audio_path) {
            // Fast path for manual next/prev when the track was preloaded for gapless.
            Self::activate_preloaded(inner, track_path, &playback)?;
            if let Some(bass) = inner.bass.as_ref() {
                // Preload path skips open_stream restart — flush stale buffer then soft-in.
                Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
                Self::set_mixer_volume_from_silence(
                    bass,
                    inner.mixer_handle,
                    inner.volume,
                    MANUAL_SWITCH_FADE_IN_MS,
                );
            }
            inner.suppress_gapless_until =
                Self::now_millis().saturating_add(MANUAL_SEGMENT_SUPPRESS_MS);
        } else {
            Self::open_stream(inner, track_path, &playback)?;
            inner.suppress_gapless_until =
                Self::now_millis().saturating_add(MANUAL_SEGMENT_SUPPRESS_MS);
        }

        Self::refresh_pending_next(inner);
        Ok(())
    }


    fn cue_relative_position(inner: &PlayerInner, reported_secs: f64) -> f64 {
        let start = inner.cue_start.unwrap_or(0.0);
        let reported = reported_secs.max(0.0);
        let relative = if inner.cue_pos_relative || (start > 0.05 && reported + 0.35 < start) {
            // Channel already counts from segment start.
            reported
        } else {
            (reported - start).max(0.0)
        };
        // Clamp to known CUE segment so the seekbar never shoots past 100%.
        if let (Some(s), Some(e)) = (inner.cue_start, inner.cue_end) {
            let seg = (e - s).max(0.0);
            if seg > 0.0 {
                return relative.min(seg);
            }
        }
        relative
    }

    fn cue_segment_duration(inner: &PlayerInner, absolute_duration: f64) -> f64 {
        match (inner.cue_start, inner.cue_end) {
            (Some(start), Some(end)) => (end - start).max(0.0),
            (Some(start), None) => (absolute_duration - start).max(0.0),
            _ => absolute_duration,
        }
    }

    fn absolute_seek_position(inner: &PlayerInner, relative_secs: f64) -> f64 {
        // Always target the real file timeline on the decode handle.
        let start = inner.cue_start.unwrap_or(0.0);
        let mut absolute = start + relative_secs.max(0.0);
        if let (Some(start), Some(end)) = (inner.cue_start, inner.cue_end) {
            absolute = absolute.clamp(start, end.max(start));
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
            // Free tempo wrapper first (no FREESOURCE → source is not auto-freed).
            let _ = bass.channel_free(mixer_channel);
            let _ = bass.channel_free(tracked_decode);
            return;
        }
        let _ = bass.channel_free(mixer_channel);
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

    fn base_freq(bass: &BassLibrary, handle: u32) -> f32 {
        if let Ok(info) = bass.channel_get_info(handle) {
            if info.freq > 0 {
                return info.freq as f32;
            }
        }
        44100.0
    }

    /// Explicit sample rate for a playback multiplier. Never returns 0: BASS treats
    /// FREQ=0 as "default" only for SetAttribute; SlideAttribute would ramp to 0 Hz.
    fn freq_rate_target(bass: &BassLibrary, handle: u32, rate: f32) -> f32 {
        Self::base_freq(bass, handle) * rate.max(0.01)
    }

    fn apply_freq_rate(bass: &BassLibrary, handle: u32, rate: f32) {
        let target = Self::freq_rate_target(bass, handle, rate);
        let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_FREQ, target);
    }

    /// If FREQ is still the BASS default (0), materialize the real base Hz.
    /// Sliding from 0 would ramp through near-silence instead of from 1.0×.
    fn ensure_explicit_freq(bass: &BassLibrary, handle: u32) {
        match bass.channel_get_attribute(handle, bass::BASS_ATTRIB_FREQ) {
            Ok(freq) if freq > 1.0 => {}
            _ => {
                let base = Self::base_freq(bass, handle);
                let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_FREQ, base);
            }
        }
    }

    fn slide_freq_rate(bass: &BassLibrary, handle: u32, rate: f32, time_ms: u32) {
        Self::ensure_explicit_freq(bass, handle);
        let target = Self::freq_rate_target(bass, handle, rate);
        if time_ms == 0 {
            let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_FREQ, target);
            return;
        }
        if bass
            .channel_slide_attribute(handle, bass::BASS_ATTRIB_FREQ, target, time_ms)
            .is_err()
        {
            let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_FREQ, target);
        }
    }

    fn slide_tempo_pct(bass: &BassLibrary, channel: u32, rate: f32, time_ms: u32) -> Result<(), String> {
        let tempo_pct = (rate - 1.0) * 100.0;
        if time_ms == 0 {
            return bass.channel_set_attribute(channel, bass::BASS_ATTRIB_TEMPO, tempo_pct);
        }
        if bass
            .channel_slide_attribute(channel, bass::BASS_ATTRIB_TEMPO, tempo_pct, time_ms)
            .is_err()
        {
            bass.channel_set_attribute(channel, bass::BASS_ATTRIB_TEMPO, tempo_pct)?;
        }
        Ok(())
    }

    /// Read the rate BASS is currently producing (including mid-slide).
    fn read_current_rate(
        bass: &BassLibrary,
        channel: u32,
        tracked_decode: u32,
        fallback: f32,
    ) -> f32 {
        if channel == 0 {
            return fallback;
        }
        if Self::is_tempo_wrapped(bass, channel, tracked_decode) {
            if let Ok(tempo_pct) = bass.channel_get_attribute(channel, bass::BASS_ATTRIB_TEMPO) {
                return (1.0 + tempo_pct / 100.0).clamp(0.25, 2.0);
            }
            return fallback;
        }
        let decode = Self::decode_handle_for_channel(bass, channel, tracked_decode);
        let base = Self::base_freq(bass, decode);
        match bass.channel_get_attribute(decode, bass::BASS_ATTRIB_FREQ) {
            Ok(freq) if freq > 1.0 && base > 0.0 => (freq / base).clamp(0.25, 2.0),
            // 0 = BASS "use default frequency" → rate 1.0
            Ok(freq) if freq <= 1.0 => 1.0,
            _ => fallback,
        }
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

        // Pitch-coupled (or no FX): change sample rate on the decode stream.
        if pitch_enabled || !bass.has_fx() {
            if (rate - 1.0).abs() >= 0.001 {
                Self::apply_freq_rate(bass, decode, rate);
            }
            return Ok((decode, 0));
        }

        // Pitch-preserving mode: always keep a tempo wrapper so speed changes never
        // rebuild the mixer graph mid-playback (rebuilds cause clicks/rattling).
        let tempo = bass.fx_tempo_create(decode, bass::BASS_STREAM_DECODE)?;
        let _ = bass.channel_set_attribute(
            tempo,
            bass::BASS_ATTRIB_TEMPO_OPTION_PREVENT_CLICK,
            1.0,
        );
        let tempo_pct = (rate - 1.0) * 100.0;
        bass.channel_set_attribute(tempo, bass::BASS_ATTRIB_TEMPO, tempo_pct)?;
        Ok((tempo, decode))
    }

    fn wants_tempo_wrap(bass: &BassLibrary, _rate: f32, pitch_enabled: bool) -> bool {
        !pitch_enabled && bass.has_fx()
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
        slide_ms: u32,
    ) -> Result<(u32, u32), String> {
        if channel == 0 {
            return Ok((0, 0));
        }

        let wrapped = Self::is_tempo_wrapped(bass, channel, tracked_decode);
        let wants_wrap = Self::wants_tempo_wrap(bass, rate, pitch_enabled);

        if wants_wrap == wrapped {
            if wants_wrap {
                Self::slide_tempo_pct(bass, channel, rate, slide_ms)?;
                Ok((channel, tracked_decode))
            } else {
                let decode = Self::decode_handle_for_channel(bass, channel, tracked_decode);
                Self::slide_freq_rate(bass, decode, rate, slide_ms);
                Ok((channel, 0))
            }
        } else {
            Err("playback mode switch requires channel rebuild".to_string())
        }
    }

    /// Absolute content position (seconds on the original file timeline).
    ///
    /// FREQ and tempo channels use different byte timelines at rate ≠ 1; the
    /// underlying decode handle can run ahead while filling the mixer. Read the
    /// mixer-facing channel position, then map its output timeline to content time.
    /// Using the decode position directly makes one poll cross several CUE bounds.
    fn content_absolute_secs(
        bass: &BassLibrary,
        mixer_channel: u32,
        tracked_decode: u32,
        in_mixer: bool,
    ) -> f64 {
        if mixer_channel == 0 {
            return 0.0;
        }
        let decode = Self::decode_handle_for_channel(bass, mixer_channel, tracked_decode);
        let pos = if in_mixer {
            bass.mixer_channel_get_position(mixer_channel, bass::BASS_POS_BYTE)
        } else {
            bass.channel_get_position(mixer_channel, bass::BASS_POS_BYTE)
        };
        let output_secs = bass
            .channel_bytes2seconds(mixer_channel, pos)
            .max(0.0);
        if decode == 0 || decode == mixer_channel {
            return output_secs;
        }

        let output_duration = Self::stream_duration_secs(bass, mixer_channel);
        let content_duration = Self::stream_duration_secs(bass, decode);
        Self::map_output_position_to_content(output_secs, output_duration, content_duration)
    }

    fn map_output_position_to_content(
        output_secs: f64,
        output_duration: f64,
        content_duration: f64,
    ) -> f64 {
        if output_duration.is_finite()
            && content_duration.is_finite()
            && output_duration > 0.001
            && content_duration > 0.001
        {
            return (output_secs.max(0.0) / output_duration * content_duration)
                .clamp(0.0, content_duration);
        }
        output_secs.max(0.0)
    }

    /// Seek the mixer-facing channel so content is at `abs_secs` on the file timeline.
    /// Includes `0.0` — CUE track 1 often starts at INDEX 01 00:00:00, and skipping
    /// that seek left a later preloaded segment position in place.
    fn seek_content_absolute(
        bass: &BassLibrary,
        mixer_channel: u32,
        tracked_decode: u32,
        in_mixer: bool,
        abs_secs: f64,
    ) {
        if mixer_channel == 0 || abs_secs < 0.0 {
            return;
        }
        let decode = Self::decode_handle_for_channel(bass, mixer_channel, tracked_decode);
        let content_len = Self::stream_duration_secs(bass, decode);
        if content_len <= 0.001 {
            return;
        }
        let clamped = abs_secs.clamp(0.0, (content_len - 0.05).max(0.0));

        // Tempo output timeline is shorter/longer than content; map by fraction.
        let out_len = Self::stream_duration_secs(bass, mixer_channel);
        let seek_secs = if out_len > 0.001 && (out_len - content_len).abs() > 0.02 {
            (clamped / content_len) * out_len
        } else {
            clamped
        };

        let byte_pos = bass.channel_seconds2bytes(mixer_channel, seek_secs);
        if in_mixer {
            let _ = bass.mixer_channel_set_position(mixer_channel, byte_pos, bass::BASS_POS_BYTE);
        } else {
            let _ = bass.channel_set_position(mixer_channel, byte_pos, bass::BASS_POS_BYTE);
        }
    }

    /// Full reopen of the current track at the same content position.
    /// Used for pitch topology switches (FREQ ↔ tempo) which cannot be done in place safely.
    fn reopen_current_preserving_position(inner: &mut PlayerInner) -> Result<(), String> {
        let track_path = inner
            .current_file
            .clone()
            .ok_or_else(|| "No current track".to_string())?;
        let audio_path = inner.current_audio_path.clone();
        let cue_start = inner.cue_start;
        let cue_end = inner.cue_end;
        let was_paused = inner.user_paused;

        let abs_pos = if inner.current_source != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                Self::content_absolute_secs(
                    bass,
                    inner.current_source,
                    inner.current_decode,
                    true,
                )
            } else {
                cue_start.unwrap_or(0.0)
            }
        } else {
            cue_start.unwrap_or(0.0)
        };

        let playback = cue::resolve_playback(
            &track_path,
            audio_path.as_deref(),
            cue_start,
            cue_end,
        )?;

        // open_stream tears down old handles (now properly StreamFree'd) and wraps
        // with the current pitch_enabled / playback_rate.
        Self::open_stream(inner, &track_path, &playback)?;

        if let Some(bass) = inner.bass.as_ref() {
            Self::seek_content_absolute(
                bass,
                inner.current_source,
                inner.current_decode,
                true,
                abs_pos,
            );
            if was_paused {
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
                let _ = bass.channel_pause(inner.mixer_handle);
            } else {
                // open_stream already restarted + faded in at segment start; re-seek and
                // soft-restore so the topology switch itself does not click.
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
                Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
                Self::set_mixer_volume_from_silence(
                    bass,
                    inner.mixer_handle,
                    inner.volume,
                    MANUAL_SWITCH_FADE_IN_MS,
                );
            }
        }

        Ok(())
    }

    fn reapply_at_rate(inner: &mut PlayerInner, rate: f32) {
        Self::reapply_at_rate_with_slide(inner, rate, 0);
    }

    fn reapply_at_rate_with_slide(inner: &mut PlayerInner, rate: f32, slide_ms: u32) {
        let pitch_enabled = inner.pitch_enabled;
        let current_source = inner.current_source;
        let current_decode = inner.current_decode;
        let had_preload = inner.preloaded_source != 0;

        let in_place = if current_source != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                Self::apply_rate_in_place(
                    bass,
                    current_source,
                    current_decode,
                    rate,
                    pitch_enabled,
                    slide_ms,
                )
            } else {
                return;
            }
        } else {
            Ok((0, 0))
        };

        match in_place {
            Ok((channel, decode)) if current_source != 0 => {
                inner.current_source = channel;
                inner.current_decode = decode;
            }
            Ok(_) => {}
            Err(_) => {
                // Topology switch while changing rate: reopen at same position.
                inner.suppress_gapless_until = Self::now_millis().saturating_add(2500);
                if let Err(e) = Self::reopen_current_preserving_position(inner) {
                    eprintln!("Rate mode reopen failed: {e}");
                }
            }
        }

        if had_preload {
            // Preload was built under a possibly different topology — rebuild.
            Self::clear_preload(inner);
            Self::refresh_pending_next(inner);
        }
    }

    fn stream_duration_secs(bass: &BassLibrary, handle: u32) -> f64 {
        let len_bytes = bass.channel_get_length(handle, bass::BASS_POS_BYTE);
        bass.channel_bytes2seconds(handle, len_bytes)
    }

    fn track_end_position(inner: &PlayerInner, bass: &BassLibrary, handle: u32) -> f64 {
        if let Some(end) = inner.cue_end {
            return end;
        }
        // Prefer decode length (content timeline) when tempo-wrapped.
        let decode = Self::decode_handle_for_channel(bass, handle, inner.current_decode);
        Self::stream_duration_secs(bass, decode)
    }

    /// `reported_secs` is the raw BASS read (may be absolute or segment-relative).
    fn track_ending(inner: &PlayerInner, bass: &BassLibrary, reported_secs: f64) -> bool {
        if inner.current_source == 0 {
            return false;
        }
        let end = Self::track_end_position(inner, bass, inner.current_source);
        if !end.is_finite() || end < 0.0 {
            return false;
        }
        let start = inner.cue_start.unwrap_or(0.0);
        // Degenerate / missing bounds must never auto-fire end (that causes
        // immediate multi-track skip right after clicking a CUE entry).
        if end <= start + 0.05 {
            return false;
        }
        let timeline = Self::content_timeline_secs(inner, reported_secs);
        // Must have actually reached the segment — ignore stale high positions
        // only when they still sit *before* this track's start (previous CUE).
        if start > 0.05 && timeline + 0.5 < start {
            return false;
        }
        timeline + GAPLESS_END_EPSILON_SECS >= end
    }

    /// True when a STOPPED source is a real end-of-track, not a mid-switch glitch.
    fn stream_done_is_real_end(
        inner: &PlayerInner,
        bass: &BassLibrary,
        reported_secs: f64,
        stream_done: bool,
    ) -> bool {
        if !stream_done {
            return false;
        }
        if inner.user_paused {
            return false;
        }
        // Source already torn down — poll recovery path; trust after spurious guard.
        if inner.current_source == 0 {
            return true;
        }
        let end = Self::track_end_position(inner, bass, inner.current_source);
        if !end.is_finite() || end <= 0.0 {
            // Unknown length — trust BASS only after the spurious guard.
            return true;
        }
        let start = inner.cue_start.unwrap_or(0.0);
        let timeline = Self::content_timeline_secs(inner, reported_secs);
        // Near / past expected end on the content timeline.
        if timeline + 1.25 >= end {
            return start <= 0.05 || timeline + 0.25 >= start;
        }
        // Some codecs reset position to 0 the moment the decode ends. Fall back
        // to wall-clock: only accept STOPPED if we've played most of the segment.
        let segment = (end - start).max(0.0);
        if segment <= 0.05 {
            return false;
        }
        let age_ms = Self::now_millis().saturating_sub(inner.current_track_start_time);
        let min_play_ms = ((segment * 0.82) * 1000.0).round() as u64;
        age_ms >= min_play_ms.max(POLL_INTERVAL_MS)
    }

    /// Segment length in ms (CUE bounds or full stream). 0 if unknown.
    fn current_segment_duration_ms(inner: &PlayerInner, bass: &BassLibrary) -> u64 {
        if inner.current_source == 0 {
            return 0;
        }
        let absolute_duration = Self::stream_duration_secs(bass, inner.current_source);
        let segment = Self::cue_segment_duration(inner, absolute_duration);
        if segment.is_finite() && segment > 0.0 {
            (segment * 1000.0).round() as u64
        } else {
            0
        }
    }

    /// Whether enough time has passed since play() to trust an "ended" signal.
    /// Long tracks keep a 1.5s anti-spurious window; sub-second tracks use a
    /// duration-capped window so they can still gapless-advance / emit ended.
    fn past_spurious_end_guard(inner: &PlayerInner, bass: &BassLibrary) -> bool {
        let age = Self::now_millis().saturating_sub(inner.current_track_start_time);
        let segment_ms = Self::current_segment_duration_ms(inner, bass);
        let guard_ms = if segment_ms == 0 {
            SPURIOUS_END_GUARD_MS
        } else {
            // Allow advance once we've roughly reached the real end (50ms slack),
            // but never wait longer than the long-track spurious window.
            // Floor at one poll interval so open-glitch frames are still ignored.
            segment_ms
                .saturating_sub(50)
                .clamp(POLL_INTERVAL_MS, SPURIOUS_END_GUARD_MS)
        };
        age > guard_ms
    }

    pub fn pause(&self) -> Result<(), String> {
        let _ops = self.ops.lock();
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
        let _ops = self.ops.lock();
        self.run_on_bass_thread(|inner| {
            Self::cancel_pending_pause(inner);
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            if inner.mixer_handle != 0 {
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
        let _ops = self.ops.lock();
        let cleared_mix = self.run_on_bass_thread(|inner| {
            let was_mix = inner.mix_preview_active;
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
            inner.mix_preview_active = false;
            Ok(was_mix)
        })?;
        if cleared_mix {
            self.emit_mix_preview_active(false);
        }
        self.set_source_active(false);
        Ok(())
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
            inner.cue_pos_relative = false;
            inner.gapless_queue.clear();
            inner.gapless_queue_index = 0;
            inner.pending_next = None;
            Ok(())
        })
    }

    /// Seek to a position in seconds.
    /// Mute → jump + mixer flush → short fade-in (avoids post-seek clicks).
    /// While paused, position updates but playback stays paused (arrow keys / scrub).
    pub fn seek(&self, position_secs: f64) -> Result<(), String> {
        let _ops = self.ops.lock();
        self.run_on_bass_thread(move |inner| {
            if inner.current_source == 0 || inner.mixer_handle == 0 {
                return Err("Nothing is playing".into());
            }
            let target = inner.volume;
            let was_paused = inner.user_paused;
            let absolute_secs = Self::absolute_seek_position(inner, position_secs);
            let segment_duration = match (inner.cue_start, inner.cue_end) {
                (Some(start), Some(end)) => (end - start).max(0.0),
                _ => 0.0,
            };
            let seeks_to_segment_end = segment_duration > 0.05
                && position_secs + GAPLESS_END_EPSILON_SECS >= segment_duration;
            let source = inner.current_source;
            let decode = inner.current_decode;
            let mixer = inner.mixer_handle;

            {
                let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
                // Silent through the flush so restart doesn't spit a full-volume edge.
                let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, SEEK_DIP_LEVEL);

                // Content timeline (decode) — correct for tempo wrappers and CUE segments.
                Self::seek_content_absolute(bass, source, decode, true, absolute_secs);

                // Flush mixer buffer so the new position is heard immediately.
                // channel_play restarts the mixer — re-pause if we were paused.
                Self::restart_mixer_with_buffer(bass, mixer);

                if was_paused {
                    let _ = bass.channel_pause(mixer);
                    // Match post-pause-fade state: silent until resume.
                    let _ = bass.channel_set_attribute(mixer, bass::BASS_ATTRIB_VOL, 0.0);
                } else {
                    Self::set_mixer_volume_from_silence(
                        bass,
                        mixer,
                        target,
                        SEEK_FADE_IN_MS,
                    );
                }
            }

            // Re-probe absolute vs segment-relative reporting after the seek.
            Self::detect_cue_position_mode(inner);
            // Don't gapless-advance on a post-seek STOPPED glitch.
            inner.suppress_gapless_until = if seeks_to_segment_end {
                // Seeking to 100% is an intentional request to advance.
                0
            } else {
                Self::now_millis().saturating_add(MANUAL_SEGMENT_SUPPRESS_MS)
            };
            if was_paused {
                inner.user_paused = true;
            }

            Ok(())
        })
    }

    /// Set volume (0.0 — 1.0).
    pub fn set_volume(&self, vol: f32) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            let next = vol.clamp(0.0, 1.0);
            // Skip no-op writes so bootstrap re-apply after Ctrl+R does not cancel
            // an in-flight VOL slide or touch the device for nothing.
            if (inner.volume - next).abs() < 0.0005 {
                return Ok(());
            }
            inner.volume = next;
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    // While paused the mixer is intentionally silent (post fade-out).
                    // Only store the target — resume() fades back up to inner.volume.
                    if !inner.user_paused {
                        bass.channel_set_attribute(
                            inner.mixer_handle,
                            bass::BASS_ATTRIB_VOL,
                            inner.volume,
                        )?;
                    }
                }
            }
            Ok(())
        })
    }

    /// Set playback rate (0.25 — 2.0).
    ///
    /// Uses a fixed-duration BASS attribute slide (FREQ or TEMPO) so the change is
    /// sample-accurate and always takes the same intentional time.
    pub fn set_playback_rate(&self, rate: f32) -> Result<(), String> {
        let rate = rate.clamp(0.25, 2.0);
        self.run_on_bass_thread(move |inner| {
            inner.playback_rate = rate;

            // Prefer the live attribute (mid-slide). Do not trust applied alone —
            // a second identical request while still ramping must keep sliding.
            let from = if let Some(bass) = inner.bass.as_ref() {
                Self::read_current_rate(
                    bass,
                    inner.current_source,
                    inner.current_decode,
                    inner.applied_playback_rate,
                )
            } else {
                inner.applied_playback_rate
            };

            if (from - rate).abs() < 0.0005 {
                // Already there: snap to exact value (cancels any residual slide).
                Self::reapply_at_rate_with_slide(inner, rate, 0);
                inner.applied_playback_rate = rate;
                return Ok(());
            }

            // Always full ramp so presets and settle feel consistent — not "too fast".
            Self::reapply_at_rate_with_slide(inner, rate, PLAYBACK_RATE_RAMP_MS);
            inner.applied_playback_rate = rate;
            Ok(())
        })
    }

    /// When enabled, playback speed also shifts pitch. When disabled, tempo FX preserves pitch.
    pub fn set_pitch_enabled(&self, enabled: bool) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            if inner.pitch_enabled == enabled {
                return Ok(());
            }
            inner.pitch_enabled = enabled;
            let rate = inner.playback_rate;

            // FREQ ↔ tempo is a graph rebuild. In-place rebuild left the source
            // STOPPED and gapless advanced the queue — reopen at content position.
            if inner.current_source != 0 {
                inner.suppress_gapless_until = Self::now_millis().saturating_add(2500);
                if let Err(e) = Self::reopen_current_preserving_position(inner) {
                    eprintln!("Pitch mode reopen failed: {e}");
                    return Err(e);
                }
            }

            // Preload was built under the old topology — drop and rebuild.
            Self::clear_preload(inner);
            Self::refresh_pending_next(inner);

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
        let was_mix = if self.on_bass_thread() {
            Self::release_ended_stream_inner(&mut self.inner.lock())
        } else {
            self.run_on_bass_thread(|inner| Ok(Self::release_ended_stream_inner(inner)))
                .unwrap_or(false)
        };
        if was_mix {
            self.emit_mix_preview_active(false);
        }
    }

    /// Returns true if a mix-preview session was active (caller should emit).
    fn release_ended_stream_inner(inner: &mut PlayerInner) -> bool {
        Self::teardown_current(inner);
        inner.user_paused = false;
        // Natural end of mix preview (or any stream) must release device ownership
        // so the main library is not stuck frozen after Preview finishes.
        let was_mix = inner.mix_preview_active;
        inner.mix_preview_active = false;
        // Do not stop the mixer here — it allows smoother resume / next play.
        // Full stop() command will handle explicit stop.
        was_mix
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
        // Dual-deck mix: current_source may still be the previous track after it
        // ended, while next is still playing. Prefer any live mix deck so the
        // poller does not emit track-ended and tear down the surviving deck.
        if inner.mix_crossfade.is_some() && Self::mix_has_active_audio(inner) {
            let mix = inner.mix_crossfade.as_ref().unwrap();
            let bass = inner.bass.as_ref().unwrap();
            let (src, dec) = if mix.from_source != 0
                && bass.channel_is_active(mix.from_source) != bass::BASS_ACTIVE_STOPPED
            {
                (mix.from_source, mix.from_decode)
            } else if mix.to_source != 0 {
                (mix.to_source, mix.to_decode)
            } else {
                (inner.current_source, inner.current_decode)
            };
            let reported = if src != 0 {
                Self::content_absolute_secs(bass, src, dec, true)
            } else {
                0.0
            };
            let decode = if src != 0 {
                Self::decode_handle_for_channel(bass, src, dec)
            } else {
                0
            };
            let absolute_duration = if decode != 0 {
                Self::stream_duration_secs(bass, decode)
            } else {
                0.0
            };
            let position = Self::cue_relative_position(inner, reported);
            let duration = Self::cue_segment_duration(inner, absolute_duration);
            let file_name = inner.current_file.as_ref().map(|f| {
                std::path::Path::new(f)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            return PlayerStateSnapshot {
                state: PlaybackState::Playing,
                is_playing: true,
                is_paused: false,
                volume: inner.volume,
                position,
                duration,
                current_file: inner.current_file.clone(),
                current_file_name: file_name,
            };
        }

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
        let decode = Self::decode_handle_for_channel(bass, inner.current_source, inner.current_decode);
        let reported = Self::content_absolute_secs(
            bass,
            inner.current_source,
            inner.current_decode,
            true,
        );
        let absolute_duration = Self::stream_duration_secs(bass, decode);
        let position = Self::cue_relative_position(inner, reported);
        let duration = Self::cue_segment_duration(inner, absolute_duration);

        if source_ended || mixer_active == PlaybackState::Stopped {
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
        // Keep this thread above BELOW_NORMAL process priority when unfocused.
        crate::process_util::register_audio_thread();
    }

    /// Poll playback on the main thread and emit position / track-ended events.
    ///
    /// BASS must only be called from the thread that invoked `BASS_Init`.
    /// Uses one long-lived worker (not a new OS thread every tick).
    ///
    /// While a source is active, gapless polls at [`POLL_INTERVAL_MS`]. When idle
    /// (no source), the worker sleeps [`POLL_IDLE_MS`] and **skips** main-thread
    /// hops so the WebView message loop is not poked 20×/s with an empty player.
    /// UI position IPC is further throttled via [`UI_EMIT_HOT_MS`] / cold.
    pub fn start_position_emitter(&self, app: AppHandle) {
        let player = self.clone();
        let was_playing = Arc::new(StdMutex::new(false));
        let last_rpc_state = Arc::new(StdMutex::new(None::<PlaybackState>));
        // Force an immediate position push on the first Playing tick.
        let last_rpc_sync = Arc::new(StdMutex::new(
            Instant::now()
                .checked_sub(Duration::from_millis(RPC_POSITION_SYNC_MS))
                .unwrap_or_else(Instant::now),
        ));
        let last_ui_emit = Arc::new(StdMutex::new(
            Instant::now()
                .checked_sub(Duration::from_millis(UI_EMIT_COLD_MS))
                .unwrap_or_else(Instant::now),
        ));

        let _ = thread::Builder::new()
            .name("muzeeka-position-poll".into())
            .spawn(move || {
                loop {
                    let active = player.is_source_active();
                    let sleep_ms = if active {
                        POLL_INTERVAL_MS
                    } else {
                        POLL_IDLE_MS
                    };
                    thread::sleep(Duration::from_millis(sleep_ms));

                    // Still idle after sleep (and play() didn't arm the flag) —
                    // do not bounce the UI/BASS thread for a no-op poll.
                    if !player.is_source_active() {
                        continue;
                    }

                    let app_emit = app.clone();
                    let player_for_main = player.clone();
                    let was_for_main = was_playing.clone();
                    let rpc_for_main = last_rpc_state.clone();
                    let rpc_sync_for_main = last_rpc_sync.clone();
                    let ui_emit_for_main = last_ui_emit.clone();
                    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);

                    let _ = app.run_on_main_thread(move || {
                        Self::position_poll_tick(
                            &app_emit,
                            &player_for_main,
                            &was_for_main,
                            &rpc_for_main,
                            &rpc_sync_for_main,
                            &ui_emit_for_main,
                        );
                        let _ = done_tx.send(());
                    });
                    // Bound wait so a stuck main thread cannot freeze the worker forever.
                    let _ = done_rx.recv_timeout(Duration::from_secs(2));
                }
            });
    }

    fn should_emit_ui_position(
        player: &Player,
        last_ui_emit: &StdMutex<Instant>,
        force: bool,
    ) -> bool {
        if force {
            if let Ok(mut last) = last_ui_emit.lock() {
                *last = Instant::now();
            }
            return true;
        }
        let interval_ms = if player.ui_is_hot() {
            UI_EMIT_HOT_MS
        } else {
            UI_EMIT_COLD_MS
        };
        let Ok(mut last) = last_ui_emit.lock() else {
            return true;
        };
        if last.elapsed() >= Duration::from_millis(interval_ms) {
            *last = Instant::now();
            true
        } else {
            false
        }
    }

    fn position_poll_tick(
        app_emit: &AppHandle,
        player_for_main: &Player,
        was_for_main: &StdMutex<bool>,
        rpc_for_main: &StdMutex<Option<PlaybackState>>,
        rpc_sync_for_main: &StdMutex<Instant>,
        last_ui_emit: &StdMutex<Instant>,
    ) {
        let poll_result = player_for_main.run_on_bass_thread(|inner| {

            if inner.mixer_handle == 0 || inner.bass.is_none() {
                return Ok(None);
            }
            // Allow current_source==0 during intentional silence gap (pending next).
            if inner.current_source == 0 && inner.mix_crossfade.is_none() {
                return Ok(None);
            }

            let now = Self::now_millis();
            // Mix timeline: inject next only when graph delay has elapsed
            // (never early when prev ends — that kills user-planned pauses).
            // Drop prev when its remaining length is done (audio time or STOPPED).
            if inner.mix_crossfade.is_some() {
                let (inject, drop_from, mix_secs, delay_left) = {
                    let mix = inner.mix_crossfade.as_ref().unwrap();
                    let bass = inner.bass.as_ref().unwrap();
                    // Graph / playhead clock (1× wall) — not content time under Speed.
                    let mix_secs = Self::mix_timeline_secs(mix, bass, now);
                    let inject = mix.pending_to.is_some()
                        && mix_secs + 0.002 >= mix.to_graph_delay_secs;
                    let mut drop_from = false;
                    if mix.from_source != 0 {
                        let from_stopped = bass.channel_is_active(mix.from_source)
                            == bass::BASS_ACTIVE_STOPPED;
                        // End prev when the *layout* says so (or stream already dry).
                        let duration_done = mix.from_duration_secs > 0.0
                            && mix_secs + 0.05 >= mix.from_duration_secs;
                        drop_from = from_stopped || duration_done;
                    }
                    let delay_left = mix.to_graph_delay_secs - mix_secs;
                    (inject, drop_from, mix_secs, delay_left)
                };
                if inject {
                    Self::inject_pending_to_deck(inner);
                }
                if drop_from {
                    // Prev done but next still in the future on the graph → silence gap.
                    let wait_for_next = inner.mix_crossfade.as_ref().is_some_and(|m| {
                        m.pending_to.is_some() && delay_left > 0.05
                    });
                    if wait_for_next {
                        Self::drop_mix_from_only(inner, mix_secs);
                        Self::apply_mix_volume_automation(inner);
                        return Ok(None);
                    }
                    let path = Self::finish_mix_from_deck(inner);
                    // Apply follow-volume immediately after handoff.
                    Self::apply_mix_volume_automation(inner);
                    return Ok(path);
                }
                // Drive volume envelopes every poll tick while dual-deck is live.
                Self::apply_mix_volume_automation(inner);
                // Still in mix session — no normal gapless / end teardown.
                return Ok(None);
            }
            // Surviving next deck after handoff — keep envelope running.
            if inner.mix_vol_follow.is_some() {
                Self::apply_mix_volume_automation(inner);
            }
            // Pitch/rate topology rebuild and seeks can briefly look "ended".
            // Never advance or tear down during the suppress window.
            if now < inner.suppress_gapless_until {
                return Ok(None);
            }

            let (ending, real_stream_done, past_guard) = {
                let bass = inner.bass.as_ref().unwrap();
                let mixer_active = bass.channel_is_active(inner.mixer_handle);
                let src_active = if inner.current_source != 0 {
                    bass.channel_is_active(inner.current_source)
                } else {
                    bass::BASS_ACTIVE_STOPPED
                };
                // Content timeline (decode handle) — required for correct CUE INDEX end detection
                // with tempo wrappers and for absolute-vs-relative seek reporting.
                let reported = Self::content_absolute_secs(
                    bass,
                    inner.current_source,
                    inner.current_decode,
                    true,
                );

                let playing = mixer_active == bass::BASS_ACTIVE_PLAYING
                    && !inner.user_paused;
                let ending = playing && Self::track_ending(inner, bass, reported);
                // Source ended (or no source) means the track is done, even if mixer is still "playing" silence (NONSTOP)
                let stream_done_raw = src_active == bass::BASS_ACTIVE_STOPPED
                    || mixer_active == bass::BASS_ACTIVE_STOPPED;
                // Ignore mid-track STOPPED glitches (very common right after CUE seeks).
                let real_stream_done =
                    Self::stream_done_is_real_end(inner, bass, reported, stream_done_raw);
                let past_guard = Self::past_spurious_end_guard(inner, bass);
                (ending, real_stream_done, past_guard)
            };

            if Self::has_next_in_gapless_queue(inner) && (ending || real_stream_done) {
                // Guard against spurious early advance after manual click in que
                // (new track can briefly appear done). Short tracks cap the guard
                // by their own duration so <1s files still auto-advance.
                if past_guard {
                    return Self::try_advance_gapless(inner).map(Some).map_err(|error| {
                        eprintln!("Gapless advance failed: {error}");
                        error
                    });
                }
            }

            if (ending || real_stream_done)
                && !Self::has_next_in_gapless_queue(inner)
                && past_guard
            {
                // End of playlist: ensure source is gone (AUTOFREE usually handles it).
                // Wait for the duration-aware guard so short last tracks still emit
                // track-ended promptly (duration is known while the source lives).
                // Do not stop the mixer — keep it running (silent) so next playback starts without device hiccup.
                if inner.current_source != 0 {
                    if let Some(bass) = inner.bass.as_ref() {
                        let _ = bass.mixer_channel_remove(inner.current_source);
                        Self::free_playback_channel(
                            bass,
                            inner.current_source,
                            inner.current_decode,
                        );
                    }
                    inner.current_source = 0;
                    inner.current_decode = 0;
                }
            }

            Ok(None)
        });

        let advanced_path = match poll_result {
            Ok(result) => result,
            Err(error) => {
                eprintln!("Gapless poll failed: {error}");
                None
            }
        };

        if let Some(path) = advanced_path {
            // Gapless auto-advance is a new play start for the next track.
            player_for_main.record_play_stat(&path);
            player_for_main.set_source_active(true);
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
            let _ = Self::should_emit_ui_position(player_for_main, last_ui_emit, true);
            player_for_main.sync_discord_presence();
            if let Ok(mut rpc_state) = rpc_for_main.lock() {
                *rpc_state = Some(snapshot.state);
            }
            if let Ok(mut last_sync) = rpc_sync_for_main.lock() {
                *last_sync = Instant::now();
            }
            return;
        }

        let snapshot = player_for_main.get_state();

        let mut was = was_for_main.lock().unwrap_or_else(|e| e.into_inner());
        match snapshot.state {
            PlaybackState::Playing => {
                *was = true;
                if Self::should_emit_ui_position(player_for_main, last_ui_emit, false) {
                    let payload = PositionPayload {
                        position: snapshot.position,
                        duration: snapshot.duration,
                        state: snapshot.state,
                    };
                    let _ = app_emit.emit("player:position", &payload);
                }
                // Note: do not emit player:state on every poll tick.
                // The position event already carries the state, and the
                // frontend position listener applies it (with pause guard).
                // Frequent player:state was causing extra clobbering of isPlaying.
            }
            PlaybackState::Paused => {
                // Emit on transition into pause, while focused (seek while paused),
                // or on the cold interval — never spam 20 Hz while paused in background.
                let entered_pause = *was;
                *was = false;
                if Self::should_emit_ui_position(player_for_main, last_ui_emit, entered_pause) {
                    let payload = PositionPayload {
                        position: snapshot.position,
                        duration: snapshot.duration,
                        state: snapshot.state,
                    };
                    let _ = app_emit.emit("player:position", &payload);
                }
            }
            PlaybackState::Stopped if *was => {
                let recovered = player_for_main
                    .run_on_bass_thread(|inner| {
                        if !Self::has_next_in_gapless_queue(inner) {
                            return Ok(None);
                        }
                        let now = Self::now_millis();
                        // Same guards as the playing-path advance: pitch/rate
                        // topology rebuilds can briefly look "stopped".
                        if now < inner.suppress_gapless_until {
                            return Ok(None);
                        }
                        let bass = match inner.bass.as_ref() {
                            Some(b) => b,
                            None => return Ok(None),
                        };
                        if Self::past_spurious_end_guard(inner, bass) {
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
                    player_for_main.record_play_stat(&path);
                    player_for_main.set_source_active(true);
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
                    let _ = Self::should_emit_ui_position(player_for_main, last_ui_emit, true);
                    player_for_main.sync_discord_presence();
                    if let Ok(mut rpc_state) = rpc_for_main.lock() {
                        *rpc_state = Some(snapshot.state);
                    }
                    if let Ok(mut last_sync) = rpc_sync_for_main.lock() {
                        *last_sync = Instant::now();
                    }
                    return;
                }

                // End-of-queue (or gapless failed): emit track-ended only after the
                // anti-spurious window, so sub-second tracks aren't torn down too early
                // and frontend can still advance once the guard elapses.
                let ended_path = player_for_main
                    .run_on_bass_thread(|inner| {
                        let now = Self::now_millis();
                        if now < inner.suppress_gapless_until {
                            return Ok(None::<String>);
                        }
                        if let Some(bass) = inner.bass.as_ref() {
                            if !Self::past_spurious_end_guard(inner, bass) {
                                return Ok(None);
                            }
                        } else if now.saturating_sub(inner.current_track_start_time)
                            <= SPURIOUS_END_GUARD_MS
                        {
                            return Ok(None);
                        }
                        Ok(inner.current_file.clone())
                    })
                    .ok()
                    .flatten();

                let Some(ended_path) = ended_path else {
                    // Guard still active — keep *was so we retry on the next poll.
                    return;
                };

                player_for_main.release_ended_stream();
                *was = false;
                player_for_main.set_source_active(false);
                let _ = app_emit.emit(
                    "player:track-ended",
                    serde_json::json!({ "path": ended_path }),
                );
                player_for_main.sync_discord_presence();
                if let Ok(mut rpc_state) = rpc_for_main.lock() {
                    *rpc_state = Some(PlaybackState::Stopped);
                }
                if let Ok(mut last_sync) = rpc_sync_for_main.lock() {
                    *last_sync = Instant::now();
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
                    if let Ok(mut last_sync) = rpc_sync_for_main.lock() {
                        *last_sync = Instant::now();
                    }
                }
                *rpc_state = Some(snapshot.state);
            } else if snapshot.state == PlaybackState::Playing {
                // While playing, re-push timestamps every few seconds so Discord
                // progress does not drift (rate changes, clock skew, long tracks).
                let due = rpc_sync_for_main
                    .lock()
                    .map(|last| last.elapsed() >= Duration::from_millis(RPC_POSITION_SYNC_MS))
                    .unwrap_or(false);
                if due {
                    player_for_main.sync_discord_presence();
                    if let Ok(mut last_sync) = rpc_sync_for_main.lock() {
                        *last_sync = Instant::now();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GaplessTrack, Player};

    fn queue(first: &str) -> Vec<GaplessTrack> {
        vec![GaplessTrack {
            track_path: first.to_string(),
            audio_path: first.split("#cue:").next().unwrap_or(first).to_string(),
            cue_start: Some(0.0),
            cue_end: Some(60.0),
        }]
    }

    #[test]
    fn stale_queue_refresh_cannot_replace_current_cue_track() {
        let old = r"C:\music\album.flac#cue:3";
        let current = r"C:\music\album.flac#cue:4";

        assert!(!Player::queue_refresh_matches(
            Some(current),
            Some(old),
            &queue(old),
        ));
        assert!(Player::queue_refresh_matches(
            Some(current),
            Some(current),
            &queue(current),
        ));
    }

    #[test]
    fn queue_refresh_requires_current_track_as_first_entry() {
        let current = r"C:\music\album.flac#cue:4";
        let wrong_first = r"C:\music\album.flac#cue:5";

        assert!(!Player::queue_refresh_matches(
            Some(current),
            Some(current),
            &queue(wrong_first),
        ));
    }

    #[test]
    fn rendered_position_maps_to_content_timeline_without_decode_readahead() {
        assert_eq!(
            Player::map_output_position_to_content(30.0, 60.0, 120.0),
            60.0
        );
        assert_eq!(
            Player::map_output_position_to_content(61.0, 60.0, 120.0),
            120.0
        );
    }

    #[test]
    fn only_physical_cue_neighbours_continue_without_seek() {
        assert!(Player::cue_segments_are_contiguous(
            Some(120.0),
            Some(120.0),
        ));
        assert!(Player::cue_segments_are_contiguous(
            Some(120.0),
            Some(120.05),
        ));
        assert!(!Player::cue_segments_are_contiguous(
            Some(120.0),
            Some(180.0),
        ));
        assert!(!Player::cue_segments_are_contiguous(
            Some(300.0),
            Some(180.0),
        ));
        assert!(!Player::cue_segments_are_contiguous(None, Some(180.0)));
    }
}
