// Tauri commands — the IPC bridge between frontend and Rust backend
//
// Each `#[tauri::command]` becomes callable from JS via `invoke("command_name", { args })`.

use std::sync::Arc;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::discord_rpc::DiscordPresence;
use crate::remote_control::RemoteController;

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

fn sync_playback_ui_async(controller: Arc<RemoteController>) {
    std::thread::spawn(move || {
        controller.notify_playback();
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
    controller: State<'_, Arc<RemoteController>>,
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
    sync_playback_ui_async(Arc::clone(controller.inner()));
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
pub fn player_pause(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<RemoteController>>,
) -> Result<(), String> {
    player.pause()?;
    sync_discord(&player, &discord);
    sync_playback_ui_async(Arc::clone(controller.inner()));
    Ok(())
}

/// Resume the current playback.
#[tauri::command]
pub fn player_resume(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<RemoteController>>,
) -> Result<(), String> {
    player.resume()?;
    sync_discord(&player, &discord);
    sync_playback_ui_async(Arc::clone(controller.inner()));
    Ok(())
}

/// Stop the current playback and discard the stream.
#[tauri::command]
pub fn player_stop(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<RemoteController>>,
) -> Result<(), String> {
    player.stop()?;
    sync_discord(&player, &discord);
    sync_playback_ui_async(Arc::clone(controller.inner()));
    Ok(())
}

/// Seek to a position in seconds.
#[tauri::command]
pub fn player_seek(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<RemoteController>>,
    position: f64,
) -> Result<(), String> {
    player.seek(position)?;
    sync_discord(&player, &discord);
    sync_playback_ui_async(Arc::clone(controller.inner()));
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

// ── Input helpers ─────────────────────────────────────────────────────────────

/// Whether Ctrl is currently held (works during OS file drag; WebView often misses key events).
#[tauri::command]
pub fn input_is_ctrl_held() -> bool {
    crate::drop_handler::is_ctrl_held()
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

/// Wipe the track cover cache, re-extract covers as WebP, convert playlist GIF/JPG → WebP.
#[tauri::command]
pub async fn library_rebuild_covers(
    app: AppHandle,
) -> Result<crate::metadata::CoverRebuildStats, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut data = playlists::load_playlists(&app)?;

        let mut track_paths: Vec<String> = Vec::new();
        for pl in &data.playlists {
            for t in &pl.tracks {
                track_paths.push(t.path.clone());
            }
        }
        track_paths.extend(data.all_paths.iter().cloned());
        track_paths.extend(data.liked_paths.iter().cloned());

        let playlist_covers: Vec<(String, Option<String>)> = data
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.cover_path.clone()))
            .collect();

        let (stats, cover_updates) =
            crate::metadata::rebuild_cover_cache(&track_paths, &playlist_covers)?;

        // Refresh track cover paths from regenerated cache (clear stale paths too).
        for pl in &mut data.playlists {
            for track in &mut pl.tracks {
                let (thumb, full) = crate::metadata::fresh_cover_paths_for_track(&track.path);
                track.cover_path = thumb;
                track.cover_path_full = full;
            }
        }

        for (id, path) in cover_updates {
            if let Some(pl) = data.playlists.iter_mut().find(|p| p.id == id) {
                pl.cover_path = path;
            }
        }

        playlists::save_playlists(&app, &data)?;
        let _ = app.emit("covers:rebuilt", &stats);
        Ok(stats)
    })
    .await
    .map_err(|e| format!("Cover rebuild task failed: {e}"))?
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

/// Import a local TTML file into the lyrics cache for a track.
#[tauri::command]
pub async fn lyrics_import_ttml(
    app: AppHandle,
    title: String,
    artist: String,
    album: Option<String>,
    duration_secs: Option<u32>,
    path: String,
    track_path: Option<String>,
) -> Result<(), String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let ttml = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read TTML file: {e}"))?;
        crate::lyrics::import_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
            &ttml,
        )?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|error| format!("Lyrics import task failed: {error}"))?;

    result?;
    let _ = app.emit(
        "lyrics:imported",
        track_path.unwrap_or_default(),
    );
    Ok(())
}

/// Remove cached lyrics for a track (and stop auto-refetch until re-import).
#[tauri::command]
pub async fn lyrics_clear(
    app: AppHandle,
    title: String,
    artist: String,
    album: Option<String>,
    duration_secs: Option<u32>,
    track_path: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::lyrics::clear_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
        )
    })
    .await
    .map_err(|error| format!("Lyrics clear task failed: {error}"))??;

    let _ = app.emit(
        "lyrics:cleared",
        track_path.unwrap_or_default(),
    );
    Ok(())
}

/// Force network search for lyrics (ignores hit/miss/cleared cache).
#[tauri::command]
pub async fn lyrics_refetch(
    app: AppHandle,
    title: String,
    artist: String,
    album: Option<String>,
    duration_secs: Option<u32>,
    track_path: Option<String>,
) -> Result<bool, String> {
    let found = tauri::async_runtime::spawn_blocking(move || {
        crate::lyrics::refetch_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
        )
    })
    .await
    .map_err(|error| format!("Lyrics refetch task failed: {error}"))??
    .is_some();

    let _ = app.emit(
        "lyrics:refetched",
        track_path.unwrap_or_default(),
    );
    Ok(found)
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

/// Cache a user-selected image as a playlist cover.
#[tauri::command]
pub fn playlist_cache_cover(playlist_id: String, source_path: String) -> Result<String, String> {
    crate::metadata::cache_playlist_cover(&playlist_id, std::path::Path::new(&source_path))
}

/// Download a remote image and store it as the playlist cover.
#[tauri::command]
pub fn playlist_cache_cover_url(playlist_id: String, url: String) -> Result<String, String> {
    crate::metadata::cache_playlist_cover_from_url(&playlist_id, &url)
}

/// Delete a cached custom playlist cover file.
#[tauri::command]
pub fn playlist_remove_cover(playlist_id: String) -> Result<(), String> {
    crate::metadata::remove_playlist_cover_file(&playlist_id)
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
    if crate::vk_audio::is_vk_audio_url(&url) {
        return crate::vk_audio::probe_async(app, url).await;
    }
    if crate::spotdl::is_spotify_url(&url) {
        return tauri::async_runtime::spawn_blocking(move || crate::spotdl::probe(&app, &url))
            .await
            .map_err(|_| "Probe task failed".to_string())?;
    }
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
    if crate::vk_audio::is_vk_audio_url(&url) {
        return crate::vk_audio::download_async(
            app,
            url,
            output_dir,
            allow_playlist.unwrap_or(false),
        )
        .await;
    }
    if crate::spotdl::is_spotify_url(&url) {
        return tauri::async_runtime::spawn_blocking(move || {
            crate::spotdl::download(
                &app,
                &url,
                output_dir.as_deref(),
                allow_playlist.unwrap_or(false),
            )
        })
        .await
        .map_err(|_| "Download task failed".to_string())?;
    }
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

// ── VK auth ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn vk_auth_status(app: AppHandle) -> crate::vk_audio::VkAuthStatus {
    crate::vk_audio::auth_status(&app)
}

/// Open VK login window and wait until the user signs in.
#[tauri::command]
pub async fn vk_login(app: AppHandle) -> Result<crate::vk_audio::VkAuthStatus, String> {
    crate::vk_audio::login(app).await
}

/// Clear saved VK session.
#[tauri::command]
pub async fn vk_logout(app: AppHandle) -> Result<crate::vk_audio::VkAuthStatus, String> {
    crate::vk_audio::logout(app).await
}
