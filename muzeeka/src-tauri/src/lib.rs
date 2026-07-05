// Muzeeka — Tauri application entry point
//
// Wires the BASS player, library scanner, and all IPC commands together.

mod bass;
mod commands;
mod drop_handler;
mod equalizer;
mod library;
mod metadata;
mod player;
mod playlists;
mod settings;

use drop_handler::{handle_window_event, DropState};

use player::Player;
use std::path::PathBuf;
use tauri::Manager;

/// Resolve the directory where bass.dll lives.
///
/// In dev mode we look relative to the src-tauri directory;
/// in production we look next to the executable.
fn resolve_bass_dir() -> PathBuf {
    // Try next to the executable first (production builds)
    if let Ok(exe) = std::env::current_exe() {
        let beside_exe = exe.parent().unwrap_or_else(|| std::path::Path::new(".")).join("bass");
        if beside_exe.exists() {
            return beside_exe;
        }
    }

    // Fallback: src-tauri/bass/ (development)
    let dev_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bass");
    dev_dir
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let bass_dir = resolve_bass_dir();
    let player = Player::new(bass_dir);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(DropState::default())
        .manage(player.clone())
        .on_window_event(|window, event| {
            handle_window_event(window, event);
        })
        .setup(move |app| {
            if let Ok(app_data) = app.path().app_data_dir() {
                metadata::init_cover_cache(app_data);
            }

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval(
                    "document.addEventListener('contextmenu',e=>e.preventDefault(),{capture:true});",
                );
            }

            player.set_app_handle(app.handle().clone());
            player.mark_bass_thread();
            player.init().map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e)
            })?;

            player.start_position_emitter(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::player_init,
            commands::player_play,
            commands::player_pause,
            commands::player_resume,
            commands::player_stop,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_get_state,
            commands::player_get_equalizer,
            commands::player_get_equalizer_status,
            commands::player_set_equalizer,
            commands::load_addon,
            commands::settings_load,
            commands::settings_save,
            commands::library_scan,
            commands::library_scan_paths,
            commands::library_fetch_metadata,
            commands::playlists_load,
            commands::playlists_save,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
