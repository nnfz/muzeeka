// Player state management
//
// Wraps BASS in a higher-level API that tracks the current track, volume,
// playback state, and emits Tauri events for position updates.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex as StdMutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::bass::{self, BassLibrary};
use crate::cue::{self, PlaybackTarget};
use crate::discord_rpc::DiscordPresence;
use crate::dsp_chain::{chain_dsp_callback, ChainSlotSettings, DspChain, DspChainStatus};
use crate::icy_tap::{self, LiveMetaInbox};
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

/// ICY now-playing update pushed to the UI while a live radio stream plays.
/// `path` identifies the station; `title` is the currently announced track
/// (empty when the station sends none); `station` is the ICY station name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamMetadataPayload {
    pub path: String,
    pub title: Option<String>,
    pub station: Option<String>,
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
    /// The mixer stream (output). We play/pause this. The DSP rack attaches here.
    mixer_handle: u32,
    /// The current decode source plugged into the mixer (for the active track).
    current_source: u32,
    /// Handle of the one DSP that runs the whole effect rack (0 = not attached).
    chain_dsp_handle: u32,
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
    /// The effect rack. Ordered inside itself, so this is the only DSP the mixer
    /// ever sees — see `dsp_chain.rs` for why order is data, not BASS priority.
    chain: &'static DspChain,
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
    /// Saved transition waiting for the playhead to reach its start (playlist mix mode).
    armed_mix: Option<ArmedMix>,
    extra_outputs: Vec<ExtraOutput>,
    extra_devices_inited: Vec<i32>,
    next_extra_id: u64,
    /// The active source is an internet radio stream (URL). Live sources have
    /// no length, never auto-advance and ignore seeks.
    live_source: Option<LiveStreamState>,
    /// Bytes pushed into the live mixer source since open (diagnostics).
    live_bytes_fed: u64,
    live_last_pump_log_ms: u64,
    live_zero_pulls: u32,
    live_pcm_rate: u32,
    live_pcm_chans: u32,
    live_reconnect_after_ms: u64,
    live_meta_sync: u32,
    /// ICY text copied inside BASS_SYNC_META (GetTags is empty after the callback).
    live_meta_inbox: Arc<StdMutex<LiveMetaInbox>>,
    live_meta_user: usize,
    /// Sidecar HTTP ICY tap (BASS mixer DECODE never surfaces StreamTitle).
    icy_tap_stop: Option<Arc<AtomicBool>>,
    icy_tap_join: Option<thread::JoinHandle<()>>,
}

/// Owned by DOWNLOADPROC + META sync `user` until teardown.
struct LiveMetaUser {
    get_tags: unsafe extern "system" fn(u32, u32) -> *const std::ffi::c_void,
    inbox: Arc<StdMutex<LiveMetaInbox>>,
    icy: StdMutex<IcyParse>,
}

#[derive(Default)]
struct IcyParse {
    metaint: u32,
    audio_left: u32,
    meta_left: u32,
    meta: Vec<u8>,
}

/// Per-live-stream bookkeeping (ICY now-playing, station name, position base).
#[derive(Debug, Clone)]
struct LiveStreamState {
    /// Stream URL (`current_audio_path` while live).
    url: String,
    /// Wall-clock ms when playback of this stream started (position base).
    started_ms: u64,
    /// Wall-clock ms accumulated while paused (excluded from position).
    paused_ms: u64,
    /// Wall-clock ms when the current pause began (0 = not paused).
    pause_started_ms: u64,
    /// Last ICY `StreamTitle` reported to the UI.
    last_title: Option<String>,
    /// Last station name (ICY `name` header) reported to the UI.
    last_station: Option<String>,
    /// Bytes BASS had buffered last time ICY was checked — change ⇒ new metadata.
    last_probe_bytes: u64,
    /// Wall ms of the last ICY probe (rate limit).
    last_probe_ms: u64,
}

impl LiveStreamState {
    fn new(url: String) -> Self {
        Self {
            url,
            started_ms: Player::now_millis(),
            paused_ms: 0,
            pause_started_ms: 0,
            last_title: None,
            last_station: None,
            last_probe_bytes: 0,
            last_probe_ms: 0,
        }
    }

    /// Seconds of audio played so far: wall-clock since start minus time spent paused.
    fn elapsed_secs(&self, now_ms: u64) -> f64 {
        let gross = now_ms.saturating_sub(self.started_ms);
        // Include the in-progress pause so the clock freezes while paused.
        let live_pause = if self.pause_started_ms != 0 {
            now_ms.saturating_sub(self.pause_started_ms)
        } else {
            0
        };
        let net = gross.saturating_sub(self.paused_ms).saturating_sub(live_pause);
        net as f64 / 1000.0
    }

    /// Mark the beginning of a pause (idempotent).
    fn mark_paused(&mut self, now_ms: u64) {
        if self.pause_started_ms == 0 {
            self.pause_started_ms = now_ms;
        }
    }

    /// Accumulate the just-finished pause into `paused_ms` (idempotent).
    fn mark_resumed(&mut self, now_ms: u64) {
        if self.pause_started_ms != 0 {
            self.paused_ms = self
                .paused_ms
                .saturating_add(now_ms.saturating_sub(self.pause_started_ms));
            self.pause_started_ms = 0;
        }
    }
}

