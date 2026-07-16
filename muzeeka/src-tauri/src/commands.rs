// Tauri commands — the IPC bridge between frontend and Rust backend
//
// Each `#[tauri::command]` becomes callable from JS via `invoke("command_name", { args })`.

use serde::Deserialize;
use tauri::{AppHandle, State};

use crate::discord_rpc::DiscordPresence;
use crate::equalizer::EqualizerSettings;
use crate::library;
use crate::player::{EqualizerStatus, GaplessTrack, Player, PlayerStateSnapshot};

fn sync_discord(player: &Player, discord: &DiscordPresence) {
    // Don't block IPC on get_state + metadata reads + Discord network I/O.
    let player = player.clone();
    let discord = discord.clone();
    std::thread::spawn(move || {
        discord.update_from_player(&player.get_state());
    });
}

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
use crate::ytdlp::{self, YtdlpDownloadResult, YtdlpProbeResult};

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
    discord: State<'_, DiscordPresence>,
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
    )?;
    sync_discord(&player, &discord);
    Ok(())
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
pub fn player_pause(player: State<'_, Player>, discord: State<'_, DiscordPresence>) -> Result<(), String> {
    player.pause()?;
    sync_discord(&player, &discord);
    Ok(())
}

/// Resume the current playback.
#[tauri::command]
pub fn player_resume(player: State<'_, Player>, discord: State<'_, DiscordPresence>) -> Result<(), String> {
    player.resume()?;
    sync_discord(&player, &discord);
    Ok(())
}

/// Stop the current playback and discard the stream.
#[tauri::command]
pub fn player_stop(player: State<'_, Player>, discord: State<'_, DiscordPresence>) -> Result<(), String> {
    player.stop()?;
    sync_discord(&player, &discord);
    Ok(())
}

/// Seek to a position in seconds.
#[tauri::command]
pub fn player_seek(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    position: f64,
) -> Result<(), String> {
    player.seek(position)?;
    sync_discord(&player, &discord);
    Ok(())
}

/// Set playback volume (0.0 to 1.0).
#[tauri::command]
pub fn player_set_volume(player: State<'_, Player>, volume: f32) -> Result<(), String> {
    player.set_volume(volume)
}

/// Set playback rate multiplier (0.25 to 2.0).
#[tauri::command]
pub fn player_set_playback_rate(player: State<'_, Player>, rate: f32) -> Result<(), String> {
    player.set_playback_rate(rate)
}

/// Toggle pitch coupling with playback speed (off = preserve pitch via tempo FX).
#[tauri::command]
pub fn player_set_pitch_enabled(player: State<'_, Player>, enabled: bool) -> Result<(), String> {
    player.set_pitch_enabled(enabled)
}

/// Get a snapshot of the current player state.
#[tauri::command]
pub fn player_get_state(player: State<'_, Player>) -> PlayerStateSnapshot {
    player.get_state()
}

/// Load a BASS addon DLL (e.g. "bassflac.dll" or a tracker plugin like "basszxtune.dll").
/// Relative paths resolve against the bass directory.
/// Most tracker/chiptune plugins are auto-loaded if present in the folder.
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
pub fn settings_save(
    app: AppHandle,
    discord: State<'_, DiscordPresence>,
    data: AppSettings,
) -> Result<(), String> {
    discord.configure(data.discord_rpc_enabled);
    settings::save_settings(&app, &data)
}

// ── Library commands ──────────────────────────────────────────────────────────

/// Scan a directory recursively for music files.
#[tauri::command]
pub async fn library_scan(directory: String) -> Result<Vec<library::MusicFile>, String> {
    tauri::async_runtime::spawn_blocking(move || library::scan_directory(&directory))
        .await
        .map_err(|e| format!("Scan task failed: {e}"))?
}

/// Scan dropped file and folder paths for music files.
#[tauri::command]
pub async fn library_scan_paths(paths: Vec<String>) -> Result<Vec<library::MusicFile>, String> {
    tauri::async_runtime::spawn_blocking(move || library::scan_paths(&paths))
        .await
        .map_err(|e| format!("Scan task failed: {e}"))?
}

/// Read or refresh metadata for known file paths.
#[tauri::command]
pub async fn library_fetch_metadata(paths: Vec<String>) -> Result<Vec<library::MusicFile>, String> {
    tauri::async_runtime::spawn_blocking(move || library::fetch_metadata(&paths))
        .await
        .map_err(|e| format!("Metadata task failed: {e}"))?
}

/// Resolve a full-resolution cover path for a track (creates cache if needed).
#[tauri::command]
pub async fn library_resolve_full_cover(path: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::path::Path;
        if crate::cue::is_cue_track_path(&path) {
            if let Some((audio, _)) = crate::cue::parse_virtual_cue_path(&path) {
                return crate::metadata::resolve_full_cover(Path::new(&audio));
            }
            return None;
        }
        crate::metadata::resolve_full_cover(Path::new(&path))
    })
    .await
    .map_err(|e| format!("Cover resolve task failed: {e}"))
}

/// Return a data URL for a cover image file (works for library paths outside asset scope).
#[tauri::command]
pub fn library_cover_data_url(path: String) -> Result<Option<String>, String> {
    use std::path::Path;
    crate::metadata::cover_data_url(Path::new(&path))
}

/// Fetch synchronized lyrics TTML (network + disk cache) on a background thread.
#[tauri::command]
pub async fn lyrics_fetch(
    title: String,
    artist: String,
    album: Option<String>,
    duration_secs: Option<u32>,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::lyrics::fetch_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
        )
    })
    .await
    .map_err(|error| format!("Lyrics fetch task failed: {error}"))?
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

// ── yt-dlp ────────────────────────────────────────────────────────────────────

/// Check whether a string looks like a supported media URL.
#[tauri::command]
pub fn ytdlp_is_url(url: String) -> bool {
    ytdlp::is_supported_url(&url)
}

/// Check whether the yt-dlp binary is available.
#[tauri::command]
pub fn ytdlp_available(app: AppHandle) -> bool {
    ytdlp::ytdlp_available(&app)
}

/// Check whether a bundled ffmpeg binary is available in the bin folder.
#[tauri::command]
pub fn ytdlp_ffmpeg_available(app: AppHandle) -> bool {
    ytdlp::ffmpeg_available(&app)
}

/// Probe a URL for title/metadata without downloading.
#[tauri::command]
pub async fn ytdlp_probe(app: AppHandle, url: String) -> Result<YtdlpProbeResult, String> {
    tauri::async_runtime::spawn_blocking(move || ytdlp::probe(&app, &url))
        .await
        .map_err(|_| "Probe task failed".to_string())?
}

/// Download audio from a URL. Emits `ytdlp:progress` events during download.
#[tauri::command]
pub async fn ytdlp_download(
    app: AppHandle,
    url: String,
    output_dir: Option<String>,
    allow_playlist: Option<bool>,
) -> Result<YtdlpDownloadResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        ytdlp::download(
            &app,
            &url,
            output_dir.as_deref(),
            allow_playlist.unwrap_or(false),
        )
    })
    .await
    .map_err(|_| "Download task failed".to_string())?
}

/// Cancel an in-progress download.
#[tauri::command]
pub fn ytdlp_cancel() {
    ytdlp::cancel_download();
}

/// Get the default download folder path.
#[tauri::command]
pub fn ytdlp_default_download_dir(app: AppHandle) -> Result<String, String> {
    ytdlp::default_download_dir(&app)
        .map(|p| p.to_string_lossy().to_string())
}
