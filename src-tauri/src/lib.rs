// Muzeeka — Tauri application entry point
//
// Wires the BASS player, library scanner, and all IPC commands together.

mod bass;
mod bpm;
mod commands;
mod cue;
mod discord_rpc;
mod drop_handler;
mod drag_float;
mod file_drag;
mod cover_url_cache;
mod imgbb;
mod musicbrainz;

mod biquad;
mod dsp_chain;
mod equalizer;
mod filter;
mod limiter;
mod mix_filter;
mod library;
mod m3u;
mod lrc;
mod lrclib;
mod lyrics;
mod unison;
mod metadata;
mod tag_table;
mod waveform;
mod player;
mod path_store;
mod playlists;
mod process_util;
mod remote_control;
mod remote_server;
mod settings;
mod taskbar_handler;
mod vk_audio;
mod ytdlp;

use discord_rpc::DiscordPresence;
use drop_handler::{handle_window_event, DropState, ExportDragState};

use parking_lot::Mutex;
use player::Player;
use remote_control::RemoteController;
use remote_server::RemoteServer;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::path::BaseDirectory;
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, WindowEvent};

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

/// Load secrets / local overrides from `.env` (project root or `src-tauri/`).
/// Existing process env vars always win — `.env` only fills missing keys.
fn load_dotenv() {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".env"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"),
        PathBuf::from(".env"),
    ];
    for path in candidates {
        if path.is_file() {
            match dotenvy::from_path(&path) {
                Ok(()) => {
                    eprintln!("Loaded env from {}", path.display());
                    return;
                }
                Err(error) => {
                    eprintln!("Failed to load {}: {error}", path.display());
                }
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    load_dotenv();

    let player = Player::new();
    let player_for_close = player.clone();
    let player_for_focus = player.clone();
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
        .manage(waveform::WaveformCache::default())
        .manage(discord_presence.clone())
        .on_window_event(move |window, event| {
            handle_window_event(window, event);

            if window.label() == "main" {
                match event {
                    WindowEvent::Resized(_) | WindowEvent::Moved(_) => {
                        let mut last_save = last_window_state_save_for_event.lock();
                        if last_save.elapsed() >= Duration::from_millis(700) {
                            let app = window.app_handle();
                            if let Some(webview_window) = app.get_webview_window(window.label()) {
                                save_window_state(app, &webview_window);
                            }
                            *last_save = Instant::now();
                        }
                    }
                    // Reliable path for exclusive-fullscreen games (JS focus can miss).
                    // Throttle WebView position spam + drop process priority for Dota/etc.
                    WindowEvent::Focused(focused) => {
                        player_for_focus.set_ui_hot(*focused);
                        process_util::set_background_mode(!focused);
                        let _ = window.app_handle().emit("app:window-active", *focused);
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
                        save_window_state(app, &webview_window);
                    }
                    // Ensure audio is stopped and BASS device is freed when the main player window closes.
                    // Without this, sound could continue after the app exits.
                    discord_for_close.shutdown();
                    let _ = player_for_close.shutdown();
                    process_util::set_background_mode(false);
                }
            }
        })
        .setup(move |app| {
            let library_database = playlists::LibraryDatabase::open(app.handle())
                .map_err(std::io::Error::other)?;
            app.manage(library_database.clone());

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

            // One settings load for window geometry, EQ, Discord, and remote server.
            let app_settings = settings::load_settings(app.handle()).ok();

            if let Some(window) = app.get_webview_window("main") {
                if let Some(window_state) = app_settings
                    .as_ref()
                    .and_then(|s| s.window_state.as_ref())
                {
                    apply_window_state(&window, window_state);
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
                std::io::Error::other(e)
            })?;

            // Apply the saved effect rack as early as possible (before any playback)
            // so the first seconds of audio are processed by DSP.
            if let Some(ref app_settings) = app_settings {
                let _ = player.set_dsp_chain(app_settings.dsp_chain.clone().unwrap_or_default());
                discord_presence.configure(app_settings.discord_rpc_enabled);
            }

            player.start_position_emitter(app.handle().clone());

            let remote_controller = Arc::new(RemoteController::new(
                player.clone(),
                discord_presence.clone(),
                app.handle().clone(),
                library_database,
            ));
            app.manage(remote_controller.clone());
            taskbar_handler::setup(app.handle(), remote_controller.clone());

            let (remote_enabled, remote_port) = app_settings
                .as_ref()
                .map(|s| (s.remote_enabled, s.remote_port))
                .unwrap_or((true, remote_server::DEFAULT_REMOTE_PORT));
            let remote_http =
                RemoteServer::new(remote_controller, remote_enabled, remote_port);
            app.manage(remote_http);
            app.manage(drag_float::DragFloatState::new());
            // Never leave a leftover always-on-top overlay from a previous crash.
            if let Some(win) = app.get_webview_window(drag_float::DRAG_FLOAT_LABEL) {
                let _ = win.close();
            }

            Ok(())
        })
        // Command modules: commands/{player,library,lyrics,settings,ytdlp,vk}.rs
        .invoke_handler(tauri::generate_handler![
            // Player
            commands::player_init,
            commands::player_play,
            commands::player_mix_crossfade,
            commands::player_arm_mix,
            commands::player_disarm_mix,
            commands::player_prepare_next,
            commands::player_pause,
            commands::player_resume,
            commands::player_stop,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_set_playback_rate,
            commands::player_set_pitch_enabled,
            commands::player_get_state,
            commands::player_get_dsp_chain,
            commands::player_get_dsp_chain_status,
            commands::player_set_dsp_chain,
            commands::load_addon,
            // Settings / remote / input
            commands::settings_load,
            commands::settings_save,
            commands::remote_status,
            commands::input_is_ctrl_held,
            // Library + playlists + covers
            commands::library_scan,
            commands::library_scan_paths,
            commands::library_fetch_metadata,
            commands::library_update_track_metadata,
            commands::library_audio_tech_info,
            commands::library_get_tag_table,
            commands::library_set_tag_table,
            commands::library_set_track_cover,
            commands::library_resolve_cover,
            commands::library_resolve_full_cover,
            commands::library_cover_data_url,
            commands::library_rebuild_covers,
            commands::playlists_load,
            commands::playlists_list_meta,
            commands::library_state_save,
            commands::playlist_create,
            commands::playlist_delete,
            commands::playlist_rename,
            commands::playlist_set_cover_path,
            commands::playlist_set_mix_mode,
            commands::library_tracks_upsert,
            commands::playlist_add_tracks,
            commands::playlist_remove_tracks,
            commands::playlist_reorder,
            commands::playlists_reorder,
            commands::library_reorder,
            commands::library_remove_tracks,
            commands::library_clear_all,
            commands::track_prefs_get_playback_rate,
            commands::track_prefs_set_playback_rate,
            commands::library_detect_bpm,
            commands::library_get_bpm,
            commands::library_detect_beat_offset,
            commands::library_set_track_bpm,
            commands::library_get_waveform,
            commands::library_set_liked,
            commands::library_reorder_liked,
            commands::playlist_cache_cover,
            commands::playlist_cache_cover_url,
            commands::playlist_remove_cover,
            // Lyrics
            commands::lyrics_fetch,
            commands::lyrics_import_ttml,
            commands::lyrics_save_text,
            commands::lyrics_clear,
            commands::lyrics_refetch,
            // yt-dlp
            commands::ytdlp_is_url,
            commands::ytdlp_available,
            commands::ytdlp_ffmpeg_available,
            commands::ytdlp_probe,
            commands::ytdlp_download,
            commands::ytdlp_cancel,
            commands::ytdlp_default_download_dir,
            // VK
            commands::vk_auth_status,
            commands::vk_login,
            commands::vk_logout,
            // Native drag
            file_drag::start_file_drag,
            drag_float::drag_float_show,
            drag_float::drag_float_update,
            drag_float::drag_float_hide,
            drag_float::drag_float_get_payload,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
