use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use super::control::PlayerControl;
use super::http_server::{HttpServer, HttpStatus, ServeOpts};

/// One HTTP listener per plugin.
pub struct HttpHub {
    control: Arc<dyn PlayerControl>,
    servers: Mutex<HashMap<String, Arc<HttpServer>>>,
}

impl HttpHub {
    pub fn new(control: Arc<dyn PlayerControl>) -> Arc<Self> {
        Arc::new(Self {
            control,
            servers: Mutex::new(HashMap::new()),
        })
    }

    pub fn serve(&self, plugin_id: &str, opts: ServeOpts) -> Result<HttpStatus, String> {
        let server = {
            let mut servers = self.servers.lock();
            servers
                .entry(plugin_id.to_string())
                .or_insert_with(|| HttpServer::new(Arc::clone(&self.control)))
                .clone()
        };
        Ok(server.apply(opts))
    }

    pub fn stop(&self, plugin_id: &str) {
        if let Some(server) = self.servers.lock().get(plugin_id).cloned() {
            server.stop();
        }
    }

    pub fn status(&self, plugin_id: &str) -> HttpStatus {
        self.servers
            .lock()
            .get(plugin_id)
            .map(|server| server.status())
            .unwrap_or_else(HttpStatus::stopped)
    }

    pub fn stop_all(&self) {
        let servers: Vec<_> = self.servers.lock().values().cloned().collect();
        for server in servers {
            server.stop();
        }
    }
}