struct ExtraOutput {
    id: String,
    device: i32,
    name: String,
    split_handle: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub id: i32,
    pub name: String,
    pub driver: String,
    pub enabled: bool,
    pub is_default: bool,
    pub initialized: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtraOutputInfo {
    pub id: String,
    pub device_id: i32,
    pub name: String,
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
    /// Mix seconds at which the UI adopts the incoming track (playlist mix mode;
    /// `None` in the editor preview, which owns its own transport display).
    ui_switch_secs: Option<f64>,
    /// Set once the UI switch above has been emitted.
    ui_switched: bool,
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

/// A saved editor transition armed on the track that is **already playing**.
///
/// Playlist mix mode never re-opens the outgoing deck: when the playhead reaches
/// `start_at_secs` the live `current_source` is adopted as the mix `from` deck, so
/// the transition begins without a seam. Mix time 0 == `start_at_secs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmedMix {
    /// Track the transition belongs to — must still be current when it fires.
    pub from_path: String,
    /// Absolute content seconds on the outgoing track where mix time 0 sits.
    pub start_at_secs: f64,
    /// How long the outgoing deck keeps playing past mix time 0.
    pub from_duration_secs: f64,
    pub to_path: String,
    #[serde(default)]
    pub to_audio_path: Option<String>,
    #[serde(default)]
    pub to_cue_start: Option<f64>,
    #[serde(default)]
    pub to_cue_end: Option<f64>,
    /// Mix seconds before the incoming deck joins the graph.
    #[serde(default)]
    pub to_delay_secs: f64,
    /// End of the whole saved layout, in mix seconds. Entering later than this
    /// means the transition is already over.
    #[serde(default)]
    pub span_secs: f64,
    /// Mix seconds at which the UI should start showing the incoming track.
    #[serde(default)]
    pub ui_switch_secs: f64,
    #[serde(default)]
    pub from_vol: Vec<MixVolSegment>,
    #[serde(default)]
    pub to_vol: Vec<MixVolSegment>,
    #[serde(default)]
    pub from_lp: Vec<MixVolSegment>,
    #[serde(default)]
    pub from_hp: Vec<MixVolSegment>,
    #[serde(default)]
    pub to_lp: Vec<MixVolSegment>,
    #[serde(default)]
    pub to_hp: Vec<MixVolSegment>,
    #[serde(default)]
    pub from_speed: Vec<MixVolSegment>,
    #[serde(default)]
    pub to_speed: Vec<MixVolSegment>,
}

/// Official BASS format plugins that are known to work reliably.
/// Third-party plugins (e.g. basszxtune.dll or other tracker/chiptune addons)
/// placed in the bass/ folder will also be auto-detected and attempted.
const BASS_FORMAT_PLUGINS: &[&str] = &[
    "bass_aac.dll",
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

/// Work item executed serially on the dedicated BASS thread.
type BassJob = Box<dyn FnOnce() + Send>;

// ── Public player handle ──────────────────────────────────────────────────────
#[derive(Clone)]
pub struct Player {
    inner: Arc<Mutex<PlayerInner>>,
    /// Serializes play/pause/resume/seek so concurrent IPC calls cannot deadlock the main thread.
    ops: Arc<Mutex<()>>,
    app: Arc<RwLock<Option<AppHandle>>>,
    bass_thread: Arc<RwLock<Option<thread::ThreadId>>>,
    /// Jobs for the dedicated BASS thread. All `BASS_*` calls (including the
    /// blocking `StreamCreateURL` connect) run there — never on the UI thread
    /// and never overlapping from a Tauri worker.
    bass_jobs: mpsc::Sender<BassJob>,
    discord: Arc<RwLock<Option<DiscordPresence>>>,
    /// True while the main window is focused — drives UI event rate (games-friendly when false).
    ui_hot: Arc<AtomicBool>,
    /// True while a decode source is loaded. Position-poll sleeps long and skips
    /// BASS hops when false (idle app / stopped).
    source_active: Arc<AtomicBool>,
    /// Set after the first successful `BASS_Init`. Ctrl+R re-calls `player_init`
    /// and must not hop to the BASS thread (it may be connecting a URL).
    bass_ready: Arc<AtomicBool>,
    /// Same leaked rack as `PlayerInner::chain`, held here so the settings UI can
    /// read its atomics without taking the player lock or hopping to the BASS
    /// thread ten times a second.
    chain: &'static DspChain,
}

impl Player {
    pub fn new() -> Self {
        // Leaked exactly once: BASS holds a raw `user` pointer to this for the
        // process lifetime. Individual effects inside it are plain `Arc`s.
        let chain: &'static DspChain = Box::leak(Box::new(DspChain::new()));
        let bass_thread = Arc::new(RwLock::new(None));
        let (bass_jobs, bass_rx) = mpsc::channel::<BassJob>();
        let bass_thread_for_spawn = Arc::clone(&bass_thread);
        let _ = thread::Builder::new()
            .name("muzeeka-bass".into())
            .spawn(move || {
                *bass_thread_for_spawn.write() = Some(thread::current().id());
                crate::process_util::register_audio_thread();
                while let Ok(job) = bass_rx.recv() {
                    job();
                }
            });
        Self {
            inner: Arc::new(Mutex::new(PlayerInner {
                bass: None,
                bass_dir: PathBuf::new(),
                mixer_handle: 0,
                current_source: 0,
                chain_dsp_handle: 0,
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
                chain,
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
                armed_mix: None,
                extra_outputs: Vec::new(),
                extra_devices_inited: Vec::new(),
                next_extra_id: 1,
                live_source: None,
                live_bytes_fed: 0,
                live_last_pump_log_ms: 0,
                live_zero_pulls: 0,
                live_pcm_rate: 44100,
                live_pcm_chans: 2,
                live_reconnect_after_ms: 0,
                live_meta_sync: 0,
                live_meta_inbox: Arc::new(StdMutex::new(LiveMetaInbox::default())),
                live_meta_user: 0,
                icy_tap_stop: None,
                icy_tap_join: None,
            })),
            ops: Arc::new(Mutex::new(())),
            app: Arc::new(RwLock::new(None)),
            bass_thread,
            bass_jobs,
            discord: Arc::new(RwLock::new(None)),
            ui_hot: Arc::new(AtomicBool::new(true)),
            source_active: Arc::new(AtomicBool::new(false)),
            bass_ready: Arc::new(AtomicBool::new(false)),
            chain,
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
        // A manual switch ends any transition in flight. Both decks belong to the mix
        // session, but the fast paths below only ever release `current_source` — the
        // incoming deck would keep playing underneath the newly opened track.
        Self::clear_mix_crossfade(inner);

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
        crate::stream_debug::set_app(app.clone());
        crate::stream_debug::log(format!(
            "stream log → {}",
            crate::stream_debug::log_path().display()
        ));
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

        let inner = Arc::clone(&self.inner);
        let (tx, rx) = mpsc::sync_channel(1);
        self.bass_jobs
            .send(Box::new(move || {
                let mut guard = inner.lock();
                let _ = tx.send(f(&mut guard));
            }))
            .map_err(|_| "BASS thread is gone".to_string())?;

        rx.recv()
            .map_err(|_| "BASS thread did not respond".to_string())?
    }

    /// Initialize the BASS audio system. Must be called before any playback.
    pub fn init(&self) -> Result<(), String> {
        if self.bass_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = self.run_on_bass_thread(Self::init_inner);
        if result.is_ok() {
            self.bass_ready.store(true, Ordering::Release);
        }
        result
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
        let float_dsp_ok = bass.set_config(bass::BASS_CONFIG_FLOATDSP, 1).is_ok();

        match bass.init(-1, 44100) {
            Ok(()) => {}
            Err(e) => {
                if bass.last_error() != bass::BassError::Already {
                    return Err(e);
                }
            }
        }

        // Device playback buffer. 300ms absorbs short decode stalls
        // (Ctrl+R bootstrap, metadata, Discord) without audible dropouts. Was 200ms
        // and could dip into silence under load. Update period 15ms keeps latency OK.
        let _ = bass.set_config(bass::BASS_CONFIG_BUFFER, 300);
        let _ = bass.set_config(bass::BASS_CONFIG_UPDATEPERIOD, 15);

        // Internet radio. Mixer never GetData's the URL (we pump on this thread),
        // so a short READTIMEOUT is only a safety net if we pull with an empty
        // download buffer — that used to EOF the live stream after ~0.5s.
        let _ = bass.set_config(bass::BASS_CONFIG_NET_META, 1);
        let _ = bass.set_config(bass::BASS_CONFIG_NET_TIMEOUT, 8000);
        // 0 = wait for the next Icecast block instead of EOF'ing the decode
        // stream. Mixer pulls at device rate, so this does not drain the buffer.
        let _ = bass.set_config(bass::BASS_CONFIG_NET_READTIMEOUT, 0);
        let _ = bass.set_config(bass::BASS_CONFIG_NET_BUFFER, 8000);
        let _ = bass.set_config(bass::BASS_CONFIG_NET_PREBUF, 20);
        let _ = bass.set_config(bass::BASS_CONFIG_NET_PLAYLIST, 1);
        static NET_AGENT: OnceLock<CString> = OnceLock::new();
        let agent = NET_AGENT.get_or_init(|| {
            CString::new("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Muzeeka/1.0")
                .expect("user-agent")
        });
        let _ = bass.set_config_ptr(
            bass::BASS_CONFIG_NET_AGENT,
            agent.as_ptr().cast(),
        );

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

        inner.chain.set_float_dsp_enabled(float_dsp_ok);
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
                    crate::stream_debug::log(format!("plugin loaded: {plugin}"));
                    inner._plugin_handles.push(handle);
                    attempted.insert(plugin.to_lowercase());
                }
                Err(error) => {
                    eprintln!("BASS plugin not loaded: {plugin} ({error})");
                    crate::stream_debug::log(format!(
                        "plugin {plugin} not loaded: {error}"
                    ));
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
        Self::reattach_extra_outputs(inner);
        Ok(())
    }

    pub fn list_output_devices(&self) -> Result<Vec<OutputDevice>, String> {
        self.run_on_bass_thread(|inner| {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Ok(bass
                .list_devices()
                .into_iter()
                .map(|d| OutputDevice {
                    id: d.id,
                    name: d.name,
                    driver: d.driver,
                    enabled: d.flags & bass::BASS_DEVICE_ENABLED != 0,
                    is_default: d.flags & bass::BASS_DEVICE_DEFAULT != 0,
                    initialized: d.flags & bass::BASS_DEVICE_INIT != 0,
                })
                .collect())
        })
    }

    pub fn extra_outputs(&self) -> Result<Vec<ExtraOutputInfo>, String> {
        self.run_on_bass_thread(|inner| {
            Ok(inner
                .extra_outputs
                .iter()
                .map(|o| ExtraOutputInfo {
                    id: o.id.clone(),
                    device_id: o.device,
                    name: o.name.clone(),
                })
                .collect())
        })
    }

    pub fn add_extra_output(&self, device_id: i32) -> Result<ExtraOutputInfo, String> {
        self.run_on_bass_thread(move |inner| Self::add_extra_output_inner(inner, device_id))
    }

    pub fn remove_extra_output(&self, output_id: &str) -> Result<(), String> {
        let output_id = output_id.to_string();
        self.run_on_bass_thread(move |inner| Self::remove_extra_output_inner(inner, &output_id))
    }

    fn add_extra_output_inner(
        inner: &mut PlayerInner,
        device_id: i32,
    ) -> Result<ExtraOutputInfo, String> {
        if device_id <= 0 {
            return Err("Invalid output device".into());
        }
        if inner.mixer_handle == 0 {
            Self::create_mixer(inner)?;
        }
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let default_device = bass.get_device();
        if device_id == default_device {
            return Err(
                "That is already the main output. Pick a different device (VB-Cable, VoiceMeeter, another DAC)."
                    .into(),
            );
        }
        if inner.extra_outputs.iter().any(|o| o.device == device_id) {
            let existing = inner
                .extra_outputs
                .iter()
                .find(|o| o.device == device_id)
                .expect("checked");
            return Ok(ExtraOutputInfo {
                id: existing.id.clone(),
                device_id: existing.device,
                name: existing.name.clone(),
            });
        }

        let devices = bass.list_devices();
        let info = devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or_else(|| format!("Output device {device_id} not found"))?;
        if info.flags & bass::BASS_DEVICE_ENABLED == 0 {
            return Err(format!("Output device '{}' is disabled", info.name));
        }

        Self::ensure_device_inited(inner, device_id)?;
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let split = bass.split_stream_create(inner.mixer_handle, 0)?;
        if let Err(err) = bass.channel_set_device(split, device_id) {
            let _ = bass.channel_free(split);
            return Err(err);
        }
        if let Err(err) = bass.channel_play(split, false) {
            let _ = bass.channel_free(split);
            return Err(err);
        }

        let id = format!("out-{}", inner.next_extra_id);
        inner.next_extra_id += 1;
        let extra = ExtraOutput {
            id: id.clone(),
            device: device_id,
            name: info.name.clone(),
            split_handle: split,
        };
        inner.extra_outputs.push(extra);
        Ok(ExtraOutputInfo {
            id,
            device_id,
            name: info.name.clone(),
        })
    }

    fn remove_extra_output_inner(inner: &mut PlayerInner, output_id: &str) -> Result<(), String> {
        let Some(index) = inner.extra_outputs.iter().position(|o| o.id == output_id) else {
            return Err(format!("Unknown extra output {output_id}"));
        };
        let extra = inner.extra_outputs.remove(index);
        if let Some(bass) = inner.bass.as_ref() {
            if extra.split_handle != 0 {
                let _ = bass.channel_stop(extra.split_handle);
                let _ = bass.channel_free(extra.split_handle);
            }
        }
        Ok(())
    }

    fn ensure_device_inited(inner: &mut PlayerInner, device_id: i32) -> Result<(), String> {
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        if inner.extra_devices_inited.contains(&device_id) {
            return Ok(());
        }
        let current = bass.get_device();
        match bass.init(device_id, 44100) {
            Ok(()) => {}
            Err(err) => {
                if bass.last_error() != bass::BassError::Already {
                    let _ = bass.set_device(current);
                    return Err(err);
                }
            }
        }
        let _ = bass.set_device(current);
        if !inner.extra_devices_inited.contains(&device_id) {
            inner.extra_devices_inited.push(device_id);
        }
        Ok(())
    }

    fn reattach_extra_outputs(inner: &mut PlayerInner) {
        let devices: Vec<(String, i32)> = inner
            .extra_outputs
            .iter()
            .map(|o| (o.id.clone(), o.device))
            .collect();
        for extra in inner.extra_outputs.drain(..) {
            if extra.split_handle != 0 {
                if let Some(bass) = inner.bass.as_ref() {
                    let _ = bass.channel_free(extra.split_handle);
                }
            }
        }
        for (_id, device) in devices {
            if let Err(err) = Self::add_extra_output_inner(inner, device) {
                eprintln!("[audio] reattach extra output {device}: {err}");
            }
        }
    }

    fn teardown_extra_outputs(inner: &mut PlayerInner) {
        if let Some(bass) = inner.bass.as_ref() {
            for extra in inner.extra_outputs.drain(..) {
                if extra.split_handle != 0 {
                    let _ = bass.channel_stop(extra.split_handle);
                    let _ = bass.channel_free(extra.split_handle);
                }
            }
            let current = bass.get_device();
            for device in inner.extra_devices_inited.drain(..) {
                if device == current {
                    continue;
                }
                if bass.set_device(device).is_ok() {
                    let _ = bass.free();
                }
            }
            let _ = bass.set_device(current);
        } else {
            inner.extra_outputs.clear();
            inner.extra_devices_inited.clear();
        }
    }

    pub fn get_dsp_chain(&self) -> Vec<ChainSlotSettings> {
        self.chain.settings()
    }

    /// Rack readout. Deliberately lock-free: the settings UI polls this at ~10 Hz
    /// for the limiter meter, and a hop to the BASS thread would queue behind
    /// whatever long operation (stream open, seek) is in flight.
    pub fn get_dsp_chain_status(&self) -> DspChainStatus {
        self.chain.status()
    }

    pub fn set_dsp_chain(&self, slots: Vec<ChainSlotSettings>) -> Result<(), String> {
        // Reorder, add, remove and every slider drag are all just an ArcSwap store
        // — no BASS call, so no main-thread hop. Only attaching or detaching the
        // one chain DSP needs the BASS thread.
        let wants_dsp = !slots.is_empty();
        if wants_dsp == self.chain.is_attached() {
            self.chain.apply(&slots);
            return Ok(());
        }
        self.run_on_bass_thread(move |inner| Self::set_dsp_chain_inner(inner, slots))
    }

    fn set_dsp_chain_inner(
        inner: &mut PlayerInner,
        slots: Vec<ChainSlotSettings>,
    ) -> Result<(), String> {
        inner.chain.apply(&slots);
        Self::sync_dsp_chain(inner);
        Ok(())
    }

    /// Make the mixer's DSP state match the rack: attached iff the rack is
    /// non-empty. Also called after a mixer rebuild, which drops DSPs silently.
    fn sync_dsp_chain(inner: &mut PlayerInner) {
        if inner.mixer_handle == 0 {
            return;
        }
        if inner.chain.is_empty() {
            if inner.chain_dsp_handle != 0 {
                Self::detach_chain(inner);
            }
        } else {
            let _ = Self::attach_chain_to_mixer(inner);
        }
    }

    fn detach_chain(inner: &mut PlayerInner) {
        if inner.chain_dsp_handle != 0 && inner.mixer_handle != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_remove_dsp(inner.mixer_handle, inner.chain_dsp_handle);
            }
        }
        inner.chain_dsp_handle = 0;
        inner.chain.set_dsp_float_forced(false);
        inner.chain.set_attached(false);
    }

    /// Attach the rack to the mixer at `BASS_DSP_PRIORITY_FIRST`.
    ///
    /// Priority no longer encodes effect order — the rack does that internally —
    /// so this only needs to be the first DSP the mixer runs, ahead of anything
    /// else that might be attached later.
    fn attach_chain_to_mixer(inner: &mut PlayerInner) -> Result<(), String> {
        if inner.mixer_handle == 0 {
            return Ok(());
        }
        Self::detach_chain(inner);

        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let info = bass.channel_get_info(inner.mixer_handle)?;
        let sample_rate = if info.freq > 0 {
            info.freq
        } else {
            bass.channel_get_attribute(inner.mixer_handle, bass::BASS_ATTRIB_FREQ)
                .unwrap_or(44100.0) as u32
        };
        let sample_rate = if sample_rate > 0 { sample_rate } else { 44100 };

        inner.chain.set_dsp_float_forced(true);
        inner
            .chain
            .configure_stream(sample_rate, info.chans, info.flags);

        let user = (inner.chain as *const DspChain) as *mut std::ffi::c_void;
        let dsp = match bass.channel_set_dsp_ex(
            inner.mixer_handle,
            chain_dsp_callback,
            user,
            bass::BASS_DSP_PRIORITY_FIRST,
            bass::BASS_DSP_FLOAT,
        ) {
            Ok(dsp) => dsp,
            Err(_) => {
                // Fallback path does not force float, so re-derive the buffer format.
                inner
                    .chain
                    .set_dsp_float_forced(info.flags & bass::BASS_SAMPLE_FLOAT != 0);
                inner
                    .chain
                    .configure_stream(sample_rate, info.chans, info.flags);
                bass.channel_set_dsp(
                    inner.mixer_handle,
                    chain_dsp_callback,
                    bass::BASS_DSP_PRIORITY_FIRST,
                    user,
                )?
            }
        };
        inner.chain_dsp_handle = dsp;
        inner.chain.set_attached(dsp != 0);
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

        // Internet radio: `BASS_StreamCreateURL` blocks during connect. That call
        // runs inside `play_inner` on the dedicated BASS thread (not the UI thread),
        // so the window stays alive while a slow/dead station times out.

        let cleared_mix = self.run_on_bass_thread(move |inner| {
            let was_mix = inner.mix_preview_active;
            inner.mix_preview_active = false;
            // A new manual play invalidates any armed transition; the frontend
            // re-arms for the new edge once the track is open.
            inner.armed_mix = None;
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
        // Count after BASS open succeeds (manual play / plugin / next-prev).
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
            // Never preload a live URL — StreamCreateURL would stall the BASS thread
            // for a station that isn't playing yet.
            if cue::is_stream_url(&track.audio_path) || cue::is_stream_url(&track.track_path) {
                Self::clear_preload(inner);
            } else if let Err(error) = Self::preload_next(inner, track) {
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
        // Two decks are live and `current_audio_path` may already describe the
        // incoming one (the display switches at the transition midpoint), so seeking
        // "the open stream" would move the wrong deck. Open the track cleanly.
        if inner.mix_crossfade.is_some() {
            return false;
        }
        // Live radio is not seekable — re-clicking a station must reconnect.
        if inner.live_source.is_some() || cue::is_stream_url(audio_path) {
            return false;
        }
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
        Self::abort_icy_tap(inner);
        inner.live_bytes_fed = 0;
        inner.live_last_pump_log_ms = 0;
        inner.live_zero_pulls = 0;
        inner.live_reconnect_after_ms = 0;
        let live_handle = if inner.current_decode != 0 {
            inner.current_decode
        } else {
            inner.current_source
        };
        if inner.live_meta_sync != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                if live_handle != 0 {
                    let _ = bass.channel_remove_sync(live_handle, inner.live_meta_sync);
                }
            }
            inner.live_meta_sync = 0;
        }
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
        inner.live_source = None;
        if inner.live_meta_user != 0 {
            unsafe {
                drop(Box::from_raw(inner.live_meta_user as *mut LiveMetaUser));
            }
            inner.live_meta_user = 0;
        }
    }

    /// Signal the sidecar ICY tap to exit. Do not join — the BASS thread must
    /// not block on a socket read. The tap notices `stop` on the next chunk.
    fn abort_icy_tap(inner: &mut PlayerInner) {
        if let Some(stop) = inner.icy_tap_stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        let _ = inner.icy_tap_join.take();
        if let Ok(mut inbox) = inner.live_meta_inbox.lock() {
            inbox.gen = inbox.gen.wrapping_add(1);
            inbox.meta = None;
            inbox.icy = None;
            inbox.http = None;
        }
    }

    /// Start (or restart, if the previous thread exited) the ICY sidecar.
    fn ensure_icy_tap(inner: &mut PlayerInner) {
        let Some(url) = inner.live_source.as_ref().map(|l| l.url.clone()) else {
            return;
        };
        if inner
            .icy_tap_join
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return;
        }
        let _ = inner.icy_tap_join.take();
        if let Some(stop) = inner.icy_tap_stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        let stop = Arc::new(AtomicBool::new(false));
        inner.icy_tap_stop = Some(Arc::clone(&stop));
        let inbox = Arc::clone(&inner.live_meta_inbox);
        let gen = inner
            .live_meta_inbox
            .lock()
            .map(|g| g.gen)
            .unwrap_or(0);
        inner.icy_tap_join = Some(icy_tap::spawn(url, inbox, stop, gen));
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
    /// Value to hold when no block covers `mix_secs`.
    ///
    /// DAW-style: the last block that already ended keeps its final value, and a
    /// block still in the future pre-applies its first value. Without this a
    /// fade-out would snap back to unity the instant its block ends — the
    /// outgoing track would blare back in right after the mix.
    fn mix_env_hold_v(segments: &[MixVolSegment], mix_secs: f64) -> Option<f32> {
        let mut end_t = f64::NEG_INFINITY;
        let mut end_v = 1.0f32;
        let mut have_end = false;
        let mut start_t = f64::INFINITY;
        let mut start_v = 1.0f32;
        let mut have_start = false;
        for seg in segments {
            let dur = seg.duration_secs.max(1e-6);
            let end = seg.start_secs + dur;
            if end <= mix_secs {
                if !have_end || end > end_t {
                    end_t = end;
                    end_v = Self::sample_mix_envelope(&seg.points, 1.0, seg.curve);
                    have_end = true;
                }
            } else if seg.start_secs >= mix_secs && (!have_start || seg.start_secs < start_t) {
                start_t = seg.start_secs;
                start_v = Self::sample_mix_envelope(&seg.points, 0.0, seg.curve);
                have_start = true;
            }
        }
        if have_end {
            Some(end_v)
        } else if have_start {
            Some(start_v)
        } else {
            None
        }
    }

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
            Self::mix_env_hold_v(segments, mix_secs)
                .unwrap_or(1.0)
                .clamp(0.0, 1.0)
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
            Self::mix_env_hold_v(segments, mix_secs)
                .map(Self::envelope_v_to_rate)
                .unwrap_or(1.0)
                .clamp(0.25, 2.0)
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
            // Hold the neighbouring block's edge value instead of bypassing, so a
            // filter sweep doesn't jump wide open the moment its block ends.
            Self::mix_env_hold_v(segments, mix_secs).map(|v| map_v(v as f64) as f32)
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
        let (pending, to_cue, from_cue, from_source, from_decode, graph_delay) = {
            let Some(mix) = inner.mix_crossfade.as_mut() else {
                return;
            };
            (
                mix.pending_to.take(),
                mix.next_cue_start.unwrap_or(0.0).max(0.0),
                mix.from_cue_start.max(0.0),
                mix.from_source,
                mix.from_decode,
                mix.to_graph_delay_secs.max(0.0),
            )
        };
        let Some(pending) = pending else {
            return;
        };
        if pending.source == 0 {
            return;
        }
        let rate_f = (inner.playback_rate as f64).clamp(0.25, 2.0);
        // Where the incoming deck belongs relative to the outgoing deck's live decoding
        // point. The mix clock is wall-clock based and so ignores the mixer buffer, but a
        // channel plugged into a running mixer joins at the decode point — reading the
        // outgoing deck there is what keeps the two in step. Recomputed after the add,
        // since the mixer keeps pulling while the channel is being attached.
        let aligned_abs = |bass: &BassLibrary| -> f64 {
            if from_source == 0 {
                return to_cue;
            }
            Self::content_decode_secs(bass, from_source, from_decode)
                .map(|from_abs| {
                    Self::aligned_mix_deck_abs(from_abs, from_cue, to_cue, graph_delay, rate_f)
                })
                .map(|abs| abs.max(to_cue))
                .unwrap_or(to_cue)
        };
        // Park at the exact graph time before the mixer eats samples.
        if let Some(bass) = inner.bass.as_ref() {
            let at = aligned_abs(bass);
            Self::seek_content_absolute(bass, pending.source, pending.decode, false, at);
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
                let at = aligned_abs(bass);
                Self::seek_content_absolute(bass, pending.source, pending.decode, true, at);
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

    /// Hand the *display* over to the incoming track mid-transition.
    ///
    /// Halfway through the transition block the incoming track is the one the listener
    /// is following, so the UI should say so while the outgoing deck plays its tail
    /// underneath. Only display-facing state moves: both decks keep running and the
    /// real handoff still happens in `finish_mix_from_deck`.
    ///
    /// Returns the adopted path so the poller emits `player:track-changed` for it.
    fn adopt_mix_next_for_ui(inner: &mut PlayerInner) -> Option<String> {
        let (next_path, next_audio, cue_start, cue_end, to_src, to_dec) = {
            let mix = inner.mix_crossfade.as_mut()?;
            // Nothing to adopt until the incoming deck is actually audible.
            if mix.ui_switched || mix.to_source == 0 {
                return None;
            }
            mix.ui_switched = true;
            (
                mix.next_path.clone(),
                mix.next_audio_path.clone(),
                mix.next_cue_start,
                mix.next_cue_end,
                mix.to_source,
                mix.to_decode,
            )
        };

        inner.current_file = Some(next_path.clone());
        inner.current_audio_path = Some(next_audio);
        inner.cue_start = cue_start;
        inner.cue_end = cue_end;
        // Same test as `detect_cue_position_mode`, but against the incoming deck:
        // `current_source` is still the outgoing one until the handoff.
        inner.cue_pos_relative = match (cue_start, inner.bass.as_ref()) {
            (Some(start), Some(bass)) if start > 0.05 => {
                Self::content_absolute_secs(bass, to_src, to_dec, true) + 0.75 < start
            }
            _ => false,
        };
        Some(next_path)
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
                // The UI may have adopted this track at the transition midpoint and
                // already queued what follows it — replacing that with a one-entry
                // queue here would strand playback at the end of this track.
                let queue_ready = inner
                    .gapless_queue
                    .get(inner.gapless_queue_index)
                    .is_some_and(|t| Self::same_track_path(&t.track_path, &next_path));
                if !queue_ready {
                    inner.gapless_queue = vec![GaplessTrack {
                        track_path: next_path.clone(),
                        audio_path: mix.next_audio_path,
                        cue_start: mix.next_cue_start,
                        cue_end: mix.next_cue_end,
                    }];
                    inner.gapless_queue_index = 0;
                }
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

    /// Arm a saved editor transition on the track that is currently playing.
    ///
    /// Nothing happens until the playhead reaches `start_at_secs` — the position
    /// poller then adopts the live source as the outgoing deck (see
    /// `start_armed_mix`), so playlist mix mode never restarts the track.
    pub fn arm_mix(&self, mix: ArmedMix) -> Result<(), String> {
        if mix.from_path.trim().is_empty() || mix.to_path.trim().is_empty() {
            return Err("arm_mix: empty track path".to_string());
        }
        self.run_on_bass_thread(move |inner| {
            // The Mix Transition window owns the device during preview.
            if inner.mix_preview_active {
                return Ok(());
            }
            inner.armed_mix = Some(mix);
            Ok(())
        })
    }

    /// Drop any armed transition (mix mode off, edge changed, playlist switched).
    pub fn disarm_mix(&self) -> Result<(), String> {
        self.run_on_bass_thread(|inner| {
            inner.armed_mix = None;
            Ok(())
        })
    }

    /// End of the saved layout on the mix clock: how late the mix may still be
    /// entered and still be the transition the user drew.
    ///
    /// The frontend sends the layout's own span, which covers a bare transition
    /// container with no automation in it. Envelope ends are the floor for older
    /// payloads that predate the field.
    fn armed_mix_span_secs(armed: &ArmedMix) -> f64 {
        let lanes: [&Vec<MixVolSegment>; 8] = [
            &armed.from_vol,
            &armed.to_vol,
            &armed.from_lp,
            &armed.from_hp,
            &armed.to_lp,
            &armed.to_hp,
            &armed.from_speed,
            &armed.to_speed,
        ];
        lanes
            .iter()
            .flat_map(|lane| lane.iter())
            .map(|seg| seg.start_secs + seg.duration_secs.max(0.0))
            .fold(armed.span_secs.max(0.0), f64::max)
    }

    /// Has the armed transition reached its start on the outgoing track?
    fn poll_armed_mix(inner: &mut PlayerInner) {
        if inner.mix_preview_active || inner.user_paused || inner.mix_crossfade.is_some() {
            return;
        }
        let Some(start_at) = inner.armed_mix.as_ref().map(|a| a.start_at_secs) else {
            return;
        };
        let still_current = {
            let armed = inner.armed_mix.as_ref().unwrap();
            inner
                .current_file
                .as_deref()
                .is_some_and(|cur| Self::same_track_path(cur, &armed.from_path))
        };
        if !still_current {
            inner.armed_mix = None;
            return;
        }
        if inner.current_source == 0 || inner.mixer_handle == 0 {
            return;
        }
        // Right after a manual play / seek BASS can report the previous position.
        if Self::now_millis() < inner.suppress_gapless_until {
            return;
        }
        let abs = {
            let Some(bass) = inner.bass.as_ref() else {
                return;
            };
            if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
                return;
            }
            // Decoding point, not heard point: envelope writes and the incoming deck
            // both act on the samples the mixer is filling right now, so the mix clock
            // has to start from that same content position (as it does in the preview,
            // where both decks are built before the mixer ever runs).
            Self::content_decode_secs(bass, inner.current_source, inner.current_decode)
                .unwrap_or_else(|| {
                    Self::content_absolute_secs(
                        bass,
                        inner.current_source,
                        inner.current_decode,
                        true,
                    )
                })
        };
        if abs + 0.02 < start_at {
            return;
        }
        // Poll granularity (and a forward seek into the zone) can land past the
        // layout origin — enter the mix that far in instead of replaying it late.
        let rate = inner.playback_rate.clamp(0.25, 2.0) as f64;
        let overshoot = ((abs - start_at) / rate).max(0.0);
        // Seeking clean past the whole layout means the transition is over: firing it
        // now would only hard-cut to the incoming deck. Let gapless handle the end.
        let span = inner
            .armed_mix
            .as_ref()
            .map(Self::armed_mix_span_secs)
            .unwrap_or(0.0);
        if overshoot > span + 0.5 {
            inner.armed_mix = None;
            return;
        }
        if let Err(error) = Self::start_armed_mix(inner, overshoot) {
            eprintln!("[mix] armed transition failed: {error}");
            inner.armed_mix = None;
        }
    }

    /// Turn the armed transition into a live two-deck mix around the running source.
    ///
    /// The outgoing deck is **not** re-opened: `current_source` keeps playing and
    /// simply becomes `from_source`, so there is no flush click at the cut. Only the
    /// incoming deck is created here. `overshoot_secs` is how far past mix time 0 the
    /// poll landed; the mix clock is back-dated by it so the saved layout still lines up.
    fn start_armed_mix(inner: &mut PlayerInner, overshoot_secs: f64) -> Result<(), String> {
        let from_source = inner.current_source;
        let from_decode = inner.current_decode;
        if from_source == 0 || inner.bass.is_none() || inner.mixer_handle == 0 {
            return Ok(());
        }
        let Some(armed) = inner.armed_mix.take() else {
            return Ok(());
        };

        let to_pb = cue::resolve_playback(
            &armed.to_path,
            armed.to_audio_path.as_deref(),
            armed.to_cue_start,
            armed.to_cue_end,
        )?;

        let over = overshoot_secs.max(0.0);
        let graph_delay = armed.to_delay_secs.max(0.0);
        let delay_left = (graph_delay - over).max(0.0);
        let to_base = to_pb.cue_start.unwrap_or(0.0).max(0.0);
        let from_cue = armed.start_at_secs.max(0.0);

        let rate = inner.playback_rate;
        let pitch = inner.pitch_enabled;
        let now = Self::now_millis();
        let rate_f = (rate as f64).clamp(0.25, 2.0);
        // Late past the incoming deck's entry → open it already that far in.
        // Graph time is consumed at playback rate, so content advances by rate·time.
        let to_start = to_base + (over - graph_delay).max(0.0) * rate_f;

        let decode = Self::create_decode_source(inner, &to_pb.audio_path, to_pb.cue_start)?;
        let (to_raw, to_raw_decode) = {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Self::wrap_decode_for_rate(bass, decode, rate, pitch)?
        };
        // Where the incoming deck belongs relative to the outgoing deck's live decoding
        // point. Re-read at every step rather than reused: opening and rate-wrapping the
        // stream takes real time and the mixer keeps pulling meanwhile, so seeding from
        // the poll's estimate is what let the incoming track enter late.
        let aligned_abs = |bass: &BassLibrary| -> f64 {
            Self::content_decode_secs(bass, from_source, from_decode)
                .map(|from_abs| {
                    Self::aligned_mix_deck_abs(from_abs, from_cue, to_base, graph_delay, rate_f)
                })
                .unwrap_or(to_start)
                .max(to_base)
        };
        if let Some(bass) = inner.bass.as_ref() {
            // A delayed deck is parked at its own start and re-snapped on injection.
            let at = if delay_left <= 0.001 {
                aligned_abs(bass)
            } else {
                to_start
            };
            Self::seek_content_absolute(bass, to_raw, to_raw_decode, false, at);
        }

        // Mixer is already running — the incoming deck joins it mid-flight.
        let mut to_source = 0u32;
        let mut to_decode = 0u32;
        let mut pending_to = None;
        if delay_left <= 0.001 {
            if Self::add_source_to_mixer(inner, to_raw, true).is_err() {
                if let Some(bass) = inner.bass.as_ref() {
                    Self::free_playback_channel(bass, to_raw, to_raw_decode);
                }
                return Err("Failed to add mix deck to mixer".to_string());
            }
            to_source = to_raw;
            to_decode = to_raw_decode;
            if let Some(bass) = inner.bass.as_ref() {
                let g = Self::mix_gain_at(&armed.to_vol, over);
                let _ = bass.channel_set_attribute(to_raw, bass::BASS_ATTRIB_VOL, g);
                // Re-snap after plug-in: attaching the channel is not instant either.
                Self::seek_content_absolute(bass, to_raw, to_raw_decode, true, aligned_abs(bass));
            }
        } else {
            pending_to = Some(PendingMixDeck {
                source: to_raw,
                decode: to_raw_decode,
            });
        }

        // The mix owns both decks now — normal gapless must not race it.
        Self::clear_preload(inner);
        inner.pending_next = None;
        inner.gapless_queue.clear();
        inner.gapless_queue_index = 0;
        inner.mix_vol_follow = None;

        let from_filter = if !armed.from_lp.is_empty() || !armed.from_hp.is_empty() {
            inner
                .bass
                .as_ref()
                .and_then(|b| Self::attach_deck_filter(b, from_source))
        } else {
            None
        };
        let to_filter = if to_source != 0 && (!armed.to_lp.is_empty() || !armed.to_hp.is_empty()) {
            inner
                .bass
                .as_ref()
                .and_then(|b| Self::attach_deck_filter(b, to_source))
        } else {
            None
        };
        if let Some(ref f) = from_filter {
            f.ctx.set_targets(
                Self::mix_filter_hz_at(&armed.from_lp, over, mix_filter::envelope_v_to_lp_hz, true),
                Self::mix_filter_hz_at(&armed.from_hp, over, mix_filter::envelope_v_to_hp_hz, false),
            );
        }
        if let Some(ref f) = to_filter {
            f.ctx.set_targets(
                Self::mix_filter_hz_at(&armed.to_lp, over, mix_filter::envelope_v_to_lp_hz, true),
                Self::mix_filter_hz_at(&armed.to_hp, over, mix_filter::envelope_v_to_hp_hz, false),
            );
        }

        inner.mix_crossfade = Some(MixCrossfadeState {
            from_source,
            from_decode,
            to_source,
            to_decode,
            from_duration_secs: armed.from_duration_secs.max(0.05),
            to_delay_secs: if pending_to.is_some() { delay_left } else { 0.0 },
            to_graph_delay_secs: graph_delay,
            from_cue_start: from_cue,
            // Back-date so a late poll doesn't shift the whole saved layout.
            mix_timeline_start_ms: now.saturating_sub((over * 1000.0).round().max(0.0) as u64),
            from_ended_mix_secs: 0.0,
            from_ended_at_ms: 0,
            pending_to,
            next_path: armed.to_path,
            next_audio_path: to_pb.audio_path,
            next_cue_start: to_pb.cue_start,
            next_cue_end: to_pb.cue_end,
            ui_switch_secs: Some(armed.ui_switch_secs.max(0.0)),
            ui_switched: false,
            from_vol: armed.from_vol,
            to_vol: armed.to_vol,
            from_lp: armed.from_lp,
            from_hp: armed.from_hp,
            to_lp: armed.to_lp,
            to_hp: armed.to_hp,
            from_speed: armed.from_speed,
            to_speed: armed.to_speed,
            from_filter,
            to_filter,
        });
        // Seed gains / cutoffs / rates at the current mix time before the next tick.
        Self::apply_mix_volume_automation(inner);
        Ok(())
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
            inner.armed_mix = None;
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
                // Editor preview: the Mix Transition window drives its own display,
                // and the main window is frozen for the duration.
                ui_switch_secs: None,
                ui_switched: false,
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
        // Internet radio / web streams: open a URL decode stream. These have no
        // finite length, never prescan and are never seeked — return early before
        // the file-based path below (which waits for a length and honors cue_start).
        // Runs on the dedicated BASS thread; connect is bounded by NET_TIMEOUT.
        if cue::is_stream_url(audio_path) {
            return Self::open_url_decode(inner, audio_path.trim());
        }

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
        // DOWNMIX is a no-op for stereo sources and saves surround radio streams.
        let add_flags = if norampin {
            bass::BASS_MIXER_CHAN_NORAMPIN | bass::BASS_MIXER_CHAN_DOWNMIX
        } else {
            bass::BASS_MIXER_CHAN_DOWNMIX
        };
        bass.mixer_stream_add_channel(inner.mixer_handle, source, add_flags)?;
        Ok(())
    }

    fn log_url_stream_state(bass: &BassLibrary, handle: u32, label: &str) {
        crate::stream_debug::log(format!("--- {label} handle={handle} ---"));
        match bass.channel_get_info(handle) {
            Ok(info) => crate::stream_debug::log(format!(
                "info freq={} chans={} flags=0x{:x} ctype=0x{:x} origres={}",
                info.freq, info.chans, info.flags, info.ctype, info.origres
            )),
            Err(e) => crate::stream_debug::log(format!("info FAILED: {e}")),
        }
        let active = bass.channel_is_active(handle);
        let avail = bass.channel_data_available(handle);
        let buffer = bass.stream_get_file_position(handle, bass::BASS_FILEPOS_BUFFER);
        let download = bass.stream_get_file_position(handle, bass::BASS_FILEPOS_DOWNLOAD);
        let connected = bass.stream_get_file_position(handle, bass::BASS_FILEPOS_CONNECTED);
        crate::stream_debug::log(format!(
            "active={active} available={avail} filepos buffer={buffer} download={download} connected={connected}"
        ));
        unsafe {
            match bass.channel_get_tags_raw(handle, bass::BASS_TAG_HTTP) {
                Some(s) => crate::stream_debug::log(format!(
                    "HTTP: {}",
                    s.replace('\0', " | ")
                )),
                None => crate::stream_debug::log(format!(
                    "HTTP tags: none ({})",
                    bass.last_error_string()
                )),
            }
            if let Some(s) = bass.channel_get_tags_raw(handle, bass::BASS_TAG_ICY) {
                crate::stream_debug::log(format!("ICY: {}", s.replace('\0', " | ")));
            }
            if let Some(s) = bass.channel_get_tags_raw(handle, bass::BASS_TAG_META) {
                crate::stream_debug::log(format!("META: {s}"));
            }
        }
    }

    /// Icecast/Shoutcast decode streams die if we GetData faster than the
    /// download. Plug the URL into the mixer and let it pull at device rate.
    fn maybe_reconnect_live_url(inner: &mut PlayerInner) {
        if inner.user_paused {
            return;
        }
        let Some(url) = inner.live_source.as_ref().map(|l| l.url.clone()) else {
            return;
        };
        let handle = if inner.current_decode != 0 {
            inner.current_decode
        } else {
            inner.current_source
        };
        if handle == 0 {
            return;
        }
        let ended = inner.bass.as_ref().is_some_and(|bass| {
            bass.channel_is_active(handle) == bass::BASS_ACTIVE_STOPPED
        });
        if !ended {
            return;
        }
        let now = Self::now_millis();
        if now < inner.live_reconnect_after_ms {
            return;
        }
        inner.live_reconnect_after_ms = now.saturating_add(2500);
        crate::stream_debug::log("live URL ended — reconnecting");
        if let Some(bass) = inner.bass.as_ref() {
            let _ = bass.mixer_channel_remove(handle);
            let _ = bass.channel_free(handle);
        }
        inner.current_source = 0;
        inner.current_decode = 0;
        match Self::open_url_decode(inner, &url) {
            Ok(new) => match Self::plug_live_url(inner, new, inner.playback_rate) {
                Ok((src, dec)) => {
                    inner.current_source = src;
                    inner.current_decode = dec;
                    crate::stream_debug::log(format!("reconnected handle={src}"));
                }
                Err(e) => crate::stream_debug::log(format!("reconnect plug failed: {e}")),
            },
            Err(e) => crate::stream_debug::log(format!("reconnect open failed: {e}")),
        }
    }

    /// Add a live URL decode stream to the mixer. Do not GetData it ourselves —
    /// draining the download buffer EOF's Icecast after a few seconds.
    fn plug_live_url(
        inner: &mut PlayerInner,
        url_handle: u32,
        rate: f32,
    ) -> Result<(u32, u32), String> {
        {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            Self::log_url_stream_state(bass, url_handle, "plug live URL");
            if (rate - 1.0).abs() >= 0.001 {
                Self::apply_freq_rate(bass, url_handle, rate);
            }
        }
        Self::add_source_to_mixer(inner, url_handle, false).map_err(|e| {
            crate::stream_debug::log(format!("mixer add URL FAILED: {e}"));
            e
        })?;
        crate::stream_debug::log(format!("mixer add URL handle={url_handle} OK"));
        Ok((url_handle, 0))
    }

    /// Open an internet radio / web stream as a decode source.
    ///
    /// Tries float first (matches the mixer), then integer if the server/codec
    /// rejects it. After connect, wait briefly so headers/codec are parsed —
    /// adding an unconfigured DECODE stream to the mixer makes ChannelGetData
    /// stall the BASS update thread (no audio, Ctrl+R hangs).
    fn open_url_decode(inner: &mut PlayerInner, url: &str) -> Result<u32, String> {
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        crate::stream_debug::log(format!("StreamCreateURL begin {url}"));
        let started = std::time::Instant::now();
        let request = if url.contains("\r\n") {
            url.to_string()
        } else {
            format!("{}\r\nIcy-MetaData: 1\r\n", url.trim())
        };
        if inner.live_meta_user != 0 {
            unsafe {
                drop(Box::from_raw(inner.live_meta_user as *mut LiveMetaUser));
            }
            inner.live_meta_user = 0;
        }
        let user = Box::into_raw(Box::new(LiveMetaUser {
            get_tags: bass.tags_fn(),
            inbox: Arc::clone(&inner.live_meta_inbox),
            icy: StdMutex::new(IcyParse::default()),
        }));
        inner.live_meta_user = user as usize;
        let proc = live_download_proc as *mut std::ffi::c_void;
        let opener = bass.url_opener();
        let float_flags = bass::BASS_SAMPLE_FLOAT | bass::BASS_STREAM_DECODE;
        let source = match opener.open_proc(&request, float_flags, proc, user.cast()) {
            Ok(handle) => {
                crate::stream_debug::log(format!(
                    "float URL handle={handle} in {} ms",
                    started.elapsed().as_millis()
                ));
                handle
            }
            Err(error) => {
                crate::stream_debug::log(format!(
                    "float URL FAILED after {} ms: {error} — retry integer PCM",
                    started.elapsed().as_millis()
                ));
                let retry_at = std::time::Instant::now();
                opener
                    .open_proc(&request, bass::BASS_STREAM_DECODE, proc, user.cast())
                    .map_err(|e| {
                        crate::stream_debug::log(format!(
                            "integer URL FAILED after {} ms: {e} — {url}",
                            retry_at.elapsed().as_millis()
                        ));
                        format!("{e} — url: {url}")
                    })?
            }
        };
        match bass.channel_set_sync(
            source,
            bass::BASS_SYNC_META,
            live_meta_sync_proc,
            user.cast(),
        ) {
            Ok(sync) => {
                inner.live_meta_sync = sync;
                crate::stream_debug::log("BASS_SYNC_META attached");
            }
            Err(e) => crate::stream_debug::log(format!("BASS_SYNC_META failed: {e}")),
        }
        let _ = bass.channel_set_sync(
            source,
            bass::BASS_SYNC_OGG_CHANGE,
            live_meta_sync_proc,
            user.cast(),
        );
        Ok(source)
    }

    fn open_stream(
        inner: &mut PlayerInner,
        track_path: &str,
        playback: &PlaybackTarget,
    ) -> Result<(), String> {
        // Teardown previous source (remove from mixer).
        Self::teardown_current(inner);
        Self::clear_preload(inner);

        let is_live = cue::is_stream_url(&playback.audio_path);
        // Flush leftover file audio with silence *before* the URL connect so
        // ChannelPlay(restart) is not waiting on a stream that has no data yet.
        if is_live {
            Self::flush_mixer_hard(inner);
        }

        let decode = Self::create_decode_source(inner, &playback.audio_path, playback.cue_start)?;
        let rate = inner.playback_rate;
        let pitch_enabled = inner.pitch_enabled;
        let volume = inner.volume;
        let (mixer_channel, tracked_decode) = if is_live {
            Self::plug_live_url(inner, decode, rate)?
        } else {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            let wrapped = Self::wrap_source_for_playback(bass, decode, rate, pitch_enabled, false)?;
            Self::add_source_to_mixer(inner, wrapped.0, false)?;
            wrapped
        };

        if let Some(bass) = inner.bass.as_ref() {
            if is_live {
                // Do not linger at VOL=0: radio has no "first sample click" and a
                // failed fade-in is indistinguishable from a dead stream.
                if bass.channel_is_active(inner.mixer_handle) != bass::BASS_ACTIVE_PLAYING {
                    let play_ok = bass.channel_play(inner.mixer_handle, false);
                    crate::stream_debug::log(format!("mixer play(false) → {play_ok:?}"));
                }
                let vol_ok = bass.channel_set_attribute(
                    inner.mixer_handle,
                    bass::BASS_ATTRIB_VOL,
                    volume.clamp(0.0, 1.0),
                );
                crate::stream_debug::log(format!(
                    "mixer vol={} → {vol_ok:?} active={}",
                    volume,
                    bass.channel_is_active(inner.mixer_handle)
                ));
            } else {
                let _ = bass.channel_set_attribute(inner.mixer_handle, bass::BASS_ATTRIB_VOL, 0.0);
                Self::restart_mixer_with_buffer(bass, inner.mixer_handle);
                Self::set_mixer_volume_from_silence(
                    bass,
                    inner.mixer_handle,
                    volume,
                    MANUAL_SWITCH_FADE_IN_MS,
                );
            }
        }

        inner.current_source = mixer_channel;
        inner.current_decode = tracked_decode;
        inner.applied_playback_rate = rate;
        Self::sync_live_source(inner, &playback.audio_path);
        if is_live {
            Self::ensure_icy_tap(inner);
        }
        Self::apply_segment_metadata(inner, track_path, playback);
        Self::detect_cue_position_mode(inner);

        // The rack lives on the mixer, which survives track changes — but a
        // rebuilt mixer would drop the DSP silently.
        Self::sync_dsp_chain(inner);
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
        let (mixer_channel, tracked_decode) = Self::wrap_source_for_playback(
            bass,
            source,
            rate,
            pitch_enabled,
            cue::is_stream_url(&playback.audio_path),
        )?;

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
        let is_live = cue::is_stream_url(&playback.audio_path);
        let old_source = inner.current_source;
        let old_decode = inner.current_decode;
        let (mixer_channel, tracked_decode) = if is_live {
            Self::plug_live_url(inner, decode, rate)?
        } else {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            let wrapped = Self::wrap_source_for_playback(bass, decode, rate, pitch_enabled, false)?;
            Self::add_source_to_mixer(inner, wrapped.0, true)?;
            wrapped
        };

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
        Self::sync_live_source(inner, &playback.audio_path);
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
        let preloaded_decode = inner.preloaded_decode;

        // Snap to segment start before plugging in (content timeline).
        if !cue::is_stream_url(&playback.audio_path) {
            let start =
                Self::gapless_join_start_secs(&playback.audio_path, playback.cue_start);
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

        Self::sync_live_source(inner, &playback.audio_path);
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

    /// Mark (or clear) live-radio bookkeeping after a source is plugged in.
    fn sync_live_source(inner: &mut PlayerInner, audio_path: &str) {
        if cue::is_stream_url(audio_path) {
            inner.live_source = Some(LiveStreamState::new(audio_path.to_string()));
        } else {
            inner.live_source = None;
        }
    }

    /// Tempo wrappers read-ahead and can stall forever on an endless URL. Live
    /// radio only ever gets a FREQ rate change, never BASS_FX tempo.
    fn wrap_source_for_playback(
        bass: &BassLibrary,
        decode: u32,
        rate: f32,
        pitch_enabled: bool,
        is_live: bool,
    ) -> Result<(u32, u32), String> {
        if is_live {
            let _ = bass.channel_set_attribute(decode, bass::BASS_ATTRIB_FREQ, 0.0);
            if (rate - 1.0).abs() >= 0.001 {
                Self::apply_freq_rate(bass, decode, rate);
            }
            return Ok((decode, 0));
        }
        Self::wrap_decode_for_rate(bass, decode, rate, pitch_enabled)
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

    /// Read ICY tags from the active live stream and, when the now-playing title
    /// or the station name changes, return an update payload for the UI. Runs on
    /// the BASS thread; internally rate-limited so tag reads don't spin every tick.
    fn poll_live_metadata(inner: &mut PlayerInner) -> Option<StreamMetadataPayload> {
        // Minimum gap between ICY tag reads. Stations announce every track (tens of
        // seconds); 1s keeps changes near-instant without hammering the decoder.
        const ICY_PROBE_INTERVAL_MS: u64 = 1000;

        Self::ensure_icy_tap(inner);
        let now = Self::now_millis();
        let (dumped_meta, dumped_icy, dumped_http) = match inner.live_meta_inbox.lock() {
            Ok(mut g) => (g.meta.take(), g.icy.take(), g.http.take()),
            Err(_) => (None, None, None),
        };
        let from_sync = dumped_meta.is_some() || dumped_icy.is_some() || dumped_http.is_some();
        {
            let live = inner.live_source.as_ref()?;
            if !from_sync && now.saturating_sub(live.last_probe_ms) < ICY_PROBE_INTERVAL_MS {
                return None;
            }
        }
        if inner.current_source == 0 {
            return None;
        }
        let bass = inner.bass.as_ref()?;
        let handle = if inner.current_decode != 0 {
            inner.current_decode
        } else {
            inner.current_source
        };

        let meta = dumped_meta
            .or_else(|| unsafe { bass.channel_get_tags_raw(handle, bass::BASS_TAG_META) });
        let icy = dumped_icy
            .or_else(|| unsafe { bass.channel_get_tags_raw(handle, bass::BASS_TAG_ICY) });
        let http = dumped_http
            .or_else(|| unsafe { bass.channel_get_tags_raw(handle, bass::BASS_TAG_HTTP) });
        let ogg = unsafe { bass.channel_get_tags_raw(handle, bass::BASS_TAG_OGG) };
        let mp4 = unsafe { bass.channel_get_tags_raw(handle, bass::BASS_TAG_MP4) };

        if from_sync {
            crate::stream_debug::log(format!(
                "sync-tags META={:?} ICY={:?} HTTP={:?}",
                meta.as_deref(),
                icy.as_deref(),
                http.as_deref()
            ));
        }

        let title = meta
            .as_deref()
            .and_then(icy_tap::parse_icy_stream_title)
            .or_else(|| ogg.as_deref().and_then(|r| parse_comment_tag(r, "TITLE")))
            .or_else(|| mp4.as_deref().and_then(|r| parse_comment_tag(r, "©nam")))
            .or_else(|| mp4.as_deref().and_then(|r| parse_comment_tag(r, "title")));
        let title = match (
            title,
            ogg.as_deref().and_then(|r| parse_comment_tag(r, "ARTIST")),
        ) {
            (Some(t), Some(a)) if !t.contains(&a) => Some(format!("{a} - {t}")),
            (t, _) => t,
        };

        let station = {
            let have_station = inner
                .live_source
                .as_ref()
                .map(|l| l.last_station.is_some())
                .unwrap_or(false);
            if have_station {
                None
            } else {
                icy.as_deref()
                    .and_then(|r| parse_icy_header(r, "icy-name"))
                    .or_else(|| http.as_deref().and_then(|r| parse_icy_header(r, "icy-name")))
                    .or_else(|| icy.as_deref().and_then(|r| parse_icy_header(r, "ice-name")))
                    .or_else(|| http.as_deref().and_then(|r| parse_icy_header(r, "ice-name")))
            }
        };

        let live = inner.live_source.as_mut()?;
        live.last_probe_ms = now;

        let title_changed = title.is_some() && title != live.last_title;
        let station_changed = station.is_some() && station != live.last_station;
        if !title_changed && !station_changed {
            return None;
        }
        if title_changed {
            live.last_title = title.clone();
        }
        if station_changed {
            live.last_station = station.clone();
        }
        Some(StreamMetadataPayload {
            path: live.url.clone(),
            title: if title_changed {
                live.last_title.clone()
            } else {
                None
            },
            station: if station_changed {
                live.last_station.clone()
            } else {
                None
            },
        })
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
        Self::content_position_secs(bass, mixer_channel, tracked_decode, in_mixer)
            .unwrap_or(0.0)
    }

    /// Content position of a mixer source at the point the mixer has **decoded** to —
    /// the samples it is filling its playback buffer with, not the ones being heard.
    ///
    /// `BASS_Mixer_ChannelGetPosition` deliberately subtracts the mixer's playback
    /// buffer so it can report what is audible (and rejects `BASS_POS_DECODE`). The
    /// source's own `BASS_ChannelGetPosition` is the decoding point: the mixer pulls
    /// the source, so its position is exactly how far the mix has been rendered.
    ///
    /// A deck added to a running mixer joins there, and envelope writes land there
    /// too — both must be aligned against this, not against the heard position, which
    /// trails it by up to `MIXER_BUFFER_SECS`.
    fn content_decode_secs(
        bass: &BassLibrary,
        mixer_channel: u32,
        tracked_decode: u32,
    ) -> Option<f64> {
        Self::content_position_secs(bass, mixer_channel, tracked_decode, false)
    }

    fn content_position_secs(
        bass: &BassLibrary,
        mixer_channel: u32,
        tracked_decode: u32,
        in_mixer: bool,
    ) -> Option<f64> {
        if mixer_channel == 0 {
            return None;
        }
        let pos = if in_mixer {
            bass.mixer_channel_get_position(mixer_channel, bass::BASS_POS_BYTE)
        } else {
            bass.channel_get_position(mixer_channel, bass::BASS_POS_BYTE)
        };
        // BASS returns -1 on failure.
        if pos == u64::MAX {
            return None;
        }
        let decode = Self::decode_handle_for_channel(bass, mixer_channel, tracked_decode);
        let output_secs = bass
            .channel_bytes2seconds(mixer_channel, pos)
            .max(0.0);
        if decode == 0 || decode == mixer_channel {
            return Some(output_secs);
        }

        let output_duration = Self::stream_duration_secs(bass, mixer_channel);
        let content_duration = Self::stream_duration_secs(bass, decode);
        Some(Self::map_output_position_to_content(
            output_secs,
            output_duration,
            content_duration,
        ))
    }

    /// Where the incoming deck must sit, in absolute content seconds, so it lands at
    /// its graph position relative to the outgoing deck.
    ///
    /// Layout rule (identical in preview and playback): content(t) = cue + t·rate,
    /// with the incoming deck's timeline delayed by `graph_delay`. Substituting the
    /// outgoing deck's live decoding position for t gives
    ///
    ///   to_abs = from_abs + (to_cue − from_cue) − graph_delay·rate
    ///
    /// `graph_delay` is consumed at playback rate, so the product uses the real rate.
    fn aligned_mix_deck_abs(
        from_abs: f64,
        from_cue: f64,
        to_cue: f64,
        graph_delay: f64,
        rate: f64,
    ) -> f64 {
        from_abs + (to_cue - from_cue) - graph_delay * rate
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

        if inner.live_source.is_some() {
            return Ok(());
        }

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
        // Live radio: FREQ only. Tempo wrap / reopen would stall on the URL.
        if inner.live_source.is_some() {
            if let Some(bass) = inner.bass.as_ref() {
                if inner.current_source != 0 {
                    Self::apply_freq_rate(bass, inner.current_source, rate);
                }
            }
            inner.applied_playback_rate = rate;
            return;
        }

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
                if let Some(live) = inner.live_source.as_mut() {
                    live.mark_paused(Self::now_millis());
                }
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
            if let Some(live) = inner.live_source.as_mut() {
                live.mark_resumed(Self::now_millis());
            }
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
            inner.armed_mix = None;
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
        self.bass_ready.store(false, Ordering::Release);
        self.run_on_bass_thread(|inner| {
            Self::teardown_current(inner);
            Self::clear_preload(inner);
            Self::teardown_extra_outputs(inner);
            if let Some(bass) = inner.bass.as_ref() {
                if inner.mixer_handle != 0 {
                    let _ = bass.channel_stop(inner.mixer_handle);
                }
                // Free releases the output device and stops any background audio threads.
                let _ = bass.free();
            }
            inner.mixer_handle = 0;
            inner.bass = None;
            inner.chain_dsp_handle = 0;
            inner.chain.set_attached(false);
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
            if inner.live_source.is_some() {
                // Live radio has no timeline to seek.
                return Ok(());
            }
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

            // While a transition runs, both decks are retuned every poll tick from
            // `playback_rate` × their speed envelope. Re-applying here would fight
            // that automation, and a topology rebuild would reopen a deck that is
            // audible right now — the mix picks the new base up on the next tick.
            if inner.mix_crossfade.is_some() || inner.mix_vol_follow.is_some() {
                inner.applied_playback_rate = rate;
                return Ok(());
            }

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
            // Once the display has moved to the incoming track (transition midpoint),
            // position and duration must come from its deck — otherwise the seekbar
            // shows the outgoing track's clock under the new title.
            let (src, dec) = if mix.ui_switched && mix.to_source != 0 {
                (mix.to_source, mix.to_decode)
            } else if mix.from_source != 0
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

        // Live radio stream: no finite length or seek. Position is elapsed wall-clock
        // since playback started (minus paused time); duration stays 0 so the UI
        // renders a live indicator instead of a seekbar. A stalled stream still
        // reports Playing (BASS is buffering) — never Stopped, which would advance.
        if let Some(live) = inner.live_source.as_ref() {
            if inner.current_source != 0 && inner.mixer_handle != 0 && inner.bass.is_some() {
                let file_name = inner.current_file.as_ref().map(|f| f.clone());
                let position = live.elapsed_secs(Self::now_millis());
                let (state, is_playing, is_paused) = if inner.user_paused {
                    (PlaybackState::Paused, false, true)
                } else {
                    (PlaybackState::Playing, true, false)
                };
                return PlayerStateSnapshot {
                    state,
                    is_playing,
                    is_paused,
                    volume: inner.volume,
                    position,
                    duration: 0.0,
                    current_file: inner.current_file.clone(),
                    current_file_name: file_name,
                };
            }
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

    /// Poll playback on the BASS thread and emit position / track-ended events.
    ///
    /// BASS must only be called from the thread that invoked `BASS_Init` (the
    /// dedicated `muzeeka-bass` thread). Uses one long-lived worker that hops
    /// there — never the UI/main thread.
    ///
    /// While a source is active, gapless polls at [`POLL_INTERVAL_MS`]. When idle
    /// (no source), the worker sleeps [`POLL_IDLE_MS`] and **skips** BASS hops
    /// so the audio thread is not poked 20×/s with an empty player.
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
                    Self::position_poll_tick(
                        &app_emit,
                        &player_for_main,
                        &was_for_main,
                        &rpc_for_main,
                        &rpc_sync_for_main,
                        &ui_emit_for_main,
                    );
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
            // Playlist mix mode: a saved transition may be due on this very tick.
            // Checked before gapless so the mix wins the end of the outgoing track.
            if inner.armed_mix.is_some() {
                Self::poll_armed_mix(inner);
            }
            // Mix timeline: inject next only when graph delay has elapsed
            // (never early when prev ends — that kills user-planned pauses).
            // Drop prev when its remaining length is done (audio time or STOPPED).
            if inner.mix_crossfade.is_some() {
                let (inject, drop_from, mix_secs, delay_left, ui_switch_due) = {
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
                    // Playlist mix mode: the display follows the incoming track from
                    // the middle of the transition, long before the outgoing deck dies.
                    let ui_switch_due = !mix.ui_switched
                        && mix.to_source != 0
                        && mix
                            .ui_switch_secs
                            .is_some_and(|at| mix_secs + 0.002 >= at);
                    (inject, drop_from, mix_secs, delay_left, ui_switch_due)
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
                    // The display already moved to this track mid-transition; a second
                    // track-changed would only reset its position back to zero.
                    let ui_done = inner
                        .mix_crossfade
                        .as_ref()
                        .is_some_and(|m| m.ui_switched);
                    let path = Self::finish_mix_from_deck(inner);
                    // Apply follow-volume immediately after handoff.
                    Self::apply_mix_volume_automation(inner);
                    return Ok(if ui_done { None } else { path });
                }
                if ui_switch_due {
                    let adopted = Self::adopt_mix_next_for_ui(inner);
                    Self::apply_mix_volume_automation(inner);
                    return Ok(adopted);
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
            // Live radio stream: never auto-advance, never tear down on transient
            // stalls. ICY now-playing metadata is polled separately (below) so the
            // emit can run with the AppHandle outside this BASS closure.
            if inner.live_source.is_some() {
                Self::maybe_reconnect_live_url(inner);
                return Ok(None);
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

        // Live radio: read ICY tags on the BASS thread and, if the now-playing
        // title or station name changed, push it to the UI. Rate-limited inside.
        if let Ok(Some(update)) =
            player_for_main.run_on_bass_thread(|inner| Ok(Self::poll_live_metadata(inner)))
        {
            let _ = app_emit.emit("player:stream-metadata", &update);
        }

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

unsafe extern "system" fn live_download_proc(
    buffer: *const std::ffi::c_void,
    length: u32,
    user: *mut std::ffi::c_void,
) {
    if user.is_null() || buffer.is_null() {
        return;
    }
    let cap = &*(user as *const LiveMetaUser);
    if length == 0 {
        if let Some(headers) =
            crate::bass::ffi::BassLibrary::copy_tag_ptr(buffer, true)
                .or_else(|| crate::bass::ffi::BassLibrary::copy_tag_ptr(buffer, false))
        {
            apply_download_headers(&headers, cap);
        }
        return;
    }
    let bytes = std::slice::from_raw_parts(buffer as *const u8, length as usize);
    if let Some(title) = scan_stream_title(bytes) {
        crate::stream_debug::log(format!("download StreamTitle='{title}'"));
        if let Ok(mut g) = cap.inbox.lock() {
            g.meta = Some(format!("StreamTitle='{title}';"));
        }
    }
    if let Ok(mut icy) = cap.icy.lock() {
        if icy.metaint == 0 {
            let peek = String::from_utf8_lossy(bytes);
            if peek.to_ascii_lowercase().contains("icy-metaint")
                || peek.to_ascii_lowercase().contains("icy-name")
            {
                apply_download_headers(&peek, cap);
            }
            return;
        }
        feed_icy_bytes(&mut icy, bytes, &cap.inbox);
    }
}

fn scan_stream_title(data: &[u8]) -> Option<String> {
    for key in [b"StreamTitle='".as_slice(), b"StreamTitle=\"".as_slice()] {
        let Some(pos) = data.windows(key.len()).position(|w| w == key) else {
            continue;
        };
        let quote = *key.last()?;
        let rest = &data[pos + key.len()..];
        let end = rest.iter().position(|&b| b == quote).unwrap_or(rest.len().min(200));
        if end == 0 {
            continue;
        }
        let bytes = &rest[..end];
        let title = if let Ok(s) = std::str::from_utf8(bytes) {
            s.trim().to_string()
        } else {
            bytes.iter().map(|&b| b as char).collect::<String>().trim().to_string()
        };
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

fn apply_download_headers(raw: &str, cap: &LiveMetaUser) {
    crate::stream_debug::log(format!(
        "download headers: {}",
        raw.replace('\0', " | ").chars().take(400).collect::<String>()
    ));
    if let Some(v) = parse_icy_header(raw, "icy-metaint").or_else(|| parse_icy_header(raw, "ice-metaint"))
    {
        if let Ok(n) = v.trim().parse::<u32>() {
            if n > 0 && n < 1_000_000 {
                if let Ok(mut icy) = cap.icy.lock() {
                    icy.metaint = n;
                    icy.audio_left = n;
                    icy.meta_left = 0;
                    icy.meta.clear();
                }
                crate::stream_debug::log(format!("icy-metaint={n}"));
            }
        }
    }
    if let Some(name) = parse_icy_header(raw, "icy-name").or_else(|| parse_icy_header(raw, "ice-name"))
    {
        crate::stream_debug::log(format!("icy-name={name}"));
        if let Ok(mut inbox) = cap.inbox.lock() {
            inbox.icy = Some(format!("icy-name:{name}"));
            inbox.http = Some(raw.to_string());
        }
    }
}

fn feed_icy_bytes(parse: &mut IcyParse, data: &[u8], inbox: &StdMutex<LiveMetaInbox>) {
    let metaint = parse.metaint;
    if metaint == 0 {
        return;
    }
    let mut i = 0usize;
    while i < data.len() {
        if parse.meta_left > 0 {
            let n = (parse.meta_left as usize).min(data.len() - i);
            parse.meta.extend_from_slice(&data[i..i + n]);
            parse.meta_left -= n as u32;
            i += n;
            if parse.meta_left == 0 {
                let raw = String::from_utf8_lossy(&parse.meta).into_owned();
                parse.meta.clear();
                parse.audio_left = metaint;
                if icy_tap::parse_icy_stream_title(&raw).is_some() {
                    crate::stream_debug::log(format!("ICY in-band META={raw}"));
                    if let Ok(mut g) = inbox.lock() {
                        g.meta = Some(raw);
                    }
                }
            }
            continue;
        }
        if parse.audio_left == 0 {
            let len = u32::from(data[i]) * 16;
            i += 1;
            if len == 0 {
                parse.audio_left = metaint;
            } else {
                parse.meta_left = len;
                parse.meta.clear();
                parse.meta.reserve(len as usize);
            }
            continue;
        }
        let n = (parse.audio_left as usize).min(data.len() - i);
        parse.audio_left -= n as u32;
        i += n;
    }
}

unsafe extern "system" fn live_meta_sync_proc(
    _sync: u32,
    channel: u32,
    data: *mut std::ffi::c_void,
    user: *mut std::ffi::c_void,
) {
    if user.is_null() {
        return;
    }
    let cap = &*(user as *const LiveMetaUser);
    // 64-bit BASS passes the metadata pointer in `data`; GetTags is often already empty.
    let from_data = crate::bass::ffi::BassLibrary::copy_tag_ptr(data, false);
    let meta = from_data
        .or_else(|| copy_tag_via(cap.get_tags, channel, crate::bass::BASS_TAG_META, false));
    let icy = copy_tag_via(cap.get_tags, channel, crate::bass::BASS_TAG_ICY, true);
    let http = copy_tag_via(cap.get_tags, channel, crate::bass::BASS_TAG_HTTP, true);
    crate::stream_debug::log(format!(
        "BASS_SYNC_META data={data:?} META={:?} ICY={:?} HTTP={:?}",
        meta.as_deref(),
        icy.as_deref(),
        http.as_deref()
    ));
    if let Ok(mut inbox) = cap.inbox.lock() {
        if meta.is_some() {
            inbox.meta = meta;
        }
        if icy.is_some() {
            inbox.icy = icy;
        }
        if http.is_some() {
            inbox.http = http;
        }
    }
}

unsafe fn copy_tag_via(
    get_tags: unsafe extern "system" fn(u32, u32) -> *const std::ffi::c_void,
    handle: u32,
    tags: u32,
    list: bool,
) -> Option<String> {
    crate::bass::ffi::BassLibrary::copy_tag_ptr(get_tags(handle, tags), list)
}

/// Pull a header value (case-insensitive key) out of an ICY header block.
/// BASS returns lines like `icy-name:Some Station` separated by NULs — the FFI
/// layer already joined them, so we scan line by line for `key:value`.
fn parse_comment_tag(raw: &str, key: &str) -> Option<String> {
    for line in raw.split(|c| c == '\n' || c == '\r' || c == '\0') {
        let line = line.trim();
        let Some(idx) = line.find('=') else {
            continue;
        };
        let (name, value) = line.split_at(idx);
        if name.trim().eq_ignore_ascii_case(key) {
            let value = value[1..].trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn parse_icy_header(raw: &str, key: &str) -> Option<String> {
    for line in raw.split(|c| c == '\n' || c == '\r' || c == '\0') {
        let line = line.trim();
        if let Some(idx) = line.find(':') {
            let (name, value) = line.split_at(idx);
            if name.trim().eq_ignore_ascii_case(key) {
                let value = value[1..].trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
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

    #[test]
    fn parses_icy_stream_title() {
        use crate::icy_tap::parse_icy_stream_title;
        assert_eq!(
            parse_icy_stream_title("StreamTitle='Artist - Song';StreamUrl='http://x';"),
            Some("Artist - Song".to_string())
        );
        assert_eq!(
            parse_icy_stream_title("StreamTitle='Only Title';"),
            Some("Only Title".to_string())
        );
        // Empty title between songs → None (don't clobber the last good title).
        assert_eq!(parse_icy_stream_title("StreamTitle='';StreamUrl='';"), None);
        assert_eq!(parse_icy_stream_title("NoTitleHere"), None);
    }

    #[test]
    fn parses_icy_station_header() {
        use super::parse_icy_header;
        let raw = "ICY 200 OK\r\nicy-name:Cool Radio\r\nicy-genre:Jazz\r\n";
        assert_eq!(
            parse_icy_header(raw, "icy-name"),
            Some("Cool Radio".to_string())
        );
        assert_eq!(parse_icy_header(raw, "icy-genre"), Some("Jazz".to_string()));
        assert_eq!(parse_icy_header(raw, "icy-br"), None);
    }
}
