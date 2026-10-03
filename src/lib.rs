// Muzeeka Native - Music Player Library
//
// Core audio engine and state management

pub mod audio;
pub mod library;

// Re-export main types
pub use audio::{EventSink, Player};
pub use library::MusicFile;
