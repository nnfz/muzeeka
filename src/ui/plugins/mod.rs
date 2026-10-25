//! Plugin host: JS and native DLLs, plus the player HTTP API.
//!
//! Plugins live in `plugins/<name>/plugin.json`. Settings and the enabled flag
//! are stored under `%APPDATA%\com.nnfz.muzeeka`, same as the previous app.

mod bridge;
mod control;
mod host;
mod http;
mod http_server;
mod js;
mod log;
mod manifest;
pub(crate) mod model;
mod native;
mod paths;
mod registry;

use gpui::{App, Entity};
use serde_json::Value;

pub use manifest::{PluginRuntime, PluginSettingSpec, SettingKind};
pub use model::ImportRequest;
pub use http_server::HttpStatus;
pub use registry::PluginInfo;

use crate::ui::session::Session;

/// Start the host and keep draining plugin calls from the UI thread.
pub fn attach(session: Entity<Session>, cx: &mut gpui::Context<crate::ui::main_window::MainWindow>) {
    bridge::set_session(session);
    bridge::enter(cx, registry::boot);
    cx.spawn(async move |view, async_cx| loop {
        async_cx
            .background_executor()
            .timer(std::time::Duration::from_millis(40))
            .await;
        let drained = view.update(async_cx, |_view, cx| {
            if let Some(session) = bridge::session() {
                bridge::drain(cx, &session);
            }
        });
        if drained.is_err() {
            break;
        }
    })
    .detach();
}

pub fn shutdown(cx: &mut App) {
    bridge::enter(cx, || {
        if let Some(registry) = registry::registry() {
            registry.shutdown();
        }
    });
}

pub fn plugins_dir() -> std::path::PathBuf {
    paths::plugins_dir()
}

pub fn list(cx: &mut App) -> Vec<PluginInfo> {
    bridge::enter(cx, || {
        registry::registry()
            .map(|registry| registry.list())
            .unwrap_or_default()
    })
}

pub fn set_enabled(cx: &mut App, id: &str, enabled: bool) -> Result<PluginInfo, String> {
    let id = id.to_string();
    bridge::enter(cx, || {
        let registry = registry::registry().ok_or_else(|| "Plugins are not running".to_string())?;
        registry.set_enabled(&id, enabled)
    })
}

pub fn set_config(cx: &mut App, id: &str, patch: Value) -> Result<Value, String> {
    let id = id.to_string();
    bridge::enter(cx, || {
        let registry = registry::registry().ok_or_else(|| "Plugins are not running".to_string())?;
        registry.set_plugin_settings(&id, patch)
    })
}

pub fn open_plugins_dir() -> Result<(), String> {
    let dir = paths::plugins_dir();
    std::fs::create_dir_all(&dir).ok();
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|error| format!("Could not open the plugins folder: {error}"))?;
    Ok(())
}

pub fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map_err(|error| format!("Could not open {url}: {error}"))?;
    Ok(())
}
