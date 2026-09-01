//! Micspam — duplicates the player output into a virtual audio cable.
//!
//! What it does: finds an output device by name (`device_match`), attaches a parallel
//! player output to it and keeps that output alive. In Discord or a game you then pick the
//! cable's paired input (`CABLE Output`) as your microphone.
//!
//! The cable driver is kernel-mode and cannot be created from a plugin: VB-CABLE is
//! installed once by hand. If the device is missing, the plugin just logs that and waits.

#[path = "../../sdk/muzeeka_plugin.rs"]
mod muzeeka_plugin;

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use muzeeka_plugin::{MuzeekaHost, MUZEEKA_PLUGIN_ABI};

static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

#[no_mangle]
pub extern "C" fn muzeeka_plugin_abi() -> u32 {
    MUZEEKA_PLUGIN_ABI
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_start(host: *const MuzeekaHost) -> c_int {
    if host.is_null() {
        return 1;
    }
    let host = unsafe { *host };
    STOP.store(false, Ordering::SeqCst);

    let handle = match thread::Builder::new()
        .name("micspam".into())
        .spawn(move || worker(host))
    {
        Ok(h) => h,
        Err(err) => {
            log(&host, "error", &format!("could not spawn thread: {err}"));
            return 1;
        }
    };
    *WORKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    0
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    STOP.store(true, Ordering::SeqCst);
    // The worker removes the output itself before exiting, so joining before we return is
    // mandatory: once stop() returns, the host pointer is no longer valid.
    if let Some(handle) = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = handle.join();
    }
}

/// Worker state carried between poll iterations.
struct State {
    /// Id of the active extra output (`out-N`) while it is attached.
    output_id: Option<String>,
    /// Last error message, so the log is not flooded every couple of seconds.
    last_error: Option<String>,
    /// Volume the host has already accepted. Keeps us from calling setOutputVolume on
    /// every poll iteration.
    applied_volume: Option<f32>,
}

fn worker(host: MuzeekaHost) {
    let mut state = State {
        output_id: None,
        last_error: None,
        applied_volume: None,
    };

    log(&host, "info", "micspam started");

    while !STOP.load(Ordering::SeqCst) {
        let cfg = settings(&host);
        tick(&host, &mut state, &cfg);
        sleep_interruptible(Duration::from_millis(cfg.poll_ms));
    }

    detach(&host, &mut state);
    log(&host, "info", "micspam stopped");
}

struct Config {
    device_match: String,
    poll_ms: u64,
    /// Cable-only volume, 0.0–1.0. Nothing changes in the headphones.
    volume: f32,
}

fn settings(host: &MuzeekaHost) -> Config {
    let v = host.call("settings.get", "{}").unwrap_or(serde_json::Value::Null);
    let s = |key: &str, fallback: &str| -> String {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .unwrap_or(fallback)
            .to_string()
    };
    Config {
        device_match: s("device_match", "CABLE Input"),
        poll_ms: v
            .get("poll_ms")
            .and_then(|x| x.as_u64())
            .unwrap_or(2000)
            .clamp(500, 10_000),
        // The setting is in whole percent: a nicer slider in the UI than 0.0–1.0.
        volume: (v
            .get("volume_percent")
            .and_then(|x| x.as_f64())
            .unwrap_or(70.0)
            .clamp(0.0, 100.0)
            / 100.0) as f32,
    }
}

