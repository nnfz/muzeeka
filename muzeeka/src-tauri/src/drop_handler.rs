// Native drag-and-drop handling in Rust.
//
// For the main window, Tauri routes drops as WindowEvent (not WebviewEvent).
// On Windows, drop paths can be corrupted — we keep enter paths as fallback.

use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tauri::{DragDropEvent, Emitter, Manager, Window, WindowEvent};

use crate::library;

const EXPORT_DROP_SUPPRESS_FOR: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct DropState {
    pub last_paths: Mutex<Vec<PathBuf>>,
}

#[derive(Clone, Default)]
pub struct ExportDragContext {
    pub track_paths: Vec<String>,
    pub source_playlist_id: Option<String>,
    pub is_copy: bool,
}

/// Tracks files dragged out of the app so re-entering the window is not treated as import.
#[derive(Default)]
pub struct ExportDragState {
    suppressed_keys: Mutex<HashSet<String>>,
    suppress_until: Mutex<Option<Instant>>,
    export_in_progress: Mutex<bool>,
    context: Mutex<Option<ExportDragContext>>,
}

pub fn normalize_path_key(path_str: &str) -> String {
    let mut key = path_str.trim().to_lowercase().replace('/', "\\");
    if let Some(rest) = key.strip_prefix(r"\\?\unc\") {
        if let Some((server, share)) = rest.split_once('\\') {
            key = format!(r"\\{server}\{share}");
        }
    } else if let Some(rest) = key.strip_prefix(r"\\?\") {
        key = rest.to_string();
    }
    key
}

pub fn path_match_key(path: &Path) -> String {
    normalize_path_key(&path.to_string_lossy())
}

fn register_path_keys(keys: &mut HashSet<String>, path: &Path) {
    keys.insert(path_match_key(path));
    keys.insert(normalize_path_key(&path.to_string_lossy()));
    if let Ok(canonical) = std::fs::canonicalize(path) {
        keys.insert(normalize_path_key(&canonical.to_string_lossy()));
    }
    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        keys.insert(file_name.to_lowercase());
    }
}

impl ExportDragState {
    pub fn register_export(&self, paths: &[PathBuf], context: Option<ExportDragContext>) {
        let mut keys = self.suppressed_keys.lock();
        keys.clear();
        for path in paths {
            register_path_keys(&mut keys, path);
        }
        *self.export_in_progress.lock() = true;
        *self.suppress_until.lock() = None;
        *self.context.lock() = context;
    }

    pub fn finish_export(&self) {
        *self.export_in_progress.lock() = false;
        *self.context.lock() = None;
        *self.suppress_until.lock() = Some(Instant::now() + EXPORT_DROP_SUPPRESS_FOR);
    }

    pub fn has_track_context(&self) -> bool {
        self.context.lock().is_some()
    }

    pub fn export_in_progress(&self) -> bool {
        *self.export_in_progress.lock()
    }

    fn should_suppress_import_ui(&self, paths: &[String]) -> bool {
        if self.export_in_progress() {
            return true;
        }
        self.all_paths_suppressed(paths)
    }

    fn purge_expired(&self) {
        let until = *self.suppress_until.lock();
        if let Some(deadline) = until {
            if Instant::now() >= deadline {
                self.suppressed_keys.lock().clear();
                *self.suppress_until.lock() = None;
            }
        }
    }

    fn is_suppressed_key(&self, key: &str) -> bool {
        self.purge_expired();
        let keys = self.suppressed_keys.lock();
        if !keys.contains(key) {
            return false;
        }
        match *self.suppress_until.lock() {
            None => true,
            Some(deadline) => Instant::now() < deadline,
        }
    }

    pub fn is_suppressed_path(&self, path: &str) -> bool {
        if self.export_in_progress() {
            return true;
        }
        let path = Path::new(path);
        if self.is_suppressed_key(&path_match_key(path)) {
            return true;
        }
        if self.is_suppressed_key(&normalize_path_key(&path.to_string_lossy())) {
            return true;
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| self.is_suppressed_key(&name.to_lowercase()))
    }

    pub fn filter_drop_paths(&self, paths: Vec<String>) -> Vec<String> {
        paths
            .into_iter()
            .filter(|path| !self.is_suppressed_path(path))
            .collect()
    }

    pub fn all_paths_suppressed(&self, paths: &[String]) -> bool {
        !paths.is_empty() && paths.iter().all(|path| self.is_suppressed_path(path))
    }
}

#[derive(Clone, Serialize)]
pub struct DroppedTracksPayload {
    pub files: Vec<library::MusicFile>,
    pub position: [f64; 2],
    pub message: Option<String>,
}

fn effective_paths(drop_paths: &[PathBuf], fallback: &[PathBuf]) -> Vec<String> {
    let drop_valid: Vec<String> = drop_paths
        .iter()
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .collect();

    if !drop_valid.is_empty() {
        return drop_valid;
    }

    fallback
        .iter()
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn emit_drop_result(window: &Window, position: [f64; 2], paths: Vec<String>) {
    let payload = match library::scan_paths(&paths) {
        Ok(files) if files.is_empty() => DroppedTracksPayload {
            files,
            position,
            message: Some("No supported audio files found".into()),
        },
        Ok(files) => DroppedTracksPayload {
            files,
            position,
            message: None,
        },
        Err(error) => DroppedTracksPayload {
            files: Vec::new(),
            position,
            message: Some(error),
        },
    };

    let _ = window.emit("muzeeka:dropped-tracks", &payload);
}

fn handle_drag_drop(window: &Window, state: &DropState, drag: &DragDropEvent) {
    let export_state = window.state::<ExportDragState>();

    match drag {
        DragDropEvent::Enter { paths, .. } => {
            *state.last_paths.lock() = paths.clone();
            let entered: Vec<String> = paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            let active = !export_state.should_suppress_import_ui(&entered);
            let _ = window.emit("muzeeka:drag-active", active);
        }
        DragDropEvent::Over { .. } => {
            let entered: Vec<String> = state
                .last_paths
                .lock()
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            if !export_state.should_suppress_import_ui(&entered) {
                let _ = window.emit("muzeeka:drag-active", true);
            }
        }
        DragDropEvent::Drop { paths, position } => {
            let fallback = state.last_paths.lock().clone();
            let resolved = effective_paths(paths, &fallback);
            state.last_paths.lock().clear();
            let _ = window.emit("muzeeka:drag-active", false);

            if resolved.is_empty() {
                let _ = window.emit(
                    "muzeeka:dropped-tracks",
                    &DroppedTracksPayload {
                        files: Vec::new(),
                        position: [position.x, position.y],
                        message: Some("Could not read dropped files or folders".into()),
                    },
                );
                return;
            }

            let import_paths = export_state.filter_drop_paths(resolved);
            if import_paths.is_empty() {
                return;
            }

            emit_drop_result(window, [position.x, position.y], import_paths);
        }
        DragDropEvent::Leave => {
            state.last_paths.lock().clear();
            let _ = window.emit("muzeeka:drag-active", false);
        }
        _ => {}
    }
}

pub fn handle_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::DragDrop(drag) = event else {
        return;
    };

    let state = window.state::<DropState>();
    handle_drag_drop(window, &state, drag);
}