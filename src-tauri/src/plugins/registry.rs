use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use super::host::{load_settings, save_settings, PluginHost};
use super::js::JsEngine;
use super::manifest::{PluginManifest, PluginRuntime};
use super::native::NativeEngine;
use super::paths;
use super::http_server::HttpStatus;

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
    app: AppHandle,
    host: PluginHost,
    js: JsEngine,
    native: NativeEngine,
    inner: Mutex<Inner>,
}

impl PluginRegistry {
    pub fn boot(app: AppHandle, host: PluginHost) -> Result<Arc<Self>, String> {
        let js = JsEngine::spawn();
        let native = NativeEngine::new();
        let state = load_state(&app).unwrap_or_default();
        let registry = Arc::new(Self {
            app: app.clone(),
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
        registry.migrate_legacy_remote();
        registry.sync_running();
        Ok(registry)
    }

    pub fn plugins_dir(&self) -> PathBuf {
        paths::plugins_dir(&self.app)
    }

    pub fn rescan(&self) {
        let root = paths::plugins_dir(&self.app);
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
                .filter(|(id, p)| p.enabled && !inner.started.contains(*id))
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

    fn migrate_legacy_remote(&self) {
        const ID: &str = "muzeeka.remote";
        let existing = load_settings(&self.app, ID);
        if existing.get("port").is_some() {
            return;
        }
        let Ok(settings) = crate::settings::load_settings(&self.app) else {
            return;
        };
        let mut seed = serde_json::json!({
            "port": settings.legacy_remote_port,
        });
        if let Some(obj) = existing.as_object() {
            if !obj.is_empty() {
                seed = existing;
            }
        }
        let _ = save_settings(&self.app, ID, &seed);

        let mut inner = self.inner.lock();
        if !inner.state.enabled.contains_key(ID) {
            inner
                .state
                .enabled
                .insert(ID.to_string(), settings.legacy_remote_enabled);
            if let Some(plugin) = inner.plugins.get_mut(ID) {
                plugin.enabled = settings.legacy_remote_enabled;
            }
            drop(inner);
            self.persist_state();
        }
    }

    fn start_plugin(&self, id: &str) -> Result<(), String> {
        if let Ok(resolved) = self.resolved_config(id) {
            let _ = save_settings(&self.app, id, &resolved);
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
                    .map_err(|e| format!("Failed to read {}: {e}", main.display()))?;
                self.js
                    .start(id, source, permissions, dir, self.host.clone())
            }
            PluginRuntime::Native => self.native.start(
                id,
                &main,
                permissions,
                dir,
                self.host.clone(),
            ),
        }
    }

    fn stop_plugin(&self, id: &str) {
        if let Err(err) = self.js.stop(id) {
            eprintln!("[plugins] stop js {id}: {err}");
        }
        if let Err(err) = self.native.stop(id) {
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
            .map(|p| {
                let stored = load_settings(&self.app, &p.manifest.id);
                PluginInfo {
                    id: p.manifest.id.clone(),
                    name: p.manifest.name.clone(),
                    version: p.manifest.version.clone(),
                    author: p.manifest.author.clone(),
                    description: p.manifest.description.clone(),
                    enabled: p.enabled,
                    permissions: p.manifest.permissions.clone(),
                    settings: p.manifest.settings.clone(),
                    config: p.manifest.resolved_config(&stored),
                    runtime: p.manifest.runtime(),
                    error: p.error.clone(),
                    http: self.host.http.status(&p.manifest.id),
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
                self.emit_updated();
                return Err(err);
            }
            self.inner.lock().started.insert(id.to_string());
        } else {
            self.stop_plugin(id);
            self.inner.lock().started.remove(id);
        }
        self.emit_updated();
        self.list()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("Unknown plugin {id}"))
    }

    pub fn set_plugin_settings(&self, id: &str, value: Value) -> Result<Value, String> {
        let manifest = self.manifest_of(id)?;
        let mut current = load_settings(&self.app, id);
        if let (Some(obj), Some(patch)) = (current.as_object_mut(), value.as_object()) {
            for (k, v) in patch {
                obj.insert(k.clone(), v.clone());
            }
        } else if value.is_object() {
            current = value;
        } else {
            return Err("settings must be an object".into());
        }
        let resolved = manifest.resolved_config(&current);
        save_settings(&self.app, id, &resolved)?;
        self.restart_if_enabled(id)?;
        self.emit_updated();
        Ok(resolved)
    }

    fn manifest_of(&self, id: &str) -> Result<PluginManifest, String> {
        self.inner
            .lock()
            .plugins
            .get(id)
            .map(|p| p.manifest.clone())
            .ok_or_else(|| format!("Unknown plugin {id}"))
    }

    fn resolved_config(&self, id: &str) -> Result<Value, String> {
        let manifest = self.manifest_of(id)?;
        Ok(manifest.resolved_config(&load_settings(&self.app, id)))
    }

    fn restart_if_enabled(&self, id: &str) -> Result<(), String> {
        let enabled = self
            .inner
            .lock()
            .plugins
            .get(id)
            .map(|p| p.enabled)
            .unwrap_or(false);
        if enabled {
            self.stop_plugin(id);
            self.start_plugin(id)?;
            self.inner.lock().started.insert(id.to_string());
        }
        Ok(())
    }

    pub fn http_status(&self, id: &str) -> HttpStatus {
        self.host.http.status(id)
    }

    pub fn shutdown(&self) {
        let ids: Vec<String> = self.inner.lock().plugins.keys().cloned().collect();
        for id in ids {
            self.stop_plugin(&id);
        }
        self.host.http.stop_all();
    }

    fn persist_state(&self) {
        let state = self.inner.lock().state.clone();
        if let Ok(path) = paths::registry_state_path(&self.app) {
            if let Ok(raw) = serde_json::to_string_pretty(&state) {
                let _ = fs::write(path, raw);
            }
        }
    }

    fn emit_updated(&self) {
        let list = self.list();
        if let Err(e) = self.app.emit("plugins:updated", &list) {
            eprintln!("[plugins] emit failed: {e}");
        }
    }
}

fn load_state(app: &AppHandle) -> Result<RegistryState, String> {
    let path = paths::registry_state_path(app)?;
    if !path.is_file() {
        return Ok(RegistryState::default());
    }
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
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
        if path.file_name().and_then(|n| n.to_str()) == Some("sdk") {
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
            Err(err) => eprintln!("[plugins] skip {}: {err}", path.display()),
        }
    }
}
