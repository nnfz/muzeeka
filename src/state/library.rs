// Library state

use std::path::PathBuf;

#[derive(Clone)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: f64,
    pub bpm: Option<u16>,
    pub cover_path: Option<PathBuf>,
    pub liked: bool,
}

pub struct LibraryState {
    pub tracks: Vec<Track>,
}

impl LibraryState {
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
        }
    }

    pub fn add_track(&mut self, track: Track) {
        self.tracks.push(track);
    }

    pub fn get_tracks(&self) -> &[Track] {
        &self.tracks
    }
}
