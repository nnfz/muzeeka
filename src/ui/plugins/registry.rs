use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bridge;
use super::host::{load_settings, save_settings, PluginHost};
use super::http_server::HttpStatus;
use super::js::JsEngine;
use super::manifest::{PluginManifest, PluginRuntime};
use super::native::NativeEngine;
use super::paths;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub enabled: bool,
    pub permissions: Vec<String>,
    pub settings: Vec<super::manifest::PluginSettingSpec>,
    pub config: serde_json::Value,
    pub runtime: PluginRuntime,
    pub error: Option<String>,
    pub http: HttpStatus,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryState {
    #[serde(default)]
    enabled: HashMap<String, bool>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    dir: PathBuf,
    enabled: bool,
    error: Option<String>,
}

struct Inner {
    plugins: HashMap<String, LoadedPlugin>,
    state: RegistryState,
    started: HashSet<String>,
}

pub struct PluginRegistry {
    host: PluginHost,
    js: JsEngine,
    native: Arc<NativeEngine>,
    inner: Mutex<Inner>,
}

static REGISTRY: Mutex<Option<Arc<PluginRegistry>>> = Mutex::new(None);

impl PluginRegistry {
    pub fn boot() -> Arc<Self> {
        if let Some(existing) = REGISTRY.lock().clone() {
            return existing;
        }
        let host = PluginHost::new();
        let js = JsEngine::spawn();
        let native = Arc::new(NativeEngine::new());
        let state = load_state().unwrap_or_default();
        let registry = Arc::new(Self {
            host,
            js,
            native,
            inner: Mutex::new(Inner {
                plugins: HashMap::new(),
                state,
                started: HashSet::new(),
            }),
        });
        registry.rescan();
        registry.sync_running();
        *REGISTRY.lock() = Some(Arc::clone(&registry));
        registry
    }

    pub fn rescan(&self) {
        let root = paths::plugins_dir();
        let mut found: HashMap<String, LoadedPlugin> = HashMap::new();
        scan_dir(&root, &mut found);

        let mut inner = self.inner.lock();
        let previous_errors: HashMap<String, String> = inner
            .plugins
            .iter()
            .filter_map(|(id, plugin)| plugin.error.clone().map(|err| (id.clone(), err)))
            .collect();
        for (id, plugin) in found.iter_mut() {
            plugin.enabled = inner
                .state
                .enabled
                .get(id)
                .copied()
                .unwrap_or(plugin.manifest.enabled_by_default);
            plugin.error = previous_errors.get(id).cloned();
        }
        inner.plugins = found;
    }

    fn sync_running(&self) {
        let (to_start, to_stop) = {
            let inner = self.inner.lock();
            let present: HashSet<String> = inner.plugins.keys().cloned().collect();
            let to_stop: Vec<String> = inner
                .started
                .iter()
                .filter(|id| !present.contains(*id))
                .cloned()
                .collect();
            let to_start: Vec<String> = inner
                .plugins
                .iter()
                .filter(|(id, plugin)| plugin.enabled && !inner.started.contains(*id))
                .map(|(id, _)| id.clone())
                .collect();
            (to_start, to_stop)
        };
        for id in to_stop {
            self.stop_plugin(&id);
            self.inner.lock().started.remove(&id);
        }
        for id in to_start {
            match self.start_plugin(&id) {
                Ok(()) => {
                    let mut inner = self.inner.lock();
                    inner.started.insert(id.clone());
                    if let Some(plugin) = inner.plugins.get_mut(&id) {
                        plugin.error = None;
                    }
                }
                Err(err) => {
                    eprintln!("[plugins] failed to start {id}: {err}");
                    if let Some(plugin) = self.inner.lock().plugins.get_mut(&id) {
                        plugin.error = Some(err);
                    }
                }
            }
        }
    }

    fn start_plugin(&self, id: &str) -> Result<(), String> {
        if let Ok(resolved) = self.resolved_config(id) {
            let _ = save_settings(id, &resolved);
        }
        let (runtime, main, permissions, dir) = {
            let inner = self.inner.lock();
            let plugin = inner
                .plugins
                .get(id)
                .ok_or_else(|| format!("Unknown plugin {id}"))?;
            (
                plugin.manifest.runtime(),
                plugin.dir.join(&plugin.manifest.main),
                plugin.manifest.permissions.clone(),
                plugin.dir.clone(),
            )
        };
        match runtime {
            PluginRuntime::Js => {
                let source = fs::read_to_string(&main)
                    .map_err(|error| format!("Failed to read {}: {error}", main.display()))?;
                self.js
                    .start(id, source, permissions, dir, self.host.clone())
            }
            PluginRuntime::Native => {
                // start() may spawn workers that call back into the player.
                // Run it off the UI thread and keep draining those calls.
                let native = Arc::clone(&self.native);
                let host = self.host.clone();
                let id = id.to_string();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::Builder::new()
                    .name(format!("plugin-start-{id}"))
                    .spawn(move || {
                        let result = native.start(&id, &main, permissions, dir, host);
                        let _ = tx.send(result);
                    })
                    .map_err(|error| format!("Failed to start plugin thread: {error}"))?;
                bridge::wait_on_ui(&rx, Duration::from_secs(30))?
            }
        }
    }

