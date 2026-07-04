// Tauri commands — the IPC bridge between frontend and Rust backend
//
// Each `#[tauri::command]` becomes callable from JS via `invoke("command_name", { args })`.

use tauri::{AppHandle, State};

use crate::library;
use crate::player::{Player, PlayerStateSnapshot};
use crate::playlists::{self, PlaylistsData};

// ── Player commands ───────────────────────────────────────────────────────────

/// Initialize the BASS audio engine. Must be called once before playback.
#[tauri::command]
pub fn player_init(player: State<'_, Player>) -> Result<(), String> {
    player.init()
}

/// Start playing a file by its full path.
#[tauri::command]
pub fn player_play(player: State<'_, Player>, file_path: String) -> Result<(), String> {
    player.play(&file_path)
}

/// Pause the current playback.
#[tauri::command]
pub fn player_pause(player: State<'_, Player>) -> Result<(), String> {
    player.pause()
}

/// Resume the current playback.
#[tauri::command]
pub fn player_resume(player: State<'_, Player>) -> Result<(), String> {
    player.resume()
}

/// Stop the current playback and discard the stream.
#[tauri::command]
pub fn player_stop(player: State<'_, Player>) -> Result<(), String> {
    player.stop()
}

/// Seek to a position in seconds.
#[tauri::command]
pub fn player_seek(player: State<'_, Player>, position: f64) -> Result<(), String> {
    player.seek(position)
}

/// Set playback volume (0.0 to 1.0).
#[tauri::command]
pub fn player_set_volume(player: State<'_, Player>, volume: f32) -> Result<(), String> {
    player.set_volume(volume)
}

/// Get a snapshot of the current player state.
#[tauri::command]
pub fn player_get_state(player: State<'_, Player>) -> PlayerStateSnapshot {
    player.get_state()
}

/// Load a BASS addon DLL (e.g. "bassflac.dll"). Relative paths resolve against
/// the bass directory.
#[tauri::command]
pub fn load_addon(player: State<'_, Player>, path: String) -> Result<(), String> {
    player.load_addon(&path)
}

// ── Library commands ──────────────────────────────────────────────────────────

/// Scan a directory recursively for music files.
#[tauri::command]
pub fn library_scan(directory: String) -> Result<Vec<library::MusicFile>, String> {
    library::scan_directory(&directory)
}

/// Scan dropped file and folder paths for music files.
#[tauri::command]
pub fn library_scan_paths(paths: Vec<String>) -> Result<Vec<library::MusicFile>, String> {
    library::scan_paths(&paths)
}

/// Read or refresh metadata for known file paths.
#[tauri::command]
pub fn library_fetch_metadata(paths: Vec<String>) -> Result<Vec<library::MusicFile>, String> {
    library::fetch_metadata(&paths)
}

// ── Playlist persistence ──────────────────────────────────────────────────────

/// Load saved playlists from disk.
#[tauri::command]
pub fn playlists_load(app: AppHandle) -> Result<PlaylistsData, String> {
    playlists::load_playlists(&app)
}

/// Save playlists to disk.
#[tauri::command]
pub fn playlists_save(app: AppHandle, data: PlaylistsData) -> Result<(), String> {
    playlists::save_playlists(&app, &data)
}
