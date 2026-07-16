// Remote playback controller — mirrors frontend queue logic for the HTTP API.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::discord_rpc::DiscordPresence;
use crate::library::MusicFile;
use crate::metadata;
use crate::player::{GaplessTrack, Player, PositionPayload, TrackChangedPayload};
use crate::playlists::{self, PlaylistsData};

pub const VIRTUAL_ALL_ID: &str = "__all__";
pub const VIRTUAL_LIKED_ID: &str = "__liked__";
const MAX_GAPLESS_FOLLOWING: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    fn from_str(value: Option<&str>) -> Self {
        match value {
            Some("all") => Self::All,
            Some("one") => Self::One,
            _ => Self::Off,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTrackInfo {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteState {
    pub is_playing: bool,
    pub is_paused: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub shuffle_enabled: bool,
    pub repeat_mode: String,
    pub track: Option<RemoteTrackInfo>,
    pub active_playlist_id: Option<String>,
    pub active_playlist_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemotePlaylistSummary {
    pub id: String,
    pub name: String,
    pub track_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaylistView {
    pub id: String,
    pub name: String,
    pub tracks: Vec<RemoteTrackInfo>,
}

#[derive(Debug, Clone, Serialize)]
struct PlayerStateEvent {
    is_playing: bool,
    is_paused: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreSyncPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    active_playlist_id: Option<String>,
    playing_playlist_id: Option<String>,
    shuffle_enabled: bool,
    repeat_mode: Option<String>,
    volume: Option<f32>,
    current_file: Option<String>,
    is_playing: bool,
    is_paused: bool,
    position: f64,
    duration: f64,
}

struct PlaylistsCache {
    data: PlaylistsData,
    modified: Option<SystemTime>,
}

pub struct RemoteController {
    player: Player,
    discord: DiscordPresence,
    app: AppHandle,
    shuffle_order: Mutex<Vec<usize>>,
    shuffle_position: Mutex<usize>,
    playlists_cache: Mutex<Option<PlaylistsCache>>,
}

impl RemoteController {
    pub fn new(player: Player, discord: DiscordPresence, app: AppHandle) -> Self {
        Self {
            player,
            discord,
            app,
            shuffle_order: Mutex::new(Vec::new()),
            shuffle_position: Mutex::new(0),
            playlists_cache: Mutex::new(None),
        }
    }

    fn playing_id_from_data(data: &PlaylistsData) -> Option<String> {
        data.playing_playlist_id
            .clone()
            .or_else(|| data.active_playlist_id.clone())
    }

    fn notify_playback(&self) {
        let snapshot = self.player.get_state();
        let _ = self.app.emit(
            "player:state",
            PlayerStateEvent {
                is_playing: snapshot.is_playing,
                is_paused: snapshot.is_paused,
            },
        );
        let _ = self.app.emit(
            "player:position",
            PositionPayload {
                position: snapshot.position,
                duration: snapshot.duration,
                state: snapshot.state,
            },
        );
    }

    fn notify_store_sync(&self, data: &PlaylistsData) {
        let snapshot = self.player.get_state();
        let _ = self.app.emit(
            "player:store-sync",
            StoreSyncPayload {
                active_playlist_id: None,
                playing_playlist_id: data.playing_playlist_id.clone(),
                shuffle_enabled: data.shuffle_enabled,
                repeat_mode: data.repeat_mode.clone(),
                volume: data.volume,
                current_file: data.current_file.clone(),
                is_playing: snapshot.is_playing,
                is_paused: snapshot.is_paused,
                position: snapshot.position,
                duration: snapshot.duration,
            },
        );
    }

    fn notify_track_changed(&self, path: &str) {
        let _ = self.app.emit(
            "player:track-changed",
            TrackChangedPayload {
                path: path.to_string(),
            },
        );
    }

    fn load_data(&self) -> Result<PlaylistsData, String> {
        let path = playlists::playlists_path(&self.app)?;
        let modified = fs::metadata(&path).ok().and_then(|meta| meta.modified().ok());

        if let Some(cached) = self.playlists_cache.lock().as_ref() {
            if cached.modified == modified {
                return Ok(cached.data.clone());
            }
        }

        let data = playlists::load_playlists_fast(&self.app)?;
        *self.playlists_cache.lock() = Some(PlaylistsCache {
            data: data.clone(),
            modified,
        });
        Ok(data)
    }

    fn save_data(&self, data: &PlaylistsData) -> Result<(), String> {
        playlists::save_playlists(&self.app, data)?;
        let modified = playlists::playlists_path(&self.app)
            .ok()
            .and_then(|path| fs::metadata(&path).ok().and_then(|meta| meta.modified().ok()));
        *self.playlists_cache.lock() = Some(PlaylistsCache {
            data: data.clone(),
            modified,
        });
        Ok(())
    }

    fn sync_discord(&self) {
        let player = self.player.clone();
        let discord = self.discord.clone();
        std::thread::spawn(move || {
            discord.update_from_player(&player.get_state());
        });
    }

    fn track_map(data: &PlaylistsData) -> std::collections::HashMap<String, MusicFile> {
        let mut map = std::collections::HashMap::new();
        for playlist in &data.playlists {
            for track in &playlist.tracks {
                map.entry(track.path.clone()).or_insert_with(|| track.clone());
            }
        }
        map
    }

    fn default_all_paths(data: &PlaylistsData) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for playlist in &data.playlists {
            for track in &playlist.tracks {
                if seen.insert(track.path.clone()) {
                    result.push(track.path.clone());
                }
            }
        }
        result
    }

    fn all_tracks(data: &PlaylistsData) -> Vec<MusicFile> {
        let track_map = Self::track_map(data);
        let default_order = Self::default_all_paths(data);

        if data.all_paths.is_empty() {
            return default_order
                .into_iter()
                .filter_map(|path| track_map.get(&path).cloned())
                .collect();
        }

        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for path in &data.all_paths {
            if let Some(track) = track_map.get(path) {
                result.push(track.clone());
                seen.insert(path.clone());
            }
        }

        for path in default_order {
            if !seen.contains(&path) {
                if let Some(track) = track_map.get(&path) {
                    result.push(track.clone());
                }
            }
        }

        result
    }

    fn liked_tracks(data: &PlaylistsData) -> Vec<MusicFile> {
        let track_map = Self::track_map(data);
        data.liked_paths
            .iter()
            .filter_map(|path| track_map.get(path).cloned())
            .collect()
    }

    fn playing_tracks(data: &PlaylistsData, playing_id: Option<&str>) -> Vec<MusicFile> {
        match playing_id {
            Some(VIRTUAL_ALL_ID) => Self::all_tracks(data),
            Some(VIRTUAL_LIKED_ID) => Self::liked_tracks(data),
            Some(id) => data
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.tracks.clone())
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    fn playlist_name(data: &PlaylistsData, id: Option<&str>) -> Option<String> {
        match id {
            Some(VIRTUAL_ALL_ID) => Some("All tracks".to_string()),
            Some(VIRTUAL_LIKED_ID) => Some("Liked".to_string()),
            Some(id) => data
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone()),
            None => None,
        }
    }

    fn find_playlist_for_track(data: &PlaylistsData, path: &str, playing_id: Option<&str>) -> Option<String> {
        if let Some(id) = playing_id {
            let tracks = Self::playing_tracks(data, Some(id));
            if tracks.iter().any(|t| t.path == path) {
                return Some(id.to_string());
            }
        }
        for playlist in &data.playlists {
            if playlist.tracks.iter().any(|t| t.path == path) {
                return Some(playlist.id.clone());
            }
        }
        None
    }

    fn track_display_title(track: &MusicFile) -> String {
        track
            .title
            .as_ref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .unwrap_or_else(|| {
                track
                    .file_name
                    .rsplit_once('.')
                    .map(|(stem, _)| stem.to_string())
                    .unwrap_or_else(|| track.file_name.clone())
            })
    }

    fn track_display_artist(track: &MusicFile) -> String {
        track
            .artist
            .as_ref()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(|a| a.to_string())
            .unwrap_or_else(|| "Unknown Artist".to_string())
    }

    fn cover_url_for_track(track: &MusicFile) -> Option<String> {
        let cover_path = track
            .cover_path
            .as_deref()
            .or(track.cover_path_full.as_deref())?;
        Some(format!(
            "/api/cover?path={}",
            urlencoding::encode(cover_path)
        ))
    }

    fn to_remote_track(track: &MusicFile) -> RemoteTrackInfo {
        RemoteTrackInfo {
            path: track.path.clone(),
            title: Self::track_display_title(track),
            artist: Self::track_display_artist(track),
            album: track.album.clone(),
            duration_secs: track.duration_secs,
            cover_url: Self::cover_url_for_track(track),
        }
    }

    fn audio_path_for_track(track: &MusicFile, file_path: &str) -> String {
        if let Some(audio_path) = &track.audio_path {
            return audio_path.clone();
        }
        const CUE_MARKER: &str = "#cue:";
        if let Some(pos) = file_path.rfind(CUE_MARKER) {
            if pos > 0 {
                return file_path[..pos].to_string();
            }
        }
        file_path.to_string()
    }

    fn gapless_track(track: &MusicFile, file_path: &str) -> GaplessTrack {
        GaplessTrack {
            track_path: file_path.to_string(),
            audio_path: Self::audio_path_for_track(track, file_path),
            cue_start: track.cue_start_secs,
            cue_end: track.cue_end_secs,
        }
    }

    fn repeat_mode(data: &PlaylistsData) -> RepeatMode {
        RepeatMode::from_str(data.repeat_mode.as_deref())
    }

    fn shuffle_indices(count: usize) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..count).collect();
        for i in (1..count).rev() {
            let j = (rand_simple() as usize) % (i + 1);
            indices.swap(i, j);
        }
        indices
    }

    fn rebuild_shuffle_order(&self, data: &PlaylistsData, keep_current: bool) {
        let playing_id = Self::playing_id_from_data(data);
        let tracks = Self::playing_tracks(data, playing_id.as_deref());
        if tracks.is_empty() {
            *self.shuffle_order.lock() = Vec::new();
            *self.shuffle_position.lock() = 0;
            return;
        }

        let mut indices = Self::shuffle_indices(tracks.len());
        if keep_current {
            let current = self.player.get_state().current_file;
            if let Some(path) = current {
                if let Some(current_idx) = tracks.iter().position(|t| t.path == path) {
                    if let Some(at) = indices.iter().position(|&i| i == current_idx) {
                        if at > 0 {
                            indices.remove(at);
                            indices.insert(0, current_idx);
                        }
                    }
                }
            }
        }

        *self.shuffle_order.lock() = indices;
        *self.shuffle_position.lock() = 0;
    }

    fn sync_shuffle_position(&self, data: &PlaylistsData) {
        let playing_id = Self::playing_id_from_data(data);
        let tracks = Self::playing_tracks(data, playing_id.as_deref());
        let current = self.player.get_state().current_file;
        let Some(path) = current else { return };
        let Some(current_idx) = tracks.iter().position(|t| t.path == path) else {
            return;
        };
        let order = self.shuffle_order.lock();
        if let Some(pos) = order.iter().position(|&i| i == current_idx) {
            *self.shuffle_position.lock() = pos;
        }
    }

    fn ensure_shuffle_order(&self, data: &PlaylistsData) {
        if !data.shuffle_enabled {
            return;
        }
        let playing_id = Self::playing_id_from_data(data);
        let tracks = Self::playing_tracks(data, playing_id.as_deref());
        let order = self.shuffle_order.lock();
        if order.len() != tracks.len() || order.iter().any(|&i| i >= tracks.len()) {
            drop(order);
            self.rebuild_shuffle_order(data, true);
            self.sync_shuffle_position(data);
        }
    }

    fn ordered_tracks_from(&self, data: &PlaylistsData, file_path: &str) -> Vec<MusicFile> {
        let playing_id = Self::playing_id_from_data(data);
        let tracks = Self::playing_tracks(data, playing_id.as_deref());
        if tracks.is_empty() {
            return Vec::new();
        }

        if data.shuffle_enabled {
            self.ensure_shuffle_order(data);
            let order = self.shuffle_order.lock();
            let track_idx = tracks.iter().position(|t| t.path == file_path);
            let Some(track_idx) = track_idx else {
                return Vec::new();
            };
            let order_pos = order.iter().position(|&i| i == track_idx);
            let Some(order_pos) = order_pos else {
                return Vec::new();
            };
            return order[order_pos..order_pos + MAX_GAPLESS_FOLLOWING]
                .iter()
                .filter_map(|&index| tracks.get(index).cloned())
                .collect();
        }

        let index = tracks.iter().position(|t| t.path == file_path);
        let Some(index) = index else {
            return Vec::new();
        };
        tracks[index..index + MAX_GAPLESS_FOLLOWING].to_vec()
    }

    fn build_gapless_queue(&self, data: &PlaylistsData, file_path: &str) -> Vec<GaplessTrack> {
        let repeat = Self::repeat_mode(data);
        if repeat == RepeatMode::One {
            let track_map = Self::track_map(data);
            if let Some(track) = track_map.get(file_path) {
                return vec![Self::gapless_track(track, file_path)];
            }
            return Vec::new();
        }

        self.ordered_tracks_from(data, file_path)
            .iter()
            .map(|track| Self::gapless_track(track, &track.path))
            .collect()
    }

    fn play_track(&self, file_path: &str, playlist_id: Option<&str>) -> Result<(), String> {
        let mut data = self.load_data()?;
        let track_map = Self::track_map(&data);
        let track = track_map.get(file_path);

        let mut playing_id = playlist_id
            .map(|id| id.to_string())
            .or_else(|| data.playing_playlist_id.clone())
            .or_else(|| data.active_playlist_id.clone());

        if let Some(ref id) = playing_id {
            if id != VIRTUAL_ALL_ID && id != VIRTUAL_LIKED_ID && !data.playlists.iter().any(|p| p.id == *id) {
                playing_id = Self::find_playlist_for_track(&data, file_path, None);
            }
        } else {
            playing_id = Self::find_playlist_for_track(&data, file_path, None);
        }

        if let Some(id) = playing_id.clone() {
            data.playing_playlist_id = Some(id);
        }

        let repeat = Self::repeat_mode(&data);
        let queue = if repeat == RepeatMode::One {
            let fallback = MusicFile {
                path: file_path.to_string(),
                file_name: file_path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(file_path)
                    .to_string(),
                extension: String::new(),
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
            let resolved = track.unwrap_or(&fallback);
            vec![Self::gapless_track(resolved, file_path)]
        } else if let Some(ref id) = playing_id {
            if let Some(playlist) = data.playlists.iter().find(|p| p.id == *id) {
                let idx = playlist.tracks.iter().position(|t| t.path == file_path);
                let slice: Vec<&MusicFile> = if let Some(idx) = idx {
                    playlist.tracks[idx..idx + MAX_GAPLESS_FOLLOWING].iter().collect()
                } else if let Some(t) = track {
                    vec![t]
                } else {
                    vec![]
                };
                slice
                    .into_iter()
                    .map(|t| Self::gapless_track(t, &t.path))
                    .collect()
            } else {
                self.build_gapless_queue(&data, file_path)
            }
        } else {
            self.build_gapless_queue(&data, file_path)
        };

        let resolved = track.cloned().unwrap_or_else(|| MusicFile {
            path: file_path.to_string(),
            file_name: file_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(file_path)
                .to_string(),
            extension: String::new(),
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
        });

        self.player.play(
            file_path,
            Some(&Self::audio_path_for_track(&resolved, file_path)),
            resolved.cue_start_secs,
            resolved.cue_end_secs,
            queue,
        )?;

        data.current_file = Some(file_path.to_string());
        self.save_data(&data)?;
        self.sync_discord();
        self.notify_track_changed(file_path);
        self.notify_store_sync(&data);
        Ok(())
    }

    pub fn get_state(&self) -> Result<RemoteState, String> {
        let data = self.load_data()?;
        let snapshot = self.player.get_state();
        let active_id = Self::playing_id_from_data(&data).or_else(|| {
            snapshot.current_file.as_ref().and_then(|path| {
                Self::find_playlist_for_track(&data, path, data.playing_playlist_id.as_deref())
            })
        });

        let track = snapshot.current_file.as_ref().and_then(|path| {
            Self::track_map(&data)
                .get(path)
                .map(|t| Self::to_remote_track(t))
        });

        Ok(RemoteState {
            is_playing: snapshot.is_playing,
            is_paused: snapshot.is_paused,
            position: snapshot.position,
            duration: snapshot.duration,
            volume: snapshot.volume,
            shuffle_enabled: data.shuffle_enabled,
            repeat_mode: Self::repeat_mode(&data).as_str().to_string(),
            track,
            active_playlist_id: active_id.clone(),
            active_playlist_name: Self::playlist_name(&data, active_id.as_deref()),
        })
    }

    pub fn get_playlists(&self) -> Result<Vec<RemotePlaylistSummary>, String> {
        let data = self.load_data()?;
        let mut result = vec![
            RemotePlaylistSummary {
                id: VIRTUAL_ALL_ID.to_string(),
                name: "All tracks".to_string(),
                track_count: Self::all_tracks(&data).len(),
            },
            RemotePlaylistSummary {
                id: VIRTUAL_LIKED_ID.to_string(),
                name: "Liked".to_string(),
                track_count: Self::liked_tracks(&data).len(),
            },
        ];

        for playlist in &data.playlists {
            result.push(RemotePlaylistSummary {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
                track_count: playlist.tracks.len(),
            });
        }

        Ok(result)
    }

    pub fn get_playlist_view(&self, playlist_id: &str) -> Result<RemotePlaylistView, String> {
        let data = self.load_data()?;
        let tracks = Self::playing_tracks(&data, Some(playlist_id));
        let name = Self::playlist_name(&data, Some(playlist_id))
            .unwrap_or_else(|| "Playlist".to_string());

        Ok(RemotePlaylistView {
            id: playlist_id.to_string(),
            name,
            tracks: tracks.iter().map(Self::to_remote_track).collect(),
        })
    }

    pub fn play(&self, path: &str, playlist_id: Option<&str>) -> Result<(), String> {
        self.play_track(path, playlist_id)
    }

    pub fn pause(&self) -> Result<(), String> {
        self.player.pause()?;
        self.sync_discord();
        self.notify_playback();
        Ok(())
    }

    pub fn resume(&self) -> Result<(), String> {
        self.player.resume()?;
        self.sync_discord();
        self.notify_playback();
        Ok(())
    }

    pub fn toggle(&self) -> Result<(), String> {
        let state = self.player.get_state();
        if state.is_playing {
            self.pause()
        } else if state.is_paused {
            self.resume()
        } else if let Some(path) = state.current_file {
            self.play_track(&path, None)
        } else {
            let data = self.load_data()?;
            let playing_id = Self::playing_id_from_data(&data);
            let tracks = Self::playing_tracks(&data, playing_id.as_deref());
            if let Some(track) = tracks.first() {
                self.play_track(&track.path, playing_id.as_deref())
            } else {
                Ok(())
            }
        }
    }

    pub fn next(&self) -> Result<(), String> {
        let data = self.load_data()?;
        let playing_id = Self::playing_id_from_data(&data);
        let tracks = Self::playing_tracks(&data, playing_id.as_deref());
        let current = self.player.get_state().current_file;
        let repeat = Self::repeat_mode(&data);

        if data.shuffle_enabled {
            self.ensure_shuffle_order(&data);
            let mut pos = *self.shuffle_position.lock();
            let order = self.shuffle_order.lock();
            if pos < order.len().saturating_sub(1) {
                pos += 1;
            } else if repeat == RepeatMode::All {
                pos = 0;
            } else {
                return Ok(());
            }
            *self.shuffle_position.lock() = pos;
            if let Some(&idx) = order.get(pos) {
                if let Some(track) = tracks.get(idx) {
                    return self.play_track(&track.path, playing_id.as_deref());
                }
            }
            return Ok(());
        }

        let current_path = current.as_deref().unwrap_or("");
        let idx = tracks.iter().position(|t| t.path == current_path);
        if let Some(idx) = idx {
            if idx + 1 < tracks.len() {
                return self.play_track(&tracks[idx + 1].path, playing_id.as_deref());
            }
        }
        if repeat == RepeatMode::All {
            if let Some(track) = tracks.first() {
                return self.play_track(&track.path, playing_id.as_deref());
            }
        }
        Ok(())
    }

    pub fn prev(&self) -> Result<(), String> {
        let state = self.player.get_state();
        if state.position > 3.0 {
            if state.current_file.is_some() {
                self.player.seek(0.0)?;
                self.sync_discord();
                self.notify_playback();
                return Ok(());
            }
        }

        let data = self.load_data()?;
        let playing_id = Self::playing_id_from_data(&data);
        let tracks = Self::playing_tracks(&data, playing_id.as_deref());

        if data.shuffle_enabled {
            self.ensure_shuffle_order(&data);
            let mut pos = *self.shuffle_position.lock();
            if pos > 0 {
                pos -= 1;
                *self.shuffle_position.lock() = pos;
                let order = self.shuffle_order.lock();
                if let Some(&idx) = order.get(pos) {
                    if let Some(track) = tracks.get(idx) {
                        return self.play_track(&track.path, playing_id.as_deref());
                    }
                }
            }
            return Ok(());
        }

        let current_path = state.current_file.as_deref().unwrap_or("");
        let idx = tracks.iter().position(|t| t.path == current_path);
        if let Some(idx) = idx {
            if idx > 0 {
                return self.play_track(&tracks[idx - 1].path, playing_id.as_deref());
            }
        }
        Ok(())
    }

    pub fn seek(&self, position: f64) -> Result<(), String> {
        self.player.seek(position)?;
        self.sync_discord();
        self.notify_playback();
        Ok(())
    }

    pub fn set_volume(&self, volume: f32) -> Result<(), String> {
        let volume = volume.clamp(0.0, 1.0);
        self.player.set_volume(volume)?;
        let mut data = self.load_data()?;
        data.volume = Some(volume);
        self.save_data(&data)?;
        self.notify_store_sync(&data);
        Ok(())
    }

    pub fn select_playlist(&self, playlist_id: &str) -> Result<(), String> {
        let mut data = self.load_data()?;
        if playlist_id != VIRTUAL_ALL_ID
            && playlist_id != VIRTUAL_LIKED_ID
            && !data.playlists.iter().any(|p| p.id == playlist_id)
        {
            return Err("Playlist not found".to_string());
        }
        // Remote queue only — do not change the desktop app's viewed playlist.
        data.playing_playlist_id = Some(playlist_id.to_string());
        self.save_data(&data)?;
        self.notify_store_sync(&data);
        Ok(())
    }

    pub fn toggle_shuffle(&self) -> Result<bool, String> {
        let mut data = self.load_data()?;
        data.shuffle_enabled = !data.shuffle_enabled;
        if data.shuffle_enabled {
            self.rebuild_shuffle_order(&data, true);
            self.sync_shuffle_position(&data);
        } else {
            *self.shuffle_order.lock() = Vec::new();
            *self.shuffle_position.lock() = 0;
        }
        let enabled = data.shuffle_enabled;
        self.save_data(&data)?;
        self.notify_store_sync(&data);
        Ok(enabled)
    }

    pub fn toggle_repeat(&self) -> Result<String, String> {
        let mut data = self.load_data()?;
        let next = Self::repeat_mode(&data).cycle();
        data.repeat_mode = Some(next.as_str().to_string());

        if let Some(path) = self.player.get_state().current_file {
            let queue = self.build_gapless_queue(&data, &path);
            let _ = self.player.prepare_next(queue);
        }

        let mode = next.as_str().to_string();
        self.save_data(&data)?;
        self.notify_store_sync(&data);
        Ok(mode)
    }

    pub fn cover_bytes(&self, path: &str) -> Result<Option<(Vec<u8>, String)>, String> {
        let path = Path::new(path);
        if !path.is_file() {
            return Ok(None);
        }
        let data = std::fs::read(path).map_err(|e| format!("Failed to read cover: {e}"))?;
        let mime = metadata::mime_from_path(path);
        Ok(Some((data, mime.to_string())))
    }
}

fn rand_simple() -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let mut hasher = DefaultHasher::new();
    nanos.hash(&mut hasher);
    (hasher.finish() & 0xFFFF_FFFF) as u32
}