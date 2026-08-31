use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::plugins::http_server::HttpStatus;
use crate::plugins::{PluginInfo, PluginRegistry};

#[tauri::command]
pub fn plugins_list(registry: State<'_, Arc<PluginRegistry>>) -> Vec<PluginInfo> {
    registry.list()
}

#[tauri::command]
pub fn plugins_dir(registry: State<'_, Arc<PluginRegistry>>) -> String {
    registry.plugins_dir().display().to_string()
}

#[tauri::command]
pub fn plugins_set_enabled(
    registry: State<'_, Arc<PluginRegistry>>,
    id: String,
    enabled: bool,
) -> Result<PluginInfo, String> {
    registry.set_enabled(&id, enabled)
}

#[tauri::command]
pub fn plugin_settings_set(
    registry: State<'_, Arc<PluginRegistry>>,
    id: String,
    data: Value,
) -> Result<Value, String> {
    registry.set_plugin_settings(&id, data)
}

#[tauri::command]
pub fn plugin_http_status(
    registry: State<'_, Arc<PluginRegistry>>,
    id: String,
) -> HttpStatus {
    registry.http_status(&id)
}
