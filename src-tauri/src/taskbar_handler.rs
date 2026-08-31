// Windows taskbar thumbnail toolbar — button handlers and UI sync.
//
// Button clicks are handled on a background thread so we never call
// `run_on_main_thread` while already inside a Windows message handler (deadlock).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Listener, Manager};
use tauri_plugin_taskbar::TaskbarExt;

use crate::session::PlaybackSession;

const TOGGLE_DEBOUNCE_MS: u64 = 120;
const NAV_DEBOUNCE_MS: u64 = 120;

static LAST_TOGGLE_MS: AtomicU64 = AtomicU64::new(0);
static LAST_NAV_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Claim a debounce slot. Returns false if another thread already acted recently
/// (or won the CAS race).
fn claim_debounce(slot: &AtomicU64, debounce_ms: u64) -> bool {
    let now = now_ms();
    let last = slot.load(Ordering::Relaxed);
    if now.saturating_sub(last) < debounce_ms {
        return false;
    }
    slot.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// Refresh play/pause and prev/next enabled state on the taskbar preview.
pub fn sync_taskbar(controller: &PlaybackSession) {
    #[cfg(windows)]
    {
        let app = controller.app_handle();
        let Some(window) = app.get_webview_window("main") else {
            return;
        };

        let snapshot = controller.player_snapshot();
        let (has_prev, has_next) = controller.navigation_enabled();

        let taskbar = app.taskbar();
        let _ = taskbar.set_playback_state(&window, snapshot.is_playing);
        let _ = taskbar.set_navigation_enabled(&window, has_prev, has_next);
    }

    #[cfg(not(windows))]
    {
        let _ = controller;
    }
}

fn spawn_action<F>(controller: Arc<PlaybackSession>, action: F)
where
    F: FnOnce(&PlaybackSession) -> Result<(), String> + Send + 'static,
{
    std::thread::spawn(move || {
        if let Err(error) = action(&controller) {
            eprintln!("Taskbar action failed: {error}");
        }
        sync_taskbar(&controller);
    });
}

fn toggle_action(controller: &PlaybackSession) -> Result<(), String> {
    if !claim_debounce(&LAST_TOGGLE_MS, TOGGLE_DEBOUNCE_MS) {
        return Ok(());
    }
    controller.toggle()
}

fn prev_action(controller: &PlaybackSession) -> Result<(), String> {
    if !claim_debounce(&LAST_NAV_MS, NAV_DEBOUNCE_MS) {
        return Ok(());
    }
    controller.prev()
}

fn next_action(controller: &PlaybackSession) -> Result<(), String> {
    if !claim_debounce(&LAST_NAV_MS, NAV_DEBOUNCE_MS) {
        return Ok(());
    }
    controller.next()
}

pub fn setup(app: &AppHandle, controller: Arc<PlaybackSession>) {
    #[cfg(windows)]
    {
        let ctrl = controller.clone();
        let _ = app.listen("media-toggle", move |_event| {
            spawn_action(ctrl.clone(), toggle_action);
        });

        let ctrl = controller.clone();
        let _ = app.listen("media-prev", move |_event| {
            spawn_action(ctrl.clone(), prev_action);
        });

        let ctrl = controller.clone();
        let _ = app.listen("media-next", move |_event| {
            spawn_action(ctrl.clone(), next_action);
        });

        let ctrl = controller.clone();
        let _ = app.listen("player:track-changed", move |_event| {
            let c = ctrl.clone();
            std::thread::spawn(move || sync_taskbar(&c));
        });

        sync_taskbar(&controller);
    }
}
