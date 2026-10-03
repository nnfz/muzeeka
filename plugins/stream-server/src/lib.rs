#[path = "../../sdk/muzeeka_plugin.rs"]
mod muzeeka_plugin;

mod capture;
mod encode;
mod hub;
mod meta;
mod server;

use std::net::TcpListener;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use encode::{snap, Format, Pipeline, BITRATES, SAMPLE_RATES};
use hub::Hub;
use meta::{NowPlaying, Track};
use muzeeka_plugin::{MuzeekaHost, MUZEEKA_PLUGIN_ABI};

pub static STOP: AtomicBool = AtomicBool::new(false);
static WORKERS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

pub struct Config {
    pub port: u16,
    pub token: String,
    pub format: Format,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub gain: f32,
    pub max_listeners: usize,
}

pub struct Shared {
    pub hub: Arc<Hub>,
    pub now: Arc<NowPlaying>,
    pub live: AtomicBool,
    pub capture_error: Mutex<Option<String>>,
}

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

    let cfg = Arc::new(read_config(&host));

    let listener = match TcpListener::bind(("0.0.0.0", cfg.port)) {
        Ok(l) => l,
        Err(err) => {
            log(&host, "error", &format!("cannot bind port {}: {err}", cfg.port));
            return 1;
        }
    };
    if listener.set_nonblocking(true).is_err() {
        log(&host, "error", "cannot switch listener to non-blocking mode");
        return 1;
    }

    let shared = Arc::new(Shared {
        hub: Arc::new(Hub::new()),
        now: Arc::new(NowPlaying::new()),
        live: AtomicBool::new(false),
        capture_error: Mutex::new(None),
    });

    let mut handles = Vec::new();

    {
        let cfg = cfg.clone();
        let shared = shared.clone();
        match thread::Builder::new()
            .name("stream-server".into())
            .spawn(move || server::serve(listener, cfg, shared))
        {
            Ok(h) => handles.push(h),
            Err(err) => {
                log(&host, "error", &format!("could not spawn server thread: {err}"));
                return 1;
            }
        }
    }

    {
        let cfg = cfg.clone();
        let shared = shared.clone();
        match thread::Builder::new()
            .name("stream-capture".into())
            .spawn(move || capture_loop(host, cfg, shared))
        {
            Ok(h) => handles.push(h),
            Err(err) => {
                STOP.store(true, Ordering::SeqCst);
                join_all(handles);
                log(&host, "error", &format!("could not spawn capture thread: {err}"));
                return 1;
            }
        }
    }

    {
        let cfg = cfg.clone();
        let shared = shared.clone();
        match thread::Builder::new()
            .name("stream-manager".into())
            .spawn(move || manager(host, cfg, shared))
        {
            Ok(h) => handles.push(h),
            Err(err) => {
                STOP.store(true, Ordering::SeqCst);
                join_all(handles);
                log(&host, "error", &format!("could not spawn manager thread: {err}"));
                return 1;
            }
        }
    }

    *WORKERS.lock().unwrap_or_else(|e| e.into_inner()) = handles;
    0
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    STOP.store(true, Ordering::SeqCst);
    let handles = std::mem::take(&mut *WORKERS.lock().unwrap_or_else(|e| e.into_inner()));
    join_all(handles);
}

fn join_all(handles: Vec<JoinHandle<()>>) {
    for h in handles {
        let _ = h.join();
    }
}

fn read_config(host: &MuzeekaHost) -> Config {
    let v = host.call("settings.get", "{}").unwrap_or(serde_json::Value::Null);
    let num = |key: &str, fallback: f64| v.get(key).and_then(|x| x.as_f64()).unwrap_or(fallback);
    let text = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    Config {
        port: num("port", 8800.0).clamp(1024.0, 65535.0) as u16,
        token: text("token"),
        format: Format::parse(&text("format")),
        bitrate: snap(num("bitrate_kbps", 128.0).clamp(32.0, 256.0) as u32, &BITRATES),
        sample_rate: snap(num("sample_rate", 48000.0).clamp(8000.0, 48000.0) as u32, &SAMPLE_RATES),
        gain: (num("gain_percent", 100.0).clamp(0.0, 200.0) / 100.0) as f32,
        max_listeners: num("max_listeners", 10.0).clamp(1.0, 100.0) as usize,
    }
}

