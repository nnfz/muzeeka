//! Transparent always-on-top drag preview that can render *outside* the main window.
//!
//! Click-through is mandatory: if ignore-cursor-events fails, the overlay would
//! sit on top of Muzeeka and swallow all UI clicks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

pub const DRAG_FLOAT_LABEL: &str = "drag-float";

const WIN_W: u32 = 280;
const WIN_H: u32 = 78;
const OFFSET_X: i32 = 18;
const OFFSET_Y: i32 = 16;
const POLL_MS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragFloatPayload {
    pub title: String,
    pub artist: String,
    pub cover_path: Option<String>,
    pub count: u32,
    pub is_copy: bool,
    #[serde(default)]
    pub rotate: f32,
}

struct DragFloatInner {
    follow: AtomicBool,
    last_payload: Mutex<Option<DragFloatPayload>>,
}

#[derive(Clone)]
pub struct DragFloatState {
    inner: Arc<DragFloatInner>,
}

impl DragFloatState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DragFloatInner {
                follow: AtomicBool::new(false),
                last_payload: Mutex::new(None),
            }),
        }
    }
}

#[cfg(windows)]
fn cursor_pos_physical() -> Option<(i32, i32)> {
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    extern "system" {
        fn GetCursorPos(lp_point: *mut Point) -> i32;
    }
    let mut p = Point { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } != 0 {
        Some((p.x, p.y))
    } else {
        None
    }
}

#[cfg(not(windows))]
fn cursor_pos_physical() -> Option<(i32, i32)> {
    None
}

fn force_click_through(win: &WebviewWindow) {
    // Set twice: some WebView2 builds only apply after the window is shown.
    let _ = win.set_ignore_cursor_events(true);
}

fn park_offscreen(win: &WebviewWindow) {
    force_click_through(win);
    let _ = win.hide();
    // Belt-and-suspenders: even if hide fails, don't cover the player.
    let _ = win.set_position(PhysicalPosition::new(-32000, -32000));
}

fn ensure_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(win) = app.get_webview_window(DRAG_FLOAT_LABEL) {
        force_click_through(&win);
        return Ok(win);
    }

    let url = if cfg!(debug_assertions) {
        WebviewUrl::External(
            "http://localhost:1420/"
                .parse()
                .map_err(|e| format!("drag-float url: {e}"))?,
        )
    } else {
        WebviewUrl::App("index.html".into())
    };

    let win = WebviewWindowBuilder::new(app, DRAG_FLOAT_LABEL, url)
        .title("Drag")
        .inner_size(WIN_W as f64, WIN_H as f64)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .focused(false)
        .visible(false)
        .shadow(false)
        .build()
        .map_err(|e| format!("create drag-float window: {e}"))?;

    force_click_through(&win);
    let _ = win.set_always_on_top(true);
    park_offscreen(&win);

    Ok(win)
}

fn place_at_cursor(win: &WebviewWindow) {
    let Some((x, y)) = cursor_pos_physical() else {
        return;
    };
    let _ = win.set_size(PhysicalSize::new(WIN_W, WIN_H));
    let _ = win.set_position(PhysicalPosition::new(x + OFFSET_X, y + OFFSET_Y));
    force_click_through(win);
}

fn emit_payload(app: &AppHandle, payload: &DragFloatPayload) {
    let _ = app.emit_to(DRAG_FLOAT_LABEL, "drag-float:update", payload);
    let _ = app.emit("drag-float:update", payload);
}

fn start_follow_loop(app: AppHandle, state: DragFloatState) {
    if state.inner.follow.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        while state.inner.follow.load(Ordering::SeqCst) {
            if let Some(win) = app.get_webview_window(DRAG_FLOAT_LABEL) {
                place_at_cursor(&win);
            } else {
                break;
            }
            thread::sleep(Duration::from_millis(POLL_MS));
        }
        state.inner.follow.store(false, Ordering::SeqCst);
    });
}

fn stop_follow(state: &DragFloatState) {
    state.inner.follow.store(false, Ordering::SeqCst);
}

#[tauri::command]
pub fn drag_float_show(
    app: AppHandle,
    state: tauri::State<'_, DragFloatState>,
    payload: DragFloatPayload,
) -> Result<(), String> {
    let win = ensure_window(&app)?;
    *state.inner.last_payload.lock() = Some(payload.clone());

    // Click-through BEFORE show — critical so we never block the main UI.
    force_click_through(&win);
    place_at_cursor(&win);
    win.show().map_err(|e| format!("show drag-float: {e}"))?;
    force_click_through(&win);
    let _ = win.set_always_on_top(true);

    // Return focus to main immediately (overlay must never keep it).
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_focus();
    }

    emit_payload(&app, &payload);
    let app2 = app.clone();
    let payload2 = payload.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        emit_payload(&app2, &payload2);
        if let Some(win) = app2.get_webview_window(DRAG_FLOAT_LABEL) {
            force_click_through(&win);
        }
        thread::sleep(Duration::from_millis(250));
        emit_payload(&app2, &payload2);
    });

    start_follow_loop(app, (*state).clone());
    Ok(())
}

#[tauri::command]
pub fn drag_float_update(
    app: AppHandle,
    state: tauri::State<'_, DragFloatState>,
    payload: DragFloatPayload,
) -> Result<(), String> {
    *state.inner.last_payload.lock() = Some(payload.clone());
    if let Some(win) = app.get_webview_window(DRAG_FLOAT_LABEL) {
        force_click_through(&win);
        emit_payload(&app, &payload);
    }
    Ok(())
}

#[tauri::command]
pub fn drag_float_hide(
    app: AppHandle,
    state: tauri::State<'_, DragFloatState>,
) -> Result<(), String> {
    stop_follow(&state);
    *state.inner.last_payload.lock() = None;
    if let Some(win) = app.get_webview_window(DRAG_FLOAT_LABEL) {
        force_click_through(&win);
        park_offscreen(&win);
        // Destroy so a stuck overlay can never cover the player after a crash.
        let _ = win.close();
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub fn drag_float_get_payload(
    state: tauri::State<'_, DragFloatState>,
) -> Option<DragFloatPayload> {
    state.inner.last_payload.lock().clone()
}
