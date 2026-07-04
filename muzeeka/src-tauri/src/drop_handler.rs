// Native drag-and-drop handling in Rust.
//
// For the main window, Tauri routes drops as WindowEvent (not WebviewEvent).
// On Windows, drop paths can be corrupted — we keep enter paths as fallback.

use parking_lot::Mutex;
use serde::Serialize;
use std::path::PathBuf;

use tauri::{DragDropEvent, Emitter, Manager, Window, WindowEvent};

use crate::library;

#[derive(Default)]
pub struct DropState {
    pub last_paths: Mutex<Vec<PathBuf>>,
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
    match drag {
        DragDropEvent::Enter { paths, .. } => {
            *state.last_paths.lock() = paths.clone();
            let _ = window.emit("muzeeka:drag-active", true);
        }
        DragDropEvent::Over { .. } => {
            let _ = window.emit("muzeeka:drag-active", true);
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

            emit_drop_result(window, [position.x, position.y], resolved);
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