// Playlist state

use std::path::PathBuf;

#[derive(Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub track_paths: Vec<PathBuf>,
    pub cover_path: Option<PathBuf>,
}

pub struct PlaylistState {
    pub playlists: Vec<Playlist>,
    pub active_playlist_id: Option<String>,
}

impl PlaylistState {
    pub fn new() -> Self {
        Self {
            playlists: Vec::new(),
            active_playlist_id: None,
        }
    }

    pub fn add_playlist(&mut self, playlist: Playlist) {
        self.playlists.push(playlist);
    }

    pub fn get_active_playlist(&self) -> Option<&Playlist> {
        self.active_playlist_id.as_ref()
            .and_then(|id| self.playlists.iter().find(|p| &p.id == id))
    }
}