/// One iteration: make sure the cable is there and our output is attached to it.
fn tick(host: &MuzeekaHost, state: &mut State, cfg: &Config) {
    // Output already attached — check the host still knows about it. A BASS restart
    // (device switch, Ctrl+R) can drop it; if so, attach again.
    if let Some(id) = state.output_id.clone() {
        match host.call("audio.outputs", "{}") {
            Ok(list) => {
                let alive = list
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .any(|o| o.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
                    })
                    .unwrap_or(false);
                if alive {
                    apply_volume(host, state, &id, cfg.volume);
                    return;
                }
                state.output_id = None;
                state.applied_volume = None;
                log(host, "info", "output was dropped, reattaching");
            }
            Err(err) => {
                report(host, state, &format!("audio.outputs: {err}"));
                return;
            }
        }
    }

    let devices = match host.call("audio.devices", "{}") {
        Ok(d) => d,
        Err(err) => {
            report(host, state, &format!("audio.devices: {err}"));
            return;
        }
    };

    let Some(device) = find_device(&devices, &cfg.device_match) else {
        report(
            host,
            state,
            &format!(
                "no device named '{}' found. Install VB-CABLE and pick its input in the plugin settings.",
                cfg.device_match
            ),
        );
        return;
    };

    // Pass the volume along with the attach: the host applies it before channel_play so
    // the cable does not blast at full volume for the first few milliseconds.
    let payload =
        serde_json::json!({ "deviceId": device.0, "volume": cfg.volume }).to_string();
    match host.call("audio.addOutput", &payload) {
        Ok(info) => {
            let id = info
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                report(host, state, "addOutput returned an empty id");
                return;
            }
            state.output_id = Some(id);
            state.applied_volume = Some(cfg.volume);
            state.last_error = None;
            log(
                host,
                "info",
                &format!(
                    "music is going to '{}' at {}% — set its paired input as your microphone",
                    device.1,
                    (cfg.volume * 100.0).round()
                ),
            );
        }
        Err(err) => report(host, state, &format!("addOutput '{}': {err}", device.1)),
    }
}

/// Moves the cable volume when the slider changed. The main output is untouched — it has
/// its own volume on the mixer.
fn apply_volume(host: &MuzeekaHost, state: &mut State, output_id: &str, volume: f32) {
    // The setting arrives from the UI in whole percent, so comparing for equality is safe
    // here: there is no float jitter to worry about.
    if state.applied_volume == Some(volume) {
        return;
    }
    let payload = serde_json::json!({ "id": output_id, "volume": volume }).to_string();
    match host.call("audio.setOutputVolume", &payload) {
        Ok(_) => {
            state.applied_volume = Some(volume);
            state.last_error = None;
            log(
                host,
                "info",
                &format!("micspam volume: {}%", (volume * 100.0).round()),
            );
        }
        Err(err) => report(host, state, &format!("setOutputVolume: {err}")),
    }
}

fn detach(host: &MuzeekaHost, state: &mut State) {
    state.applied_volume = None;
    if let Some(id) = state.output_id.take() {
        let payload = serde_json::json!({ "id": id }).to_string();
        if let Err(err) = host.call("audio.removeOutput", &payload) {
            log(host, "error", &format!("removeOutput: {err}"));
        }
    }
}

/// Finds an enabled device whose name contains `needle` (case-insensitive).
/// Returns `(deviceId, name)`.
fn find_device(devices: &serde_json::Value, needle: &str) -> Option<(i64, String)> {
    let needle = needle.to_lowercase();
    devices.as_array()?.iter().find_map(|d| {
        let name = d.get("name").and_then(|x| x.as_str())?;
        if !name.to_lowercase().contains(&needle) {
            return None;
        }
        if !d.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false) {
            return None;
        }
        let id = d.get("id").and_then(|x| x.as_i64())?;
        if id <= 0 {
            return None;
        }
        Some((id, name.to_string()))
    })
}

// ── Logging ─────────────────────────────────────────────────────────────────

/// Logs an error once until its text changes: we poll every couple of seconds.
fn report(host: &MuzeekaHost, state: &mut State, msg: &str) {
    if state.last_error.as_deref() == Some(msg) {
        return;
    }
    state.last_error = Some(msg.to_string());
    log(host, "error", msg);
}

fn log(host: &MuzeekaHost, level: &str, msg: &str) {
    let payload = serde_json::json!({ "message": msg }).to_string();
    let _ = host.call(&format!("log.{level}"), &payload);
}

fn sleep_interruptible(total: Duration) {
    let start = Instant::now();
    while start.elapsed() < total {
        if STOP.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}
