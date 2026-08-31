use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

const MAX_LINES: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub ts: u64,
    pub level: String,
    pub source: String,
    pub message: String,
}

static LINES: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn push(app: &AppHandle, level: &str, source: &str, message: &str) {
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
    let _ = app.emit("dev:log", &line);
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