fn poll_track(host: &MuzeekaHost) -> Result<Track, String> {
    let state = host.call("player.state", "{}")?;
    let Some(track) = state.get("track").filter(|v| !v.is_null()) else {
        return Ok(Track::default());
    };
    let text = |key: &str| {
        track
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Ok(Track {
        artist: text("artist"),
        title: text("title"),
        album: text("album"),
    })
}

fn capture_loop(host: MuzeekaHost, cfg: Arc<Config>, shared: Arc<Shared>) {
    while !STOP.load(Ordering::SeqCst) {
        let mut pipeline: Option<Pipeline> = None;
        let mut failure: Option<String> = None;

        let result = capture::run(&host, &mut |rate, frames| {
            shared.live.store(true, Ordering::SeqCst);
            if pipeline.is_none() {
                if let Ok(track) = poll_track(&host) {
                    shared.now.update(track);
                }
                match Pipeline::new(
                    rate,
                    cfg.sample_rate,
                    cfg.bitrate,
                    cfg.gain,
                    shared.hub.clone(),
                    cfg.format,
                    shared.now.clone(),
                ) {
                    Ok(p) => pipeline = Some(p),
                    Err(err) => {
                        failure = Some(err);
                        return;
                    }
                }
            }
            if let Some(p) = pipeline.as_mut() {
                p.set_src_rate(rate);
                p.push(frames);
            }
        });

        shared.live.store(false, Ordering::SeqCst);
        if let Some(p) = pipeline.as_mut() {
            p.finish();
        }
        let err = result.err().or(failure);
        *shared.capture_error.lock().unwrap_or_else(|e| e.into_inner()) = err;
        sleep_interruptible(Duration::from_millis(2000));
    }
}

fn manager(host: MuzeekaHost, cfg: Arc<Config>, shared: Arc<Shared>) {
    let mut last_capture_error: Option<String> = None;
    let mut was_live = false;
    let mut meta_error: Option<String> = None;

    log(
        &host,
        "info",
        &format!(
            "stream-server 0.4.4: {}, level ignores the player volume",
            cfg.format.label()
        ),
    );
    let token = if cfg.token.is_empty() {
        String::new()
    } else {
        "?token=...".to_string()
    };
    log(
        &host,
        "info",
        &format!(
            "browser: http://localhost:{}/{token}",
            cfg.port
        ),
    );
    log(
        &host,
        "info",
        &format!(
            "players: http://localhost:{}/stream.{}{token}",
            cfg.port,
            cfg.format.as_str()
        ),
    );
    if cfg.token.is_empty() {
        log(&host, "info", "no token set: anyone who finds the port can listen");
    }

    while !STOP.load(Ordering::SeqCst) {
        let current = shared
            .capture_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if current != last_capture_error {
            if let Some(err) = &current {
                log(&host, "error", &format!("capture: {err}"));
            }
            last_capture_error = current;
        }
        match poll_track(&host) {
            Ok(track) => {
                shared.now.update(track);
                meta_error = None;
            }
            Err(err) => {
                if meta_error.as_deref() != Some(err.as_str()) {
                    log(&host, "error", &format!("track title: {err}"));
                    meta_error = Some(err);
                }
            }
        }
        let live = shared.live.load(Ordering::SeqCst);
        if live != was_live {
            log(&host, "info", if live { "capture is live" } else { "capture stopped" });
            was_live = live;
        }
        sleep_interruptible(Duration::from_millis(2000));
    }

    log(&host, "info", "stream server stopped");
}

pub fn log(host: &MuzeekaHost, level: &str, msg: &str) {
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
