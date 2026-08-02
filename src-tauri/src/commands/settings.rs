// App settings, remote status, and input helpers.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::discord_rpc::DiscordPresence;
use crate::remote_server::{RemoteServer, RemoteStatus};
use crate::settings::{self, AppSettings};

#[tauri::command]
pub fn settings_load(app: AppHandle) -> Result<AppSettings, String> {
    settings::load_settings(&app)
}

#[tauri::command]
pub fn settings_save(
    app: AppHandle,
    discord: State<'_, DiscordPresence>,
    remote: State<'_, Arc<RemoteServer>>,
    mut data: AppSettings,
) -> Result<(), String> {
    discord.configure(data.discord_rpc_enabled);
    // Frontend settings payloads never include window geometry. Preserve whatever
    // the main window last wrote so a Discord/EQ save cannot wipe position/size.
    if data.window_state.is_none() {
        if let Ok(existing) = settings::load_settings(&app) {
            data.window_state = existing.window_state;
        }
    }
    data.remote_port = crate::remote_server::sanitize_port(data.remote_port);
    settings::save_settings(&app, &data)?;
    let status = remote.apply(data.remote_enabled, data.remote_port);
    if let Some(err) = status.last_error.as_ref() {
        eprintln!("[settings_save] remote server: {err}");
    }
    // Let the main player window pick up shuffle mode / other prefs without reload.
    if let Err(e) = app.emit("settings:updated", &data) {
        eprintln!("[settings_save] emit settings:updated failed: {e}");
    }
    Ok(())
}

/// Live remote control server status (IP, port, running, errors).
#[tauri::command]
pub fn remote_status(remote: State<'_, Arc<RemoteServer>>) -> RemoteStatus {
    remote.status()
}

/// Whether Ctrl is currently held (works during OS file drag; WebView often misses key events).
#[tauri::command]
pub fn input_is_ctrl_held() -> bool {
    crate::drop_handler::is_ctrl_held()
}
