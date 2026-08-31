// yt-dlp related Tauri commands.

use tauri::AppHandle;

use crate::ytdlp::{self, YtdlpDownloadResult, YtdlpProbeResult};

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
    ytdlp::default_download_dir(&app).map(|p| p.to_string_lossy().to_string())
}

#[tauri::command]
pub fn ytdlp_youtube_auth_status(app: AppHandle) -> crate::youtube_auth::YoutubeAuthStatus {
    crate::youtube_auth::auth_status(&app)
}

/// Open a YouTube login window and wait until login cookies are saved.
#[tauri::command]
pub async fn ytdlp_youtube_login(
    app: AppHandle,
    force: Option<bool>,
) -> Result<crate::youtube_auth::YoutubeAuthStatus, String> {
    crate::youtube_auth::login(app, force.unwrap_or(false)).await
}

#[tauri::command]
pub async fn ytdlp_youtube_logout(
    app: AppHandle,
) -> Result<crate::youtube_auth::YoutubeAuthStatus, String> {
    crate::youtube_auth::logout(app).await
}
