// Discord Rich Presence — track, artist, playback time, and MusicBrainz cover art.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{
    Activity, ActivityType, Assets, StatusDisplayType, Timestamps,
};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use parking_lot::Mutex;

use crate::cue;
use crate::library::MusicFile;
use crate::metadata::{self, TrackMetadata};
use crate::imgbb;
use crate::musicbrainz;
use crate::player::{PlaybackState, PlayerStateSnapshot};

/// Discord Application ID — https://discord.com/developers/applications
pub const DISCORD_CLIENT_ID: &str = "1525094033666473995";

/// Last resolved tags for the active track — avoids re-parsing ID3 on every RPC sync.
static META_CACHE: Mutex<Option<(String, TrackMetadata)>> = Mutex::new(None);

#[derive(Clone)]
pub struct DiscordPresence {
    inner: Arc<Mutex<PresenceInner>>,
    lookup_generation: Arc<AtomicU64>,
}

struct PresenceInner {
    client: Option<DiscordIpcClient>,
    enabled: bool,
    connected: bool,
    last_track_path: Option<String>,
    last_cover_url: Option<String>,
    last_sync_key: Option<String>,
    last_connect_attempt: Option<Instant>,
    connect_backoff: Duration,
}

#[derive(Clone)]
struct ActivityPayload {
    title: String,
    artist: String,
    album: Option<String>,
    cover_url: Option<String>,
    position: f64,
    duration: f64,
    paused: bool,
}

