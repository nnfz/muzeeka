// Tauri commands — the IPC bridge between frontend and Rust backend
//
// Each `#[tauri::command]` becomes callable from JS via `invoke("command_name", { args })`.

use serde::Deserialize;
use tauri::{AppHandle, State};

use crate::equalizer::EqualizerSettings;
use crate::library;
use crate::player::{EqualizerStatus, GaplessTrack, Player, PlayerStateSnapshot};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NextTrackInput {
    file_path: Option<String>,
    audio_path: Option<String>,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
}

fn parse_gapless_track(input: NextTrackInput) -> Option<GaplessTrack> {
    let track_path = input.file_path.filter(|value| !value.is_empty())?;
    let audio_path = input.audio_path.filter(|value| !value.is_empty())?;
    Some(GaplessTrack {
        track_path,
        audio_path,
        cue_start: input.cue_start,
        cue_end: input.cue_end,
    })
}

fn parse_gapless_queue(queue: Option<Vec<NextTrackInput>>) -> Vec<GaplessTrack> {
    queue
        .unwrap_or_default()
        .into_iter()
        .filter_map(parse_gapless_track)
        .collect()
}
use crate::playlists::{self, PlaylistsData};
use crate::settings::{self, AppSettings};

// ── Player commands ───────────────────────────────────────────────────────────

/// Initialize the BASS audio engine. Must be called once before playback.
#[tauri::command]
pub fn player_init(player: State<'_, Player>) -> Result<(), String> {
    player.init()
}

/// Start playing a file by its full path.
#[tauri::command]
pub fn player_play(
    player: State<'_, Player>,
    file_path: String,
    audio_path: Option<String>,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
    queue: Option<Vec<NextTrackInput>>,
) -> Result<(), String> {
    player.play(
        &file_path,
        audio_path.as_deref(),
        cue_start,
        cue_end,
        parse_gapless_queue(queue),
    )
}

/// Refresh the gapless queue from the current track onward.
#[tauri::command]
pub fn player_prepare_next(
    player: State<'_, Player>,
    queue: Option<Vec<NextTrackInput>>,
) -> Result<(), String> {
    player.prepare_next(parse_gapless_queue(queue))
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

/// Set playback rate multiplier (0.25 to 2.0). Changes speed (and pitch).
#[tauri::command]
pub fn player_set_playback_rate(player: State<'_, Player>, rate: f32) -> Result<(), String> {
    player.set_playback_rate(rate)
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

/// Get the current equalizer settings.
#[tauri::command]
pub fn player_get_equalizer(player: State<'_, Player>) -> EqualizerSettings {
    player.get_equalizer()
}

/// Equalizer diagnostics — whether DSP is attached and processing audio.
#[tauri::command]
pub fn player_get_equalizer_status(player: State<'_, Player>) -> EqualizerStatus {
    player.get_equalizer_status()
}

/// Update equalizer settings (applied live to the DSP chain).
#[tauri::command]
pub fn player_set_equalizer(player: State<'_, Player>, settings: EqualizerSettings) -> Result<(), String> {
    player.set_equalizer(settings)
}

// ── App settings ──────────────────────────────────────────────────────────────

#[tauri::command]
pub fn settings_load(app: AppHandle) -> Result<AppSettings, String> {
    settings::load_settings(&app)
}

#[tauri::command]
pub fn settings_save(app: AppHandle, data: AppSettings) -> Result<(), String> {
    settings::save_settings(&app, &data)
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
