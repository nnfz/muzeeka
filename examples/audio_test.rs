// Minimal test to verify audio engine compiles

use std::path::PathBuf;
use muzeeka_gpui::audio::{Player, EventSink};

fn main() {
    let player = Player::new();
    let sink = EventSink::new();

    // Set BASS directory
    let bass_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bass");
    player.set_bass_dir(bass_dir);

    // Initialize BASS
    match player.init() {
        Ok(()) => println!("✓ BASS initialized"),
        Err(e) => eprintln!("✗ BASS init failed: {}", e),
    }

    // Wire event sink
    player.set_event_sink(sink.clone());

    // Subscribe to events
    sink.subscribe(|event| {
        println!("Event: {} → {:?}", event.name, event.payload);
    });

    player.start_position_emitter(sink);

    println!("✓ Audio engine ready");
    println!("Press Ctrl+C to exit");

    // Keep alive
    std::thread::park();
}
