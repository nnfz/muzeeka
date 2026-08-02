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
use crate::playlists::LibraryDatabase;

const EXPORT_DROP_SUPPRESS_FOR: Duration = Duration::from_secs(8);

/// Ctrl state during OS file drag — WebView often does not receive key events while
/// Explorer owns the drag, so we read the real keyboard state here.
pub fn is_ctrl_held() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetAsyncKeyState(vKey: i32) -> i16;
        }
        // VK_CONTROL covers left/right in most cases; also check L/R explicitly.
        const VK_CONTROL: i32 = 0x11;
        const VK_LCONTROL: i32 = 0xA2;
        const VK_RCONTROL: i32 = 0xA3;
        unsafe {
            let down = |vk: i32| (GetAsyncKeyState(vk) as u16 & 0x8000) != 0;
            down(VK_CONTROL) || down(VK_LCONTROL) || down(VK_RCONTROL)
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[derive(Default)]
pub struct DropState {
    pub last_paths: Mutex<Vec<PathBuf>>,
}

#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct ExportDragContext {
    pub track_paths: Vec<String>,
    pub source_playlist_id: Option<String>,
    pub is_copy: bool,
}

/// All export-suppress state under one lock — updated/read atomically together.
#[derive(Default)]
struct ExportDragInner {
    suppressed_keys: HashSet<String>,
    suppress_until: Option<Instant>,
    export_in_progress: bool,
    context: Option<ExportDragContext>,
}

/// Tracks files dragged out of the app so re-entering the window is not treated as import.
#[derive(Default)]
pub struct ExportDragState {
    inner: Mutex<ExportDragInner>,
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

impl ExportDragInner {
    fn purge_expired(&mut self) {
        if let Some(deadline) = self.suppress_until {
            if Instant::now() >= deadline {
                self.suppressed_keys.clear();
                self.suppress_until = None;
            }
        }
    }

    fn is_suppressed_key(&mut self, key: &str) -> bool {
        self.purge_expired();
        if !self.suppressed_keys.contains(key) {
            return false;
        }
        match self.suppress_until {
            None => true, // export still in progress (until set only after finish)
            Some(deadline) => Instant::now() < deadline,
        }
    }

