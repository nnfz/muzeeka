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
use crate::cue;
use crate::equalizer::{eq_dsp_callback, EqDspContext, EqualizerSettings};

// Spotify-like short musical fades (not on track changes)
const PAUSE_FADE_MS: u32 = 220;
const RESUME_FADE_MS: u32 = 180;

// For seek: shallow dip (not to zero) + fast restore to feel like "наложение" / overlap
const SEEK_DIP_MS: u32 = 40;
const SEEK_RESTORE_MS: u32 = 60;
const SEEK_DIP_LEVEL: f32 = 0.22;

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
    current_handle: u32,
    dsp_handle: u32,
    current_file: Option<String>,
    volume: f32,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
    eq_context: &'static EqDspContext,
    /// Handles returned by BASS_PluginLoad — keep plugins registered.
    _plugin_handles: Vec<u32>,
}

/// BASS format plugins loaded via `BASS_PluginLoad` (not bass_fx / bassmix / etc.).
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
    "basszxtune.dll",
];

// ── Public player handle ──────────────────────────────────────────────────────
#[derive(Clone)]
pub struct Player {
    inner: Arc<Mutex<PlayerInner>>,
    app: Arc<RwLock<Option<AppHandle>>>,
    bass_thread: Arc<RwLock<Option<thread::ThreadId>>>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlayerInner {
                bass: None,
                bass_dir: PathBuf::new(),
                current_handle: 0,
                dsp_handle: 0,
                current_file: None,
                volume: 1.0,
                cue_start: None,
                cue_end: None,
                eq_context: Box::leak(Box::new(EqDspContext::new())),
                _plugin_handles: Vec::new(),
            })),
            app: Arc::new(RwLock::new(None)),
            bass_thread: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.write() = Some(app);
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

        let bass = BassLibrary::load(&inner.bass_dir)?;

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

        inner.eq_context.set_float_dsp_enabled(float_dsp_ok);
        inner.bass = Some(bass);
        Self::load_bass_addons(inner);
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
            if name.eq_ignore_ascii_case("bass.dll") {
                continue;
            }
            if !BASS_FORMAT_PLUGINS
                .iter()
                .any(|plugin| name.eq_ignore_ascii_case(plugin))
            {
                continue;
            }

            let path_str = path.to_string_lossy().to_string();
            match bass.plugin_load(&path_str) {
                Ok(handle) => {
                    eprintln!("BASS plugin loaded: {name}");
                    inner._plugin_handles.push(handle);
                }
                Err(error) => {
                    eprintln!("BASS plugin failed ({name}): {error}");
                }
            }
        }
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

        if inner.current_handle == 0 {
            return Ok(());
        }

        if enabled {
            let handle = inner.current_handle;
            if inner.dsp_handle == 0 {
                Self::attach_dsp(inner, handle)?;
            }
        } else if inner.dsp_handle != 0 {
            Self::detach_dsp(inner);
        }
        Ok(())
    }

    fn detach_dsp(inner: &mut PlayerInner) {
        if inner.dsp_handle == 0 || inner.current_handle == 0 {
            inner.dsp_handle = 0;
            inner.eq_context.set_dsp_float_forced(false);
            return;
        }
        if let Some(bass) = inner.bass.as_ref() {
            let _ = bass.channel_remove_dsp(inner.current_handle, inner.dsp_handle);
        }
        inner.dsp_handle = 0;
        inner.eq_context.set_dsp_float_forced(false);
    }

    fn attach_dsp(inner: &mut PlayerInner, handle: u32) -> Result<(), String> {
        Self::detach_dsp(inner);

        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        let info = bass.channel_get_info(handle)?;
        let sample_rate = if info.freq > 0 {
            info.freq
        } else {
            bass.channel_get_attribute(handle, bass::BASS_ATTRIB_FREQ)
                .unwrap_or(44100.0) as u32
        };
        let sample_rate = if sample_rate > 0 { sample_rate } else { 44100 };

        inner.eq_context.set_dsp_float_forced(true);
        inner
            .eq_context
            .configure_stream(sample_rate, info.chans, info.flags);

        let user = (inner.eq_context as *const EqDspContext) as *mut std::ffi::c_void;
        let dsp = match bass.channel_set_dsp_ex(
            handle,
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
                    handle,
                    eq_dsp_callback,
                    bass::BASS_DSP_PRIORITY_FIRST,
                    user,
                )?
            }
        };
        inner.dsp_handle = dsp;
        Ok(())
    }

    /// Play a file. Stops the current stream first if any.
    pub fn play(
        &self,
        track_path: &str,
        audio_path: Option<&str>,
        cue_start: Option<f64>,
        cue_end: Option<f64>,
    ) -> Result<(), String> {
        let track_path = track_path.to_string();
        let audio_path = audio_path.map(str::to_string);
        self.run_on_bass_thread(move |inner| {
            Self::play_inner(inner, &track_path, audio_path.as_deref(), cue_start, cue_end)
        })
    }

    fn play_inner(
        inner: &mut PlayerInner,
        track_path: &str,
        audio_path: Option<&str>,
        cue_start: Option<f64>,
        cue_end: Option<f64>,
    ) -> Result<(), String> {
        let prev_handle = inner.current_handle;
        if prev_handle != 0 {
            Self::detach_dsp(inner);
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_stop(prev_handle);
            }
            inner.current_handle = 0;
        }

        let playback = cue::resolve_playback(track_path, audio_path, cue_start, cue_end)
            .map_err(|error| format!("{error} (track: {track_path})"))?;
        let playback_path = playback.audio_path;
        let cue_start = playback.cue_start;
        let cue_end = playback.cue_end;

        eprintln!(
            "[muzeeka] play: track={track_path} audio={playback_path} cue={cue_start:?}-{cue_end:?}"
        );
        let flags = bass::BASS_STREAM_PRESCAN | bass::BASS_SAMPLE_FLOAT;

        let handle = {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            bass.stream_create_file(&playback_path, flags).map_err(|error| {
                format!("{error} — file: {playback_path}")
            })?
        };

        {
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            bass.channel_set_attribute(handle, bass::BASS_ATTRIB_VOL, inner.volume)?;
        }

        Self::wait_for_stream_length(inner, handle)?;
        Self::start_stream_playback(inner, handle, cue_start)?;

        // Attach EQ after the stream is at the CUE offset so APE seeking stays accurate.
        if inner.eq_context.get_settings().enabled {
            Self::attach_dsp(inner, handle)?;
        } else {
            inner.dsp_handle = 0;
        }

        inner.current_handle = handle;
        inner.current_file = Some(track_path.to_string());
        inner.cue_start = cue_start;
        inner.cue_end = cue_end;
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

    fn wait_for_stream_length(inner: &PlayerInner, handle: u32) -> Result<(), String> {
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        for _ in 0..100 {
            if bass.channel_get_length(handle, bass::BASS_POS_BYTE) > 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err("Audio stream is not ready (length unavailable)".to_string())
    }

    fn stream_position_secs(bass: &BassLibrary, handle: u32) -> f64 {
        let pos_bytes = bass.channel_get_position(handle, bass::BASS_POS_BYTE);
        bass.channel_bytes2seconds(handle, pos_bytes)
    }

    fn start_stream_playback(
        inner: &PlayerInner,
        handle: u32,
        cue_start: Option<f64>,
    ) -> Result<(), String> {
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        bass.channel_play(handle, false)?;

        let Some(start) = cue_start else {
            return Ok(());
        };

        Self::seek_channel_to_seconds(bass, handle, start)?;
        let actual = Self::stream_position_secs(bass, handle);
        if (actual - start).abs() > 2.0 {
            bass.channel_play(handle, false)?;
            Self::seek_channel_to_seconds(bass, handle, start)?;
            let retry = Self::stream_position_secs(bass, handle);
            if (retry - start).abs() > 2.0 {
                return Err(format!(
                    "CUE seek failed: wanted {start:.1}s, at {retry:.1}s"
                ));
            }
        }

        Ok(())
    }

    fn cue_segment_finished(inner: &PlayerInner, absolute_secs: f64) -> bool {
        inner
            .cue_end
            .is_some_and(|end| absolute_secs >= end - 0.05)
    }

    pub fn pause(&self) -> Result<(), String> {
        // Start the fade-out immediately (Spotify-style smooth tail)
        let fade_started = self.run_on_bass_thread(|inner| {
            if inner.current_handle != 0 {
                if let Some(bass) = inner.bass.as_ref() {
                    let _ = bass.channel_slide_attribute(
                        inner.current_handle,
                        bass::BASS_ATTRIB_VOL,
                        0.0,
                        PAUSE_FADE_MS,
                    );
                }
                Ok(true)
            } else {
                Ok(false)
            }
        })?;

        if fade_started {
            // Schedule the actual pause after the fade has played out.
            // This lets the full musical fade be heard (like Spotify).
            let this = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(PAUSE_FADE_MS as u64 + 10));
                let _ = this.run_on_bass_thread(|inner| {
                    if inner.current_handle != 0 {
                        if let Some(bass) = inner.bass.as_ref() {
                            let _ = bass.channel_pause(inner.current_handle);
                        }
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
            if inner.current_handle != 0 {
                let target = inner.volume;
                // Force start from silence, play, then musical fade-in (Spotify style)
                let _ = bass.channel_set_attribute(
                    inner.current_handle,
                    bass::BASS_ATTRIB_VOL,
                    0.0,
                );
                bass.channel_play(inner.current_handle, false)?;
                bass.channel_slide_attribute(
                    inner.current_handle,
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
            let handle = inner.current_handle;
            if handle != 0 {
                Self::detach_dsp(inner);
                if let Some(bass) = inner.bass.as_ref() {
                    let _ = bass.channel_stop(handle);
                }
                inner.current_handle = 0;
                inner.current_file = None;
                inner.cue_start = None;
                inner.cue_end = None;
            }
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
            if inner.current_handle == 0 {
                return Err("Nothing is playing".into());
            }
            let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
            let handle = inner.current_handle;
            let target = inner.volume;

            // Shallow quick dip — not full fade out. This gives "наложение" perception
            // (brief lower volume + position change + restore) instead of clear silence.
            let _ = bass.channel_slide_attribute(
                handle,
                bass::BASS_ATTRIB_VOL,
                SEEK_DIP_LEVEL,
                SEEK_DIP_MS,
            );

            // Seek immediately while volume is dipped
            let absolute_secs = Self::absolute_seek_position(inner, position_secs);
            Self::seek_channel_to_seconds(bass, handle, absolute_secs)?;

            // Quick restore — the short dip + fast ramp back feels like overlap/blend
            bass.channel_slide_attribute(handle, bass::BASS_ATTRIB_VOL, target, SEEK_RESTORE_MS)
        })
    }

    /// Set volume (0.0 — 1.0).
    pub fn set_volume(&self, vol: f32) -> Result<(), String> {
        self.run_on_bass_thread(move |inner| {
            inner.volume = vol.clamp(0.0, 1.0);
            if let Some(bass) = inner.bass.as_ref() {
                if inner.current_handle != 0 {
                    bass.channel_set_attribute(
                        inner.current_handle,
                        bass::BASS_ATTRIB_VOL,
                        inner.volume,
                    )?;
                }
            }
            Ok(())
        })
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
        if inner.current_handle == 0 {
            return;
        }
        Self::detach_dsp(inner);
        if let Some(bass) = inner.bass.as_ref() {
            let _ = bass.channel_stop(inner.current_handle);
        }
        inner.current_handle = 0;
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
        if inner.current_handle == 0 || inner.bass.is_none() {
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
        let active_raw = {
            let bass = inner.bass.as_ref().unwrap();
            bass.channel_is_active(inner.current_handle)
        };
        let active: PlaybackState = active_raw.into();

        if active == PlaybackState::Stopped {
            let handle = inner.current_handle;
            Self::detach_dsp(inner);
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_stop(handle);
            }
            inner.current_handle = 0;
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
        let pos_bytes = bass.channel_get_position(inner.current_handle, bass::BASS_POS_BYTE);
        let len_bytes = bass.channel_get_length(inner.current_handle, bass::BASS_POS_BYTE);
        let absolute_position = bass.channel_bytes2seconds(inner.current_handle, pos_bytes);
        let absolute_duration = bass.channel_bytes2seconds(inner.current_handle, len_bytes);
        let position = Self::cue_relative_position(inner, absolute_position);
        let duration = Self::cue_segment_duration(inner, absolute_duration);

        let file_name = inner.current_file.as_ref().map(|f| {
            std::path::Path::new(f)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        });

        PlayerStateSnapshot {
            state: active,
            is_playing: active == PlaybackState::Playing,
            is_paused: active == PlaybackState::Paused,
            volume: inner.volume,
            position,
            duration,
            current_file: inner.current_file.clone(),
            current_file_name: file_name,
        }
    }

    /// Load a BASS addon DLL by path.
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
        Self::schedule_position_poll(app, player, was_playing);
    }

    fn schedule_position_poll(
        app: AppHandle,
        player: Player,
        was_playing: Arc<StdMutex<bool>>,
    ) {
        let app_for_sleep = app.clone();
        let player_for_main = player.clone();
        let was_for_main = was_playing.clone();
        let app_for_next = app.clone();
        let player_for_next = player;
        let was_for_next = was_playing;

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            let app_emit = app_for_sleep.clone();
            let _ = app_for_sleep.run_on_main_thread(move || {
                let mut snapshot = player_for_main.get_state();
                let cue_finished = player_for_main.run_on_bass_thread(|inner| {
                    if inner.current_handle == 0 || inner.bass.is_none() {
                        return Ok(false);
                    }
                    let bass = inner.bass.as_ref().unwrap();
                    let pos_bytes =
                        bass.channel_get_position(inner.current_handle, bass::BASS_POS_BYTE);
                    let absolute =
                        bass.channel_bytes2seconds(inner.current_handle, pos_bytes);
                    Ok(Self::cue_segment_finished(inner, absolute))
                }).unwrap_or(false);

                if cue_finished && snapshot.state == PlaybackState::Playing {
                    let _ = player_for_main.stop();
                    snapshot.state = PlaybackState::Stopped;
                    snapshot.is_playing = false;
                    snapshot.is_paused = false;
                    snapshot.position = snapshot.duration;
                }

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
                    }
                    PlaybackState::Stopped if *was => {
                        player_for_main.release_ended_stream();
                        *was = false;
                        let _ = app_emit.emit("player:track-ended", serde_json::json!({}));
                    }
                    _ => {
                        if snapshot.state != PlaybackState::Paused {
                            *was = false;
                        }
                    }
                }

                Self::schedule_position_poll(app_for_next, player_for_next, was_for_next);
            });
        });
    }
}
