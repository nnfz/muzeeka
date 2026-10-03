// Application state management

use gpui::*;
use parking_lot::RwLock;
use std::sync::Arc;

pub mod player;
pub mod library;
pub mod playlists;

pub struct AppState {
    pub player: Arc<player::PlayerState>,
    pub library: Arc<RwLock<library::LibraryState>>,
    pub playlists: Arc<RwLock<playlists::PlaylistState>>,
    pub search_query: String,
}

impl AppState {
    pub fn new(cx: &mut ModelContext<Self>) -> Self {
        let player = Arc::new(player::PlayerState::new());
        let library = Arc::new(RwLock::new(library::LibraryState::new()));
        let playlists = Arc::new(RwLock::new(playlists::PlaylistState::new()));

        Self {
            player,
            library,
            playlists,
            search_query: String::new(),
        }
    }

    pub fn set_search_query(&mut self, query: String, cx: &mut ModelContext<Self>) {
        self.search_query = query;
        cx.notify();
    }
}
