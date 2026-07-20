// Muzeeka — Tauri application entry point
//
// Wires the BASS player, library scanner, and all IPC commands together.

mod bass;
mod commands;
mod cue;
mod discord_rpc;
mod drop_handler;
mod file_drag;
mod cover_url_cache;
mod imgbb;
mod musicbrainz;

mod equalizer;
mod library;
mod lrc;
mod lrclib;
mod lyrics;
mod unison;
mod metadata;
mod player;
mod playlists;
mod process_util;
mod remote_control;
mod remote_server;
mod settings;
mod taskbar_handler;
mod spotdl;
mod vk_audio;
mod ytdlp;

use discord_rpc::DiscordPresence;
use drop_handler::{handle_window_event, DropState, ExportDragState};

use player::Player;
use remote_control::RemoteController;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::path::BaseDirectory;
use tauri::{LogicalPosition, LogicalSize, Manager, WindowEvent};

fn bass_dir_is_valid(dir: &Path) -> bool {
    dir.join("bass.dll").is_file()
}

fn is_valid_window_position(x: i32, y: i32) -> bool {
    // Windows uses -32000 when minimized; never restore or persist that.
    x > -500 && y > -500 && x < 16000 && y < 16000
}

fn apply_window_state(window: &tauri::WebviewWindow, state: &settings::WindowState) {
    let width = state.width.clamp(800, 3840);
    let height = state.height.clamp(600, 2160);
    let _ = window.set_size(LogicalSize::new(width as f64, height as f64));
    if is_valid_window_position(state.x, state.y) {
        let _ = window.set_position(LogicalPosition::new(state.x as f64, state.y as f64));
    } else {
        let _ = window.center();
    }
    if state.maximized {
        let _ = window.maximize();
    }
}

fn capture_window_state(window: &tauri::WebviewWindow) -> Option<settings::WindowState> {
    let maximized = window.is_maximized().unwrap_or(false);
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;

    let (x, y) = if is_valid_window_position(position.x, position.y) {
        (position.x, position.y)
    } else {
        match settings::load_settings(window.app_handle()) {
            Ok(app_settings) => {
                if let Some(saved) = app_settings.window_state.as_ref() {
                    if is_valid_window_position(saved.x, saved.y) {
                        (saved.x, saved.y)
                    } else {
                        (100, 100)
                    }
                } else {
                    (100, 100)
                }
            }
            Err(_) => (100, 100),
        }
    };

    Some(settings::WindowState {
        x,
        y,
        width: size.width.max(800),
        height: size.height.max(600),
        maximized,
    })
}

fn save_window_state(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let Some(window_state) = capture_window_state(window) else {
        return;
    };

    match settings::load_settings(app) {
        Ok(mut app_settings) => {
            app_settings.window_state = Some(window_state);
            if let Err(error) = settings::save_settings(app, &app_settings) {
                eprintln!("Failed to save window state: {error}");
            }
        }
        Err(error) => eprintln!("Failed to load settings for window state: {error}"),
    }
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
    let discord_presence = DiscordPresence::new();
    let discord_for_close = discord_presence.clone();
    let last_window_state_save = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));
    let last_window_state_save_for_event = Arc::clone(&last_window_state_save);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_taskbar::init())
        .manage(DropState::default())
        .manage(ExportDragState::default())
        .manage(player.clone())
        .manage(discord_presence.clone())
        .on_window_event(move |window, event| {
            handle_window_event(window, event);

            if window.label() == "main" {
                match event {
                    WindowEvent::Resized(_) | WindowEvent::Moved(_) => {
                        if let Ok(mut last_save) = last_window_state_save_for_event.lock() {
                            if last_save.elapsed() >= Duration::from_millis(700) {
                                let app = window.app_handle();
                                if let Some(webview_window) = app.get_webview_window(window.label()) {
                                    save_window_state(&app, &webview_window);
                                }
                                *last_save = Instant::now();
                            }
                        }
                    }
                    _ => {}
                }
            }

            if let WindowEvent::CloseRequested { .. } = event {
                // Only shut down BASS when the *main* window is closed.
                // The settings window (label "settings") and other secondary windows
                // must not stop playback or free the audio device.
                if window.label() == "main" {
                    let app = window.app_handle();
                    if let Some(webview_window) = app.get_webview_window(window.label()) {
                        save_window_state(&app, &webview_window);
                    }
                    // Ensure audio is stopped and BASS device is freed when the main player window closes.
                    // Without this, sound could continue after the app exits.
                    discord_for_close.shutdown();
                    let _ = player_for_close.shutdown();
                }
            }
        })
        .setup(move |app| {
            if let Ok(app_data) = app.path().app_data_dir() {
                metadata::init_cover_cache(app_data.clone());
                lyrics::init_lyrics_cache(app_data.clone());
                cover_url_cache::init(app_data);
            }

            // ffmpeg for animated GIF → WebP cover conversion
            let ffmpeg = ytdlp::resolve_ffmpeg_location(app.handle()).and_then(|dir| {
                let bin = dir.join(if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                });
                bin.is_file().then_some(bin)
            });
            match &ffmpeg {
                Some(path) => eprintln!("[init] ffmpeg for GIF→WebP: {}", path.display()),
                None => eprintln!("[init] ffmpeg NOT found — animated GIF covers will lose animation"),
            }
            metadata::set_ffmpeg_bin(ffmpeg);

            if let Some(window) = app.get_webview_window("main") {
                if let Ok(app_settings) = settings::load_settings(&app.handle()) {
                    if let Some(window_state) = app_settings.window_state.as_ref() {
                        apply_window_state(&window, window_state);
                    }
                }
                let _ = window.eval(
                    "document.addEventListener('contextmenu',e=>e.preventDefault(),{capture:true});",
                );
            }

            player.set_bass_dir(resolve_bass_dir(Some(app.handle())));
            player.set_app_handle(app.handle().clone());
            player.set_discord_presence(discord_presence.clone());
            player.mark_bass_thread();
            player.init().map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e)
            })?;

            // Apply saved equalizer settings as early as possible (before any playback)
            // so the first seconds of audio are processed by DSP.
            if let Ok(app_settings) = settings::load_settings(&app.handle()) {
                let _ = player.set_equalizer(app_settings.equalizer);
                discord_presence.configure(app_settings.discord_rpc_enabled);
            }

            player.start_position_emitter(app.handle().clone());

            let remote_controller = Arc::new(RemoteController::new(
                player.clone(),
                discord_presence.clone(),
                app.handle().clone(),
            ));
            app.manage(remote_controller.clone());
            taskbar_handler::setup(app.handle(), remote_controller.clone());
            remote_server::start(remote_controller);

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
            commands::player_set_pitch_enabled,
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
            commands::library_resolve_full_cover,
            commands::library_cover_data_url,
            commands::library_rebuild_covers,
            commands::lyrics_fetch,
            commands::lyrics_import_ttml,
            commands::lyrics_clear,
            commands::lyrics_refetch,
            commands::playlists_load,
            commands::playlists_save,
            commands::playlist_cache_cover,
            commands::playlist_cache_cover_url,
            commands::playlist_remove_cover,
            commands::ytdlp_is_url,
            commands::ytdlp_available,
            commands::ytdlp_ffmpeg_available,
            commands::ytdlp_probe,
            commands::ytdlp_download,
            commands::ytdlp_cancel,
            commands::ytdlp_default_download_dir,
            commands::vk_auth_status,
            commands::vk_login,
            commands::vk_logout,
            file_drag::start_file_drag,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
