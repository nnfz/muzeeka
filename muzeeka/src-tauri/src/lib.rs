// Muzeeka — Tauri application entry point
//
// Wires the BASS player, library scanner, and all IPC commands together.

mod bass;
mod commands;
mod cue;
mod drop_handler;
mod file_drag;

mod equalizer;
mod library;
mod metadata;
mod player;
mod playlists;
mod settings;
mod ytdlp;

use drop_handler::{handle_window_event, DropState, ExportDragState};

use player::Player;
use std::path::{Path, PathBuf};
use tauri::path::BaseDirectory;
use tauri::{Manager, WindowEvent};

fn bass_dir_is_valid(dir: &Path) -> bool {
    dir.join("bass.dll").is_file()
}

/// Resolve the directory where bass.dll and format plugins live.
fn resolve_bass_dir(app: Option<&tauri::AppHandle>) -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("bass"));
        }
    }

    if let Some(app) = app {
        if let Ok(resource_bass) = app.path().resolve("bass", BaseDirectory::Resource) {
            candidates.push(resource_bass);
        }
    }

    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bass"));

    for dir in candidates {
        if bass_dir_is_valid(&dir) {
            eprintln!("BASS directory: {}", dir.display());
            return dir;
        }
    }

    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bass");
    eprintln!("BASS directory (fallback): {}", fallback.display());
    fallback
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let player = Player::new();
    let player_for_close = player.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(DropState::default())
        .manage(ExportDragState::default())
        .manage(player.clone())
        .on_window_event(move |window, event| {
            handle_window_event(window, event);

            if let WindowEvent::CloseRequested { .. } = event {
                // Only shut down BASS when the *main* window is closed.
                // The settings window (label "settings") and other secondary windows
                // must not stop playback or free the audio device.
                if window.label() == "main" {
                    // Ensure audio is stopped and BASS device is freed when the main player window closes.
                    // Without this, sound could continue after the app exits.
                    let _ = player_for_close.shutdown();
                }
            }
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

            player.set_bass_dir(resolve_bass_dir(Some(app.handle())));
            player.set_app_handle(app.handle().clone());
            player.mark_bass_thread();
            player.init().map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e)
            })?;

            // Apply saved equalizer settings as early as possible (before any playback)
            // so the first seconds of audio are processed by DSP.
            if let Ok(app_settings) = settings::load_settings(&app.handle()) {
                let _ = player.set_equalizer(app_settings.equalizer);
            }

            player.start_position_emitter(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::player_init,
            commands::player_play,
            commands::player_prepare_next,
            commands::player_pause,
            commands::player_resume,
            commands::player_stop,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_set_playback_rate,
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
            commands::ytdlp_is_url,
            commands::ytdlp_available,
            commands::ytdlp_ffmpeg_available,
            commands::ytdlp_probe,
            commands::ytdlp_download,
            commands::ytdlp_cancel,
            commands::ytdlp_default_download_dir,
            file_drag::start_file_drag,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
