use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

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

    pub fn subscribe<F>(&self, f: F)
    where
        F: Fn(PlayerEvent) + Send + Sync + 'static,
    {
        self.listeners.write().push(Arc::new(f));
    }

    pub fn emit<S: Serialize>(&self, name: &'static str, payload: S) {
        let value = match serde_json::to_value(payload) {
            Ok(v) => v,
            Err(_) => return,
        };
        self.emit_value(name, value);
    }

    pub fn emit_value(&self, name: &'static str, payload: serde_json::Value) {
        let event = PlayerEvent { name, payload };
        let listeners = self.listeners.read().clone();
        for listener in listeners {
            listener(event.clone());
        }
    }

    pub fn has_listeners(&self) -> bool {
        !self.listeners.read().is_empty()
    }
}

pub mod names {
    pub const POSITION: &str = "player:position";
    pub const TRACK_CHANGED: &str = "player:track-changed";
    pub const TRACK_ENDED: &str = "player:track-ended";
    pub const STATE: &str = "player:state";
    pub const STREAM_METADATA: &str = "player:stream-metadata";
    pub const MIX_PREVIEW: &str = "player:mix-preview";
}
