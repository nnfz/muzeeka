// Library scanning module — STUB
//
// TODO: Port from src-tauri/src/library.rs (743 lines)
// - rayon parallel scanning
// - rusqlite DB integration

#[derive(Debug, Clone)]
pub struct MusicFile {
    pub path: String,
    pub file_name: String,
    pub extension: String,
    pub size: u64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: f64,
    pub duration_secs: Option<f64>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub genre: Option<String>,
    pub cover_path: Option<String>,
    pub cover_path_full: Option<String>,
    pub audio_path: Option<String>,
    pub cue_start_secs: Option<f64>,
    pub cue_end_secs: Option<f64>,
}

pub struct LibraryScanner;

impl LibraryScanner {
    pub fn new() -> Self {
        Self
    }
}
