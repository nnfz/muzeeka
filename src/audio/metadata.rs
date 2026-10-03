// Audio metadata extraction module — STUB
//
// TODO: Port from src-tauri/src/metadata.rs (2565 lines)
// - lofty + id3 dual metadata path
// - FNV-1a content-addressed cover art cache
// - atomic temp→rename file writes
// - per-content-id locks for album-art writes

#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    pub file_name: String,
    pub extension: String,
    pub size: u64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub genre: Option<String>,
    pub cover_path: Option<String>,
    pub cover_path_full: Option<String>,
}

pub fn read_metadata(_audio_path: &str, _file_name: &str) -> TrackMetadata {
    TrackMetadata::default()
}

pub fn read_metadata_fast(_audio_path: &str, _file_name: &str) -> TrackMetadata {
    TrackMetadata::default()
}

pub fn strip_ytdlp_id_suffix(s: &str) -> String {
    s.to_string()
}
