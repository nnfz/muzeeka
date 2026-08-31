//! Test native plugin. Enable it in Settings → Plugins and watch the console:
//! `[plugin muzeeka.native-probe] ...`

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

    let _ = host.call("log.info", r#"{"message":"native probe started"}"#);

    let handle = thread::Builder::new()
        .name("native-probe".into())
        .spawn(move || worker(host))
        .expect("spawn native-probe worker");

    *WORKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    0
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(handle) = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = handle.join();
    }
}

fn worker(host: MuzeekaHost) {
    loop {
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        let interval = interval_ms(&host);
        announce(&host);
        sleep_interruptible(Duration::from_millis(interval));
    }
    let _ = host.call("log.info", r#"{"message":"native probe stopped"}"#);
}

fn interval_ms(host: &MuzeekaHost) -> u64 {
    host.call("settings.get", "{}")
        .ok()
        .and_then(|v| v.get("interval_ms").and_then(|n| n.as_u64()))
        .unwrap_or(3000)
        .clamp(500, 60_000)
}

fn announce(host: &MuzeekaHost) {
    let msg = match host.call("player.state", "{}") {
        Ok(state) => {
            let playing = state.get("isPlaying").and_then(|v| v.as_bool()).unwrap_or(false);
            let title = state
                .get("track")
                .and_then(|t| t.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("—");
            let artist = state
                .get("track")
                .and_then(|t| t.get("artist"))
                .and_then(|v| v.as_str())
                .unwrap_or("—");
            if playing {
                format!("playing: {artist} — {title}")
            } else {
                format!("idle: {artist} — {title}")
            }
        }
        Err(err) => format!("player.state failed: {err}"),
    };
    let payload = serde_json::json!({ "message": msg }).to_string();
    let _ = host.call("log.info", &payload);
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
