// Lyrics-related Tauri commands.

use tauri::{AppHandle, Emitter};

/// Fetch synchronized lyrics TTML (network + disk cache) on a background thread.
///
/// `track_path` is optional and only used as a last resort: when neither the cache nor
/// any provider has anything, lyrics embedded in the audio file's own tags
/// (`LYRICS` / `USLT`) are wrapped into unsynced TTML. Without this the properties
/// window showed the file's own text while the fullscreen view stayed empty, because
/// only properties had that fallback.
#[tauri::command]
pub async fn lyrics_fetch(
    title: String,
    artist: String,
    album: Option<String>,
    duration_secs: Option<u32>,
    track_path: Option<String>,
    audio_path: Option<String>,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(ttml) = crate::lyrics::fetch_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
        )? {
            return Ok(Some(ttml));
        }

        let Some(track_path) = track_path.as_deref().map(str::trim).filter(|p| !p.is_empty())
        else {
            return Ok(None);
        };
        // Streams have no file to read tags from.
        if crate::cue::is_stream_url(track_path) {
            return Ok(None);
        }

        // A missing or unreadable file is not an error here — just no lyrics.
        let Ok(file_path) =
            crate::commands::library::tag_write_path(track_path, audio_path.as_deref())
        else {
            return Ok(None);
        };
        match crate::tag_table::read_embedded_lyrics(&file_path) {
            Ok(Some(text)) => crate::lyrics::normalize_lyrics_content(&text).map(Some),
            _ => Ok(None),
        }
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
    tauri::async_runtime::spawn_blocking(move || {
        let ttml = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read TTML file: {e}"))?;
        crate::lyrics::import_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
            &ttml,
        )
    })
    .await
    .map_err(|error| format!("Lyrics import task failed: {error}"))??;

    if let Err(e) = app.emit("lyrics:imported", track_path.unwrap_or_default()) {
        eprintln!("[lyrics_import_ttml] emit failed: {e}");
    }
    Ok(())
}

/// Save lyrics text (TTML or plain) into the cache for a track.
#[tauri::command]
pub async fn lyrics_save_text(
    app: AppHandle,
    title: String,
    artist: String,
    album: Option<String>,
    duration_secs: Option<u32>,
    content: String,
    track_path: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::lyrics::import_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
            &content,
        )
    })
    .await
    .map_err(|error| format!("Lyrics save task failed: {error}"))??;

    if let Err(e) = app.emit("lyrics:imported", track_path.unwrap_or_default()) {
        eprintln!("[lyrics_save_text] emit failed: {e}");
    }
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

    if let Err(e) = app.emit("lyrics:cleared", track_path.unwrap_or_default()) {
        eprintln!("[lyrics_clear] emit failed: {e}");
    }
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
    // Optional user-edited search terms from the Properties window. The cache key
    // still comes from title/artist/album above so results stay findable.
    search_title: Option<String>,
    search_artist: Option<String>,
    search_album: Option<String>,
) -> Result<bool, String> {
    let found = tauri::async_runtime::spawn_blocking(move || {
        crate::lyrics::refetch_lyrics_ttml(
            &title,
            &artist,
            album.as_deref(),
            duration_secs,
            search_title.as_deref(),
            search_artist.as_deref(),
            search_album.as_deref(),
        )
    })
    .await
    .map_err(|error| format!("Lyrics refetch task failed: {error}"))??
    .is_some();

    if let Err(e) = app.emit("lyrics:refetched", track_path.unwrap_or_default()) {
        eprintln!("[lyrics_refetch] emit failed: {e}");
    }
    Ok(found)
}
