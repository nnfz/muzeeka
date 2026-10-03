// Event sink — replaces Tauri's `AppHandle` + `Emitter` for the ported player.
//
// In the Tauri build the player held an `AppHandle` and called `app.emit(name, payload)`.
// Here the same call sites go through [`EventSink`], a `Send + Sync` list of
// subscribers. The GPUI view registers a listener that forwards payloads into its
// own state (via `cx.spawn` / an mpsc drain), so no engine code needs to know
// about GPUI at all.

use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

/// One received event. Payloads are serialized to `serde_json::Value` at the
/// call site so a sink can be registered before the concrete type is known.
#[derive(Debug, Clone)]
pub struct PlayerEvent {
    pub name: &'static str,
    pub payload: serde_json::Value,
}

type Listener = Arc<dyn Fn(PlayerEvent) + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct EventSink {
    listeners: Arc<RwLock<Vec<Listener>>>,
}

impl EventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a listener. Kept for the process lifetime alongside the sinks
    /// that own it; there is no unsubscribe because the player outlives the views.
    pub fn subscribe<F>(&self, f: F)
    where
        F: Fn(PlayerEvent) + Send + Sync + 'static,
    {
        self.listeners.write().push(Arc::new(f));
    }

    /// Tauri-compatible emit: serialize the payload and fan out.
    pub fn emit<S: Serialize>(&self, name: &'static str, payload: S) {
        let value = match serde_json::to_value(payload) {
            Ok(v) => v,
            Err(_) => return,
        };
        self.emit_value(name, value);
    }

    pub fn emit_value(&self, name: &'static str, payload: serde_json::Value) {
        let event = PlayerEvent { name, payload };
        // Clone the listener list so a callback that subscribes cannot deadlock.
        let listeners = self.listeners.read().clone();
        for listener in listeners {
            listener(event.clone());
        }
    }

    pub fn has_listeners(&self) -> bool {
        !self.listeners.read().is_empty()
    }
}

/// Event names used by the player (same strings the Tauri frontend listened to).
pub mod names {
    pub const POSITION: &str = "player:position";
    pub const TRACK_CHANGED: &str = "player:track-changed";
    pub const TRACK_ENDED: &str = "player:track-ended";
    pub const STATE: &str = "player:state";
    pub const STREAM_METADATA: &str = "player:stream-metadata";
    pub const MIX_PREVIEW: &str = "player:mix-preview";
}
