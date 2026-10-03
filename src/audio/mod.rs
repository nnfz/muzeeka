// Audio engine module

pub mod bass;
pub mod biquad;
pub mod cue;
pub mod dev_log;
pub mod discord_rpc;
pub mod dsp_chain;
pub mod equalizer;
pub mod events;
pub mod filter;
pub mod icy_tap;
pub mod limiter;
pub mod metadata;
pub mod mix_filter;
pub mod output_tap;
pub mod player;
pub mod process_util;
pub mod stream_debug;
// pub mod unison;  // TODO: needs lyrics module (lrc_to_ttml, track_identity_matches, etc.)
pub mod waveform;

// Re-export main types
pub use events::EventSink;
pub use player::Player;