impl DiscordPresence {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PresenceInner {
                client: None,
                enabled: true,
                connected: false,
                last_track_path: None,
                last_cover_url: None,
                last_sync_key: None,
                last_connect_attempt: None,
                connect_backoff: Duration::from_secs(1),
            })),
            lookup_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn configure(&self, enabled: bool) {
        let mut inner = self.inner.lock();
        let changed = inner.enabled != enabled;
        inner.enabled = enabled;

        if !enabled {
            let _ = Self::clear_activity(&mut inner);
            return;
        }

        if changed {
            Self::reset_connection(&mut inner);
            inner.last_track_path = None;
            inner.last_cover_url = None;
            inner.last_sync_key = None;
        }
    }

    pub fn update_from_player(&self, snapshot: &PlayerStateSnapshot) {
        let mut inner = self.inner.lock();
        if !inner.enabled {
            return;
        }

        let Some(track_path) = snapshot.current_file.as_ref() else {
            let _ = Self::clear_activity(&mut inner);
            inner.last_sync_key = None;
            return;
        };

        if snapshot.state == PlaybackState::Stopped && !snapshot.is_playing && !snapshot.is_paused {
            let _ = Self::clear_activity(&mut inner);
            inner.last_sync_key = None;
            return;
        }

        let track_meta = metadata_for_path(track_path);
        let title = track_meta
            .title
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| filename_title(track_path));
        let artist = track_meta
            .artist
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album = track_meta.album.filter(|value| !value.is_empty());

        let duration = if snapshot.duration > 0.0 {
            snapshot.duration
        } else {
            track_meta.duration_secs.unwrap_or(0.0)
        };

        let paused = snapshot.is_paused || snapshot.state == PlaybackState::Paused;
        let cover_url = inner
            .last_track_path
            .as_ref()
            .filter(|path| path.as_str() == track_path)
            .and_then(|_| inner.last_cover_url.clone());

        let payload = ActivityPayload {
            title: title.clone(),
            artist: artist.clone(),
            album: album.clone(),
            cover_url: cover_url.clone(),
            position: snapshot.position,
            duration,
            paused,
        };
        let sync_key = Self::sync_key(track_path, &payload);
        if inner.last_sync_key.as_deref() == Some(sync_key.as_str()) {
            return;
        }

        match Self::push_activity(&mut inner, &payload) {
            Ok(()) => {
                inner.last_sync_key = Some(sync_key);
                inner.connect_backoff = Duration::from_secs(1);
            }
            Err(error) => {
                eprintln!("Discord RPC update failed: {error}");
            }
        }

        if inner.last_track_path.as_deref() != Some(track_path) {
            inner.last_track_path = Some(track_path.clone());
            inner.last_cover_url = None;
            self.spawn_cover_lookup(track_path.clone(), artist, title, album, payload);
        }
    }

    pub fn shutdown(&self) {
        let mut inner = self.inner.lock();
        let _ = Self::clear_activity(&mut inner);
        Self::reset_connection(&mut inner);
        inner.last_sync_key = None;
    }

    fn spawn_cover_lookup(
        &self,
        track_path: String,
        artist: String,
        title: String,
        album: Option<String>,
        base_payload: ActivityPayload,
    ) {
        let generation = self.lookup_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let inner = self.inner.clone();
        thread::spawn(move || {
            let cover_url = resolve_cover_url(&track_path, &artist, &title, album.as_deref());
            let Some(cover_url) = cover_url else {
                return;
            };
            let mut guard = inner.lock();
            if !guard.enabled {
                return;
            }
            if guard.last_track_path.as_deref() != Some(track_path.as_str()) {
                return;
            }

            guard.last_cover_url = Some(cover_url.clone());
            let payload = ActivityPayload {
                cover_url: Some(cover_url),
                ..base_payload
            };
            let sync_key = Self::sync_key(&track_path, &payload);
            if guard.last_sync_key.as_deref() == Some(sync_key.as_str()) {
                return;
            }

            match Self::push_activity(&mut guard, &payload) {
                Ok(()) => guard.last_sync_key = Some(sync_key),
                Err(error) => eprintln!("Discord RPC cover update failed: {error}"),
            }

            let _ = generation;
        });
    }

    fn sync_key(track_path: &str, payload: &ActivityPayload) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            track_path,
            payload.title,
            payload.artist,
            payload.album.as_deref().unwrap_or(""),
            payload.cover_url.as_deref().unwrap_or(""),
            payload.paused,
            payload.position.round() as i64,
            payload.duration.round() as i64,
        )
    }

    fn reset_connection(inner: &mut PresenceInner) {
        if let Some(mut client) = inner.client.take() {
            let _ = client.close();
        }
        inner.connected = false;
    }

    fn ensure_connected(inner: &mut PresenceInner) -> bool {
        if inner.connected {
            return true;
        }

        if let Some(last_attempt) = inner.last_connect_attempt {
            if last_attempt.elapsed() < inner.connect_backoff {
                return false;
            }
        }

        inner.last_connect_attempt = Some(Instant::now());

        let mut client = DiscordIpcClient::new(DISCORD_CLIENT_ID);
        if let Err(error) = client.connect() {
            inner.connect_backoff = (inner.connect_backoff * 2).min(Duration::from_secs(30));
            eprintln!("Discord RPC connect failed: {error}");
            return false;
        }

        inner.client = Some(client);
        inner.connected = true;
        inner.connect_backoff = Duration::from_secs(1);
        true
    }

    fn push_activity(inner: &mut PresenceInner, payload: &ActivityPayload) -> Result<(), String> {
        if !Self::ensure_connected(inner) {
            return Err("Discord is not running or RPC is unavailable".into());
        }

        match Self::set_activity(inner, payload) {
            Ok(()) => Ok(()),
            Err(error) => {
                if !Self::is_ipc_error(&error) {
                    return Err(error);
                }

                Self::reset_connection(inner);
                if !Self::ensure_connected(inner) {
                    return Err(error);
                }

                Self::set_activity(inner, payload)
            }
        }
    }

    fn is_ipc_error(error: &str) -> bool {
        error.contains("IPC socket")
            || error.contains("failed to write")
            || error.contains("failed to read")
            || error.contains("connection")
    }

    fn clear_activity(inner: &mut PresenceInner) -> Result<(), String> {
        inner.last_track_path = None;
        inner.last_cover_url = None;
        let Some(client) = inner.client.as_mut() else {
            return Ok(());
        };

        match client.clear_activity() {
            Ok(()) => Ok(()),
            Err(error) => {
                let message = error.to_string();
                if Self::is_ipc_error(&message) {
                    Self::reset_connection(inner);
                }
                Err(message)
            }
        }
    }

    fn set_activity(inner: &mut PresenceInner, payload: &ActivityPayload) -> Result<(), String> {
        let client = inner
            .client
            .as_mut()
            .ok_or_else(|| "Discord RPC is not connected".to_string())?;

        let mut activity = Activity::new()
            .activity_type(ActivityType::Listening)
            .status_display_type(StatusDisplayType::Details)
            .details(truncate(&payload.title, 128))
            .state(truncate(&payload.artist, 128));

        if !payload.paused && payload.duration > 0.0 {
            let now = unix_now();
            let start = now - payload.position.round() as i64;
            let end = start + payload.duration.round() as i64;
            activity = activity.timestamps(Timestamps::new().start(start).end(end));
        }

        if let Some(cover_url) = payload.cover_url.as_deref() {
            activity = activity.assets(Assets::new().large_image(cover_url));
        }

        client
            .set_activity(activity)
            .map_err(|error| error.to_string())
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
}

fn resolve_cover_url(
    track_path: &str,
    artist: &str,
    title: &str,
    album: Option<&str>,
) -> Option<String> {
    if let Some(url) = musicbrainz::lookup_cover_url(artist, title, album) {
        return Some(url);
    }

    local_cover_path(track_path).and_then(|path| imgbb::upload_image(&path))
}

fn local_cover_from_audio(track_path: &str, track: &MusicFile) -> Option<String> {
    if let Some(audio_path) = track.audio_path.as_deref() {
        let audio = Path::new(audio_path);
        if audio.is_file() {
            return metadata::read_metadata(audio, "").cover_path;
        }
    }

    let path = Path::new(track_path);
    if path.is_file() {
        return metadata::read_metadata(path, &track.file_name).cover_path;
    }

    None
}

fn local_cover_path(track_path: &str) -> Option<std::path::PathBuf> {
    let cover_path = metadata_for_path(track_path).cover_path?;
    let path = std::path::PathBuf::from(cover_path);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

fn filename_title(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(metadata::strip_ytdlp_id_suffix)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unknown Track".to_string())
}

fn metadata_for_path(track_path: &str) -> TrackMetadata {
    // Discord updates can fire on play/pause/seek/gapless; re-reading ID3 + decoding
    // embedded art each time made some tracks (large APIC) stutter for free.
    {
        let cache = META_CACHE.lock();
        if let Some((cached_path, meta)) = cache.as_ref() {
            if cached_path == track_path {
                return meta.clone();
            }
        }
    }

    let meta = metadata_for_path_uncached(track_path);
    *META_CACHE.lock() = Some((track_path.to_string(), meta.clone()));
    meta
}

fn metadata_for_path_uncached(track_path: &str) -> TrackMetadata {
    let path = Path::new(track_path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut track = MusicFile {
        path: track_path.to_string(),
        file_name,
        extension,
        size: 0,
        title: None,
        artist: None,
        album: None,
        duration_secs: None,
        year: None,
        track_number: None,
        genre: None,
        cover_path: None,
        cover_path_full: None,
        audio_path: None,
        cue_start_secs: None,
        cue_end_secs: None,
    };
    cue::repair_track(&mut track);

    if track.title.is_some() || track.artist.is_some() || track.album.is_some() {
        let mut cover_path = track.cover_path.clone();
        if cover_path.is_none() {
            cover_path = local_cover_from_audio(track_path, &track);
        }
        return TrackMetadata {
            title: track.title,
            artist: track.artist,
            album: track.album,
            duration_secs: track.duration_secs,
            year: track.year,
            track_number: track.track_number,
            genre: track.genre,
            cover_path,
            cover_path_full: track.cover_path_full.clone(),
        };
    }

    if path.is_file() {
        return metadata::read_metadata(path, &track.file_name);
    }

    if let Some(audio_path) = track.audio_path.as_deref() {
        let audio = Path::new(audio_path);
        if audio.is_file() {
            let mut meta = metadata::read_metadata(audio, "");
            if meta.title.is_none() {
                meta.title = track.title;
            }
            if meta.artist.is_none() {
                meta.artist = track.artist;
            }
            if meta.album.is_none() {
                meta.album = track.album;
            }
            if meta.duration_secs.is_none() {
                meta.duration_secs = track.duration_secs;
            }
            return meta;
        }
    }

    TrackMetadata {
        title: track.title,
        artist: track.artist,
        album: track.album,
        duration_secs: track.duration_secs,
        ..TrackMetadata::default()
    }
}