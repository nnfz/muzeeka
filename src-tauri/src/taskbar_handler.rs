// Windows taskbar thumbnail toolbar — button handlers and UI sync.
//
// Button clicks are handled on a background thread so we never call
// `run_on_main_thread` while already inside a Windows message handler (deadlock).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Listener, Manager};
use tauri_plugin_taskbar::TaskbarExt;

use crate::remote_control::RemoteController;

const TOGGLE_DEBOUNCE_MS: u64 = 120;

static LAST_TOGGLE_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Refresh play/pause and prev/next enabled state on the taskbar preview.
pub fn sync_taskbar(controller: &RemoteController) {
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

fn spawn_action(controller: Arc<RemoteController>, action: fn(&RemoteController) -> Result<(), String>) {
    std::thread::spawn(move || {
        if let Err(error) = action(&controller) {
            eprintln!("Taskbar action failed: {error}");
        }
        sync_taskbar(&controller);
    });
}

fn toggle_action(controller: &RemoteController) -> Result<(), String> {
    let now = now_ms();
    let last = LAST_TOGGLE_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < TOGGLE_DEBOUNCE_MS {
        return Ok(());
    }
    LAST_TOGGLE_MS.store(now, Ordering::Relaxed);
    controller.toggle()
}

pub fn setup(app: &AppHandle, controller: Arc<RemoteController>) {
    #[cfg(windows)]
    {
        let ctrl = controller.clone();
        let _ = app.listen("media-toggle", move |_event| {
            spawn_action(ctrl.clone(), toggle_action);
        });

        let ctrl = controller.clone();
        let _ = app.listen("media-prev", move |_event| {
            spawn_action(ctrl.clone(), |c| c.prev());
        });

        let ctrl = controller.clone();
        let _ = app.listen("media-next", move |_event| {
            spawn_action(ctrl.clone(), |c| c.next());
        });

        let ctrl = controller.clone();
        let _ = app.listen("player:track-changed", move |_event| {
            let c = ctrl.clone();
            std::thread::spawn(move || sync_taskbar(&c));
        });

        sync_taskbar(&controller);
    }
}