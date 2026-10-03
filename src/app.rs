// Main application state and window management

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;

use crate::audio::{EventSink, Player};
use crate::state::AppState;
use crate::ui::main_window::MainWindow;

pub struct MuzeekaApp {
    state: Model<AppState>,
    player: Arc<Player>,
    _event_sink: EventSink,
}

impl MuzeekaApp {
    pub fn new(cx: &mut ViewContext<Self>) -> Self {
        let state = cx.new_model(|cx| AppState::new(cx));

        let player = Arc::new(Player::new());
        let event_sink = EventSink::new();

        let bass_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/bass");
        player.set_bass_dir(bass_dir);

        if let Err(e) = player.init() {
            eprintln!("Failed to initialize BASS: {}", e);
        }

        player.set_event_sink(event_sink.clone());

        player.start_position_emitter(event_sink.clone());

        Self {
            state,
            player,
            _event_sink: event_sink,
        }
    }

    pub fn player(&self) -> &Player {
        &self.player
    }
}

impl Render for MuzeekaApp {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        MainWindow::new(self.state.clone())
    }
}

// Actions for keyboard shortcuts
pub struct TogglePlayPause;
impl_actions!(app, [TogglePlayPause]);

pub struct SeekForward;
impl_actions!(app, [SeekForward]);

pub struct SeekBackward;
impl_actions!(app, [SeekBackward]);

pub struct NextTrack;
impl_actions!(app, [NextTrack]);

pub struct PrevTrack;
impl_actions!(app, [PrevTrack]);

pub struct VolumeUp;
impl_actions!(app, [VolumeUp]);

pub struct VolumeDown;
impl_actions!(app, [VolumeDown]);

pub struct FocusSearch;
impl_actions!(app, [FocusSearch]);