    fn is_suppressed_path(&mut self, path: &str) -> bool {
        if self.export_in_progress {
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
}

impl ExportDragState {
    pub fn register_export(&self, paths: &[PathBuf], context: Option<ExportDragContext>) {
        let mut g = self.inner.lock();
        g.suppressed_keys.clear();
        for path in paths {
            register_path_keys(&mut g.suppressed_keys, path);
        }
        g.export_in_progress = true;
        g.suppress_until = None;
        g.context = context;
    }

    pub fn finish_export(&self) {
        let mut g = self.inner.lock();
        g.export_in_progress = false;
        g.context = None;
        g.suppress_until = Some(Instant::now() + EXPORT_DROP_SUPPRESS_FOR);
    }

    #[allow(dead_code)] // reserved for playlist-aware drop targets
    pub fn has_track_context(&self) -> bool {
        self.inner.lock().context.is_some()
    }

    fn should_suppress_import_ui(&self, paths: &[String]) -> bool {
        let mut g = self.inner.lock();
        if g.export_in_progress {
            return true;
        }
        !paths.is_empty() && paths.iter().all(|path| g.is_suppressed_path(path))
    }

    pub fn filter_drop_paths(&self, paths: Vec<String>) -> Vec<String> {
        let mut g = self.inner.lock();
        paths
            .into_iter()
            .filter(|path| !g.is_suppressed_path(path))
            .collect()
    }
}

#[derive(Clone, Serialize)]
pub struct DroppedTracksPayload {
    pub files: Vec<library::MusicFile>,
    pub position: [f64; 2],
    pub message: Option<String>,
    /// Original drop paths (files and/or folders), before scanning.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Whether Ctrl was held at drop time (import-into-playlist mode).
    #[serde(default)]
    pub ctrl: bool,
}

/// Hover state while dragging files over the window (physical pixel coords).
#[derive(Clone, Serialize)]
pub struct DragActivePayload {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    /// Whether Ctrl is currently held (for import-into-playlist highlight).
    #[serde(default)]
    pub ctrl: bool,
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

fn emit_drop_result(window: &Window, position: [f64; 2], paths: Vec<String>, ctrl: bool) {
    if let Some(db) = window.try_state::<LibraryDatabase>() {
        for path in &paths {
            let p = Path::new(path);
            if p.is_dir() {
                let _ = db.ensure_root(path);
            } else if let Some(parent) = p.parent() {
                let _ = db.ensure_root(&parent.to_string_lossy());
            }
        }
    }

    let source_paths = paths.clone();
    let progress = {
        let window = window.clone();
        library::make_throttled_scan_progress(move |current, total, label| {
            let _ = window.emit(
                "library:scan-progress",
                library::LibraryScanProgress {
                    current,
                    total,
                    label: label.to_string(),
                },
            );
        })
    };
    let payload = match library::scan_paths_with_progress(&paths, &progress) {
        Ok(files) if files.is_empty() => DroppedTracksPayload {
            files,
            position,
            message: Some("No supported audio files found".into()),
            paths: source_paths,
            ctrl,
        },
        Ok(files) => DroppedTracksPayload {
            files,
            position,
            message: None,
            paths: source_paths,
            ctrl,
        },
        Err(error) => DroppedTracksPayload {
            files: Vec::new(),
            position,
            message: Some(error),
            paths: source_paths,
            ctrl,
        },
    };

    let _ = window.emit("muzeeka:dropped-tracks", &payload);
    let _ = window.emit("library:scan-finished", ());
}

fn emit_drag_active(window: &Window, active: bool, position: Option<[f64; 2]>) {
    let _ = window.emit(
        "muzeeka:drag-active",
        &DragActivePayload {
            active,
            position,
            ctrl: active && is_ctrl_held(),
        },
    );
}

fn handle_drag_drop(window: &Window, state: &DropState, drag: &DragDropEvent) {
    let export_state = window.state::<ExportDragState>();

    match drag {
        DragDropEvent::Enter { paths, position } => {
            *state.last_paths.lock() = paths.clone();
            let entered: Vec<String> = paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            let active = !export_state.should_suppress_import_ui(&entered);
            emit_drag_active(window, active, Some([position.x, position.y]));
        }
        DragDropEvent::Over { position } => {
            let entered: Vec<String> = state
                .last_paths
                .lock()
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            if !export_state.should_suppress_import_ui(&entered) {
                emit_drag_active(window, true, Some([position.x, position.y]));
            }
        }
        DragDropEvent::Drop { paths, position } => {
            let fallback = state.last_paths.lock().clone();
            let resolved = effective_paths(paths, &fallback);
            state.last_paths.lock().clear();
            emit_drag_active(window, false, None);

            if resolved.is_empty() {
                let _ = window.emit(
                    "muzeeka:dropped-tracks",
                    &DroppedTracksPayload {
                        files: Vec::new(),
                        position: [position.x, position.y],
                        message: Some("Could not read dropped files or folders".into()),
                        paths: Vec::new(),
                        ctrl: is_ctrl_held(),
                    },
                );
                return;
            }

            let import_paths = export_state.filter_drop_paths(resolved);
            if import_paths.is_empty() {
                return;
            }

            let scan_window = window.clone();
            let scan_position = [position.x, position.y];
            let ctrl = is_ctrl_held();
            tauri::async_runtime::spawn_blocking(move || {
                emit_drop_result(&scan_window, scan_position, import_paths, ctrl);
            });
        }
        DragDropEvent::Leave => {
            state.last_paths.lock().clear();
            emit_drag_active(window, false, None);
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
