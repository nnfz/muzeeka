// In-app developer log. Replaces the Tauri build's `app.emit("dev:log", …)` with
// a plain listener list so the log can be rendered by the GPUI view directly.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const MAX_LINES: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub ts: u64,
    pub level: String,
    pub source: String,
    pub message: String,
}

static LINES: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
static LISTENERS: Mutex<Vec<fn(&LogLine)>> = Mutex::new(Vec::new());

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Register a callback invoked for every new log line. `fn` pointers only, so it
/// can live in a `static` without a leak.
pub fn subscribe(listener: fn(&LogLine)) {
    LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).push(listener);
}

pub fn push(level: &str, source: &str, message: &str) {
    let line = LogLine {
        ts: now_ms(),
        level: level.to_string(),
        source: source.to_string(),
        message: message.to_string(),
    };
    if level == "error" {
        eprintln!("[{source} ERROR] {message}");
    } else {
        eprintln!("[{source}] {message}");
    }
    {
        let mut g = LINES.lock().unwrap_or_else(|e| e.into_inner());
        g.push_back(line.clone());
        while g.len() > MAX_LINES {
            g.pop_front();
        }
    }
    let listeners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
    for listener in listeners.iter() {
        listener(&line);
    }
}

pub fn info(source: &str, message: &str) {
    push("info", source, message);
}

pub fn error(source: &str, message: &str) {
    push("error", source, message);
}

pub fn lines() -> Vec<LogLine> {
    LINES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
