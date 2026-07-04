// Player state management
//
// Wraps BASS in a higher-level API that tracks the current track, volume,
// playback state, and emits Tauri events for position updates.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use libloading::Library;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::bass::{self, BassLibrary};

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
    current_handle: u32,
    current_file: Option<String>,
    volume: f32,
    /// Loaded addon DLLs — kept alive so the plugins stay registered.
    _addons: Vec<Library>,
}

// ── Public player handle ──────────────────────────────────────────────────────
#[derive(Clone)]
pub struct Player {
    inner: Arc<Mutex<PlayerInner>>,
}

impl Player {
    /// Create a new Player. `bass_dir` is the folder containing bass.dll.
    pub fn new(bass_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlayerInner {
                bass: None,
                bass_dir,
                current_handle: 0,
                current_file: None,
                volume: 1.0,
                _addons: Vec::new(),
            })),
        }
    }

    /// Initialize the BASS audio system. Must be called before any playback.
    pub fn init(&self) -> Result<(), String> {
        let mut inner = self.inner.lock();
        if inner.bass.is_some() {
            return Ok(()); // already initialized
        }
        let bass = BassLibrary::load(&inner.bass_dir)?;
        bass.init(-1, 44100)?;
        inner.bass = Some(bass);
        Ok(())
    }

    /// Play a file. Stops the current stream first if any.
    pub fn play(&self, path: &str) -> Result<(), String> {
        let mut inner = self.inner.lock();

        // Stop previous stream before taking a long-lived borrow of `bass`
        let prev_handle = inner.current_handle;
        if prev_handle != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_stop(prev_handle);
            }
            inner.current_handle = 0;
        }

        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;

        let flags = bass::BASS_STREAM_PRESCAN
            | bass::BASS_STREAM_AUTOFREE
            | bass::BASS_SAMPLE_FLOAT;

        let handle = bass.stream_create_file(path, flags)?;

        // Apply current volume
        let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_VOL, inner.volume);

        bass.channel_play(handle, true)?;

        inner.current_handle = handle;
        inner.current_file = Some(path.to_string());
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        let inner = self.inner.lock();
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        if inner.current_handle != 0 {
            bass.channel_pause(inner.current_handle)
        } else {
            Err("Nothing is playing".into())
        }
    }

    pub fn resume(&self) -> Result<(), String> {
        let inner = self.inner.lock();
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        if inner.current_handle != 0 {
            bass.channel_play(inner.current_handle, false)
        } else {
            Err("Nothing is playing".into())
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut inner = self.inner.lock();
        let handle = inner.current_handle;
        if handle != 0 {
            if let Some(bass) = inner.bass.as_ref() {
                let _ = bass.channel_stop(handle);
            }
            inner.current_handle = 0;
            inner.current_file = None;
        }
        Ok(())
    }

    /// Seek to a position in seconds.
    pub fn seek(&self, position_secs: f64) -> Result<(), String> {
        let inner = self.inner.lock();
        let bass = inner.bass.as_ref().ok_or("BASS not initialized")?;
        if inner.current_handle == 0 {
            return Err("Nothing is playing".into());
        }
        // Convert seconds back to bytes — BASS doesn't have a seconds2bytes,
        // so we use channel info to calculate.
        let info = bass.channel_get_info(inner.current_handle)?;
        let bytes_per_sample = if info.flags & bass::BASS_SAMPLE_FLOAT != 0 { 4 } else { 2 };
        let block_align = bytes_per_sample * info.chans;
        let byte_pos = (position_secs * info.freq as f64) as u64 * block_align as u64;
        bass.channel_set_position(inner.current_handle, byte_pos, bass::BASS_POS_BYTE)
    }

    /// Set volume (0.0 — 1.0).
    pub fn set_volume(&self, vol: f32) -> Result<(), String> {
        let mut inner = self.inner.lock();
        inner.volume = vol.clamp(0.0, 1.0);
        if let Some(bass) = inner.bass.as_ref() {
            if inner.current_handle != 0 {
                bass.channel_set_attribute(inner.current_handle, bass::BASS_ATTRIB_VOL, inner.volume)?;
            }
        }
        Ok(())
    }

    /// Get a snapshot of the current player state.
    pub fn get_state(&self) -> PlayerStateSnapshot {
        let inner = self.inner.lock();
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
        let bass = inner.bass.as_ref().unwrap();
        let active: PlaybackState = bass.channel_is_active(inner.current_handle).into();
        let pos_bytes = bass.channel_get_position(inner.current_handle, bass::BASS_POS_BYTE);
        let len_bytes = bass.channel_get_length(inner.current_handle, bass::BASS_POS_BYTE);
        let position = bass.channel_bytes2seconds(inner.current_handle, pos_bytes);
        let duration = bass.channel_bytes2seconds(inner.current_handle, len_bytes);

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
        let mut inner = self.inner.lock();
        let addon_path = Path::new(path);
        // If relative, resolve against bass_dir
        let full_path = if addon_path.is_absolute() {
            addon_path.to_path_buf()
        } else {
            inner.bass_dir.join(addon_path)
        };
        let lib = bass::load_addon(&full_path)?;
        inner._addons.push(lib);
        Ok(())
    }

    /// Start a background thread that emits position updates via Tauri events.
    ///
    /// Fires `player:position` events ~10 times per second while playing.
    /// Also detects when a track ends and emits `player:track-ended`.
    pub fn start_position_emitter(&self, app: AppHandle) {
        let player = self.clone();
        thread::spawn(move || {
            let mut was_playing = false;
            loop {
                thread::sleep(Duration::from_millis(100));

                let snapshot = player.get_state();

                match snapshot.state {
                    PlaybackState::Playing => {
                        was_playing = true;
                        let payload = PositionPayload {
                            position: snapshot.position,
                            duration: snapshot.duration,
                            state: snapshot.state,
                        };
                        let _ = app.emit("player:position", &payload);
                    }
                    PlaybackState::Stopped if was_playing => {
                        // Track just ended — emit event for auto-advance
                        was_playing = false;
                        let _ = app.emit("player:track-ended", serde_json::json!({}));
                    }
                    _ => {
                        was_playing = snapshot.state == PlaybackState::Paused && was_playing;
                    }
                }
            }
        });
    }
}
