use crate::dev_log::{self, LogLine};

#[tauri::command]
pub fn dev_log_lines() -> Vec<LogLine> {
    dev_log::lines()
}

#[tauri::command]
pub fn dev_log_clear() {
    dev_log::clear();
}
