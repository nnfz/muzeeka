// Player state

use parking_lot::RwLock;

pub struct PlayerState {
    pub is_playing: RwLock<bool>,
    pub is_paused: RwLock<bool>,
    pub position: RwLock<f64>,
    pub duration: RwLock<f64>,
    pub volume: RwLock<f32>,
}

impl PlayerState {
    pub fn new() -> Self {
        Self {
            is_playing: RwLock::new(false),
            is_paused: RwLock::new(false),
            position: RwLock::new(0.0),
            duration: RwLock::new(0.0),
            volume: RwLock::new(0.7),
        }
    }

    pub fn toggle_play_pause(&self) {
        let mut is_playing = self.is_playing.write();
        *is_playing = !*is_playing;
    }

    pub fn set_volume(&self, volume: f32) {
        *self.volume.write() = volume.clamp(0.0, 1.0);
    }

    pub fn seek(&self, position: f64) {
        *self.position.write() = position;
    }
}
