// App settings and input helpers.

use tauri::{AppHandle, Emitter, State};

use crate::discord_rpc::DiscordPresence;
use crate::settings::{self, AppSettings};

#[tauri::command]
pub fn settings_load(app: AppHandle) -> Result<AppSettings, String> {
    settings::load_settings(&app)
}

#[tauri::command]
pub fn settings_save(
    app: AppHandle,
    discord: State<'_, DiscordPresence>,
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
    settings::save_settings(&app, &data)?;
    // Let the main player window pick up shuffle mode / other prefs without reload.
    if let Err(e) = app.emit("settings:updated", &data) {
        eprintln!("[settings_save] emit settings:updated failed: {e}");
    }
    Ok(())
}

/// Whether Ctrl is currently held (works during OS file drag; WebView often misses key events).
#[tauri::command]
pub fn input_is_ctrl_held() -> bool {
    crate::drop_handler::is_ctrl_held()
}