    fn stop_plugin(&self, id: &str) {
        if let Err(err) = self.js.stop(id) {
            eprintln!("[plugins] stop js {id}: {err}");
        }
        let native = Arc::clone(&self.native);
        let id_owned = id.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name(format!("plugin-stop-{id}"))
            .spawn(move || {
                let result = native.stop(&id_owned);
                let _ = tx.send(result);
            })
            .is_err()
        {
            if let Err(err) = self.native.stop(id) {
                eprintln!("[plugins] stop native {id}: {err}");
            }
            return;
        }
        if let Err(err) = bridge::wait_on_ui(&rx, Duration::from_secs(8)) {
            eprintln!("[plugins] stop native {id}: {err}");
        }
        self.host.http.stop(id);
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.rescan();
        self.sync_running();
        let inner = self.inner.lock();
        let mut list: Vec<PluginInfo> = inner
            .plugins
            .values()
            .map(|plugin| {
                let stored = load_settings(&plugin.manifest.id);
                PluginInfo {
                    id: plugin.manifest.id.clone(),
                    name: plugin.manifest.name.clone(),
                    version: plugin.manifest.version.clone(),
                    author: plugin.manifest.author.clone(),
                    description: plugin.manifest.description.clone(),
                    enabled: plugin.enabled,
                    permissions: plugin.manifest.permissions.clone(),
                    settings: plugin.manifest.settings.clone(),
                    config: plugin.manifest.resolved_config(&stored),
                    runtime: plugin.manifest.runtime(),
                    error: plugin.error.clone(),
                    http: self.host.http.status(&plugin.manifest.id),
                    log: super::log::recent(&plugin.manifest.id),
                }
            })
            .collect();
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        list
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginInfo, String> {
        {
            let mut inner = self.inner.lock();
            let plugin = inner
                .plugins
                .get_mut(id)
                .ok_or_else(|| format!("Unknown plugin {id}"))?;
            plugin.enabled = enabled;
            plugin.error = None;
            inner.state.enabled.insert(id.to_string(), enabled);
        }
        self.persist_state();
        if enabled {
            if let Err(err) = self.start_plugin(id) {
                if let Some(plugin) = self.inner.lock().plugins.get_mut(id) {
                    plugin.error = Some(err.clone());
                }
                return Err(err);
            }
            self.inner.lock().started.insert(id.to_string());
        } else {
            self.stop_plugin(id);
            self.inner.lock().started.remove(id);
        }
        self.list()
            .into_iter()
            .find(|plugin| plugin.id == id)
            .ok_or_else(|| format!("Unknown plugin {id}"))
    }

    pub fn set_plugin_settings(&self, id: &str, value: Value) -> Result<Value, String> {
        let manifest = self.manifest_of(id)?;
        let mut current = load_settings(id);
        if let (Some(obj), Some(patch)) = (current.as_object_mut(), value.as_object()) {
            for (key, item) in patch {
                obj.insert(key.clone(), item.clone());
            }
        } else if value.is_object() {
            current = value;
        } else {
            return Err("settings must be an object".into());
        }
        let resolved = manifest.resolved_config(&current);
        save_settings(id, &resolved)?;
        self.restart_if_enabled(id)?;
        Ok(resolved)
    }

    fn manifest_of(&self, id: &str) -> Result<PluginManifest, String> {
        self.inner
            .lock()
            .plugins
            .get(id)
            .map(|plugin| plugin.manifest.clone())
            .ok_or_else(|| format!("Unknown plugin {id}"))
    }

    fn resolved_config(&self, id: &str) -> Result<Value, String> {
        let manifest = self.manifest_of(id)?;
        Ok(manifest.resolved_config(&load_settings(id)))
    }

    fn restart_if_enabled(&self, id: &str) -> Result<(), String> {
        let enabled = self
            .inner
            .lock()
            .plugins
            .get(id)
            .map(|plugin| plugin.enabled)
            .unwrap_or(false);
        if enabled {
            self.stop_plugin(id);
            self.start_plugin(id)?;
            self.inner.lock().started.insert(id.to_string());
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        let ids: Vec<String> = self.inner.lock().started.iter().cloned().collect();
        for id in ids {
            self.stop_plugin(&id);
        }
        self.inner.lock().started.clear();
        self.host.http.stop_all();
    }

    fn persist_state(&self) {
        let state = self.inner.lock().state.clone();
        if let Ok(path) = paths::registry_state_path() {
            if let Ok(raw) = serde_json::to_string_pretty(&state) {
                let _ = fs::write(path, raw);
            }
        }
    }
}

pub fn registry() -> Option<Arc<PluginRegistry>> {
    REGISTRY.lock().clone()
}

pub fn boot() -> Arc<PluginRegistry> {
    PluginRegistry::boot()
}

fn load_state() -> Result<RegistryState, String> {
    let path = paths::registry_state_path()?;
    if !path.is_file() {
        return Ok(RegistryState::default());
    }
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

fn scan_dir(root: &Path, out: &mut HashMap<String, LoadedPlugin>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("sdk") {
            continue;
        }
        let manifest_path = path.join("plugin.json");
        match PluginManifest::load(&manifest_path) {
            Ok(manifest) => {
                out.insert(
                    manifest.id.clone(),
                    LoadedPlugin {
                        manifest,
                        dir: path,
                        enabled: false,
                        error: None,
                    },
                );
            }
            Err(err) => {
                eprintln!("[plugins] skip {}: {err}", path.display());
            }
        }
    }
}

