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
