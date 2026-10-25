//! JSON shapes shared by the plugin host and the player HTTP API.
//!
//! Field names are camelCase, the same names the JS object and native `host.call` use.

use serde::{Deserialize, Serialize};

pub const ALL_ID: &str = "__all__";
pub const LIKED_ID: &str = "__liked__";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerView {
    pub is_playing: bool,
    pub is_paused: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub shuffle_enabled: bool,
    pub repeat_mode: String,
    pub track: Option<TrackInfo>,
    pub active_playlist_id: Option<String>,
    pub active_playlist_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    pub track_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub id: String,
    pub name: String,
    pub tracks: Vec<TrackInfo>,
}

/// Title and artist used when the file itself has no tags.
/// `cover` is an image path used only when the file has no embedded art.
#[derive(Debug, Clone)]
pub struct ImportLabel {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub cover: String,
}

/// What `library.import` and `library.addTracks` ask the library to do.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub playlist_id: Option<String>,
    pub playlist_name: Option<String>,
    pub paths: Vec<String>,
    /// Fills a blank title, artist, or cover. Keyed by path.
    pub labels: Vec<ImportLabel>,
    pub clear: bool,
    /// Switch the open page to this playlist after the import.
    pub open: bool,
}

pub fn is_virtual_playlist(id: &str) -> bool {
    id == ALL_ID || id == LIKED_ID
}
