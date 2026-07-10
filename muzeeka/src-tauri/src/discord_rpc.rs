// Discord Rich Presence — track, artist, playback time, and MusicBrainz cover art.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use parking_lot::Mutex;

use crate::cue;
use crate::library::MusicFile;
use crate::metadata::{self, TrackMetadata};
use crate::musicbrainz;
use crate::player::{PlaybackState, PlayerStateSnapshot};

/// Discord Application ID — https://discord.com/developers/applications
pub const DISCORD_CLIENT_ID: &str = "1525094033666473995";

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
            inner.connected = false;
            inner.client = None;
            inner.last_track_path = None;
            inner.last_cover_url = None;
        }
    }

    pub fn update_from_player(&self, snapshot: &PlayerStateSnapshot) {
        let mut inner = self.inner.lock();
        if !inner.enabled {
            return;
        }

        let Some(track_path) = snapshot.current_file.as_ref() else {
            let _ = Self::clear_activity(&mut inner);
            return;
        };

        if snapshot.state == PlaybackState::Stopped && !snapshot.is_playing && !snapshot.is_paused {
            let _ = Self::clear_activity(&mut inner);
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

        if !Self::ensure_connected(&mut inner) {
            return;
        }

        if let Err(error) = Self::set_activity(
            &mut inner,
            &title,
            &artist,
            album.as_deref(),
            cover_url.as_deref(),
            snapshot.position,
            duration,
            paused,
        ) {
            eprintln!("Discord RPC update failed: {error}");
            inner.connected = false;
            inner.client = None;
        }

        if inner.last_track_path.as_deref() != Some(track_path) {
            inner.last_track_path = Some(track_path.clone());
            inner.last_cover_url = None;
            self.spawn_cover_lookup(
                track_path.clone(),
                artist,
                title,
                album,
                snapshot.position,
                duration,
                paused,
            );
        }
    }

    pub fn shutdown(&self) {
        let mut inner = self.inner.lock();
        let _ = Self::clear_activity(&mut inner);
        if let Some(client) = inner.client.as_mut() {
            let _ = client.close();
        }
        inner.client = None;
        inner.connected = false;
    }

    fn spawn_cover_lookup(
        &self,
        track_path: String,
        artist: String,
        title: String,
        album: Option<String>,
        position: f64,
        duration: f64,
        paused: bool,
    ) {
        let generation = self.lookup_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let inner = self.inner.clone();
        thread::spawn(move || {
            let cover_url = musicbrainz::lookup_cover_url(&artist, &title, album.as_deref());
            if cover_url.is_none() {
                return;
            }

            let cover_url = cover_url.unwrap();
            let mut guard = inner.lock();
            if !guard.enabled || !guard.connected {
                return;
            }
            if guard.last_track_path.as_deref() != Some(track_path.as_str()) {
                return;
            }

            guard.last_cover_url = Some(cover_url.clone());
            if let Err(error) = Self::set_activity(
                &mut guard,
                &title,
                &artist,
                album.as_deref(),
                Some(&cover_url),
                position,
                duration,
                paused,
            ) {
                eprintln!("Discord RPC cover update failed: {error}");
            }

            let _ = generation;
        });
    }

    fn ensure_connected(inner: &mut PresenceInner) -> bool {
        if inner.connected {
            return true;
        }

        let mut client = DiscordIpcClient::new(DISCORD_CLIENT_ID);

        if let Err(error) = client.connect() {
            eprintln!("Discord RPC connect failed: {error}");
            return false;
        }

        inner.client = Some(client);
        inner.connected = true;
        true
    }

    fn clear_activity(inner: &mut PresenceInner) -> Result<(), String> {
        inner.last_track_path = None;
        inner.last_cover_url = None;
        if let Some(client) = inner.client.as_mut() {
            client
                .clear_activity()
                .map_err(|error| error.to_string())
        } else {
            Ok(())
        }
    }

    fn set_activity(
        inner: &mut PresenceInner,
        title: &str,
        artist: &str,
        album: Option<&str>,
        cover_url: Option<&str>,
        position: f64,
        duration: f64,
        paused: bool,
    ) -> Result<(), String> {
        let client = inner
            .client
            .as_mut()
            .ok_or_else(|| "Discord RPC is not connected".to_string())?;

        let mut activity = Activity::new()
            .activity_type(ActivityType::Listening)
            .details(truncate(title, 128))
            .state(if paused {
                format!("⏸ {}", truncate(artist, 120))
            } else {
                truncate(artist, 128)
            });

        if !paused && duration > 0.0 {
            let now = unix_now();
            let start = now - position.round() as i64;
            let end = start + duration.round() as i64;
            activity = activity.timestamps(Timestamps::new().start(start).end(end));
        }

        if let Some(cover_url) = cover_url {
            let mut assets = Assets::new().large_image(cover_url);
            if let Some(album) = album {
                assets = assets.large_text(truncate(album, 128));
            }
            activity = activity.assets(assets);
        } else if let Some(album) = album {
            activity = activity.assets(Assets::new().large_text(truncate(album, 128)));
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

fn filename_title(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(metadata::strip_ytdlp_id_suffix)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unknown Track".to_string())
}

fn metadata_for_path(track_path: &str) -> TrackMetadata {
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
        audio_path: None,
        cue_start_secs: None,
        cue_end_secs: None,
    };
    cue::repair_track(&mut track);

    if track.title.is_some() || track.artist.is_some() || track.album.is_some() {
        return TrackMetadata {
            title: track.title,
            artist: track.artist,
            album: track.album,
            duration_secs: track.duration_secs,
            year: track.year,
            track_number: track.track_number,
            genre: track.genre,
            cover_path: track.cover_path,
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