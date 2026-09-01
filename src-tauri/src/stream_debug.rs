//! Radio-stream diagnostics. Every line goes to stderr, a temp file, and the
//! in-app developer log so we can see why a URL that plays in a browser is silent.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::AppHandle;

use crate::dev_log;

static FILE: OnceLock<Mutex<File>> = OnceLock::new();
static PATH: OnceLock<PathBuf> = OnceLock::new();
static APP: Mutex<Option<AppHandle>> = Mutex::new(None);

pub fn log_path() -> PathBuf {
    PATH.get_or_init(|| std::env::temp_dir().join("muzeeka-stream.log"))
        .clone()
}

fn file() -> &'static Mutex<File> {
    FILE.get_or_init(|| {
        let path = log_path();
        let opened = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .or_else(|_| File::create(&path));
        Mutex::new(opened.expect("muzeeka-stream.log"))
    })
}

pub fn set_app(app: AppHandle) {
    *APP.lock().unwrap_or_else(|e| e.into_inner()) = Some(app);
}

pub fn log(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    eprintln!("[stream] {msg}");
    if let Ok(mut f) = file().lock() {
        let _ = writeln!(&mut *f, "{ts} {msg}");
        let _ = f.flush();
    }
    let app = APP.lock().ok().and_then(|g| g.clone());
    if let Some(app) = app {
        dev_log::push(&app, "info", "stream", msg);
    }
}
