use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::player::Player;
use super::http_server::{sanitize_port, ServeOpts, DEFAULT_HTTP_PORT};
use crate::session::PlaybackSession;

use super::http::HttpHub;
use super::manifest::{
    AUDIO_DEVICES, AUDIO_OUTPUT, HTTP_LISTEN, LIBRARY_READ, PLAYER_CONTROL, PLAYER_READ,
};

/// Shared services plugins call into. Per-plugin permissions/dir are passed on each dispatch.
#[derive(Clone)]
pub struct PluginHost {
    pub app: AppHandle,
    pub session: Arc<PlaybackSession>,
    pub player: Player,
    pub http: Arc<HttpHub>,
}

pub struct PluginCall<'a> {
    pub plugin_id: &'a str,
    pub permissions: &'a [String],
    pub dir: &'a Path,
}

impl PluginHost {
    pub fn dispatch(
        &self,
        call: &PluginCall<'_>,
        method: &str,
        payload: &str,
    ) -> Result<Value, String> {
        let args: Value = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_str(payload).unwrap_or_else(|_| json!({}))
        };
        self.dispatch_value(call, method, &args)
    }

    pub fn dispatch_value(
        &self,
        call: &PluginCall<'_>,
        method: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let perms = call.permissions;
        let dir = call.dir;
        let plugin_id = call.plugin_id;

        match method {
            "player.state" => {
                require(perms, PLAYER_READ)?;
                Ok(serde_json::to_value(self.session.get_state()?).unwrap_or(Value::Null))
            }
            "player.play" => {
                require(perms, PLAYER_CONTROL)?;
                let path = string_arg(args, "path")?;
                let playlist_id = args
                    .get("playlistId")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                self.session.play(&path, playlist_id)?;
                ok()
            }
            "player.pause" => {
                require(perms, PLAYER_CONTROL)?;
                self.session.pause()?;
                ok()
            }
            "player.resume" => {
                require(perms, PLAYER_CONTROL)?;
                self.session.resume()?;
                ok()
            }
            "player.toggle" => {
                require(perms, PLAYER_CONTROL)?;
                self.session.toggle()?;
                ok()
            }
            "player.next" => {
                require(perms, PLAYER_CONTROL)?;
                self.session.next()?;
                ok()
            }
            "player.prev" => {
                require(perms, PLAYER_CONTROL)?;
                self.session.prev()?;
                ok()
            }
            "player.seek" => {
                require(perms, PLAYER_CONTROL)?;
                let position = args
                    .get("position")
                    .and_then(Value::as_f64)
                    .ok_or("seek: missing position")?;
                self.session.seek(position)?;
                ok()
            }
            "player.volume" => {
                require(perms, PLAYER_CONTROL)?;
                let volume = args.get("volume").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                self.session.set_volume(volume)?;
                ok()
            }
            "library.playlists" => {
                require(perms, LIBRARY_READ)?;
                Ok(serde_json::to_value(self.session.get_playlists()?).unwrap_or(Value::Null))
            }
            "library.playlist" => {
                require(perms, LIBRARY_READ)?;
                let id = string_arg(args, "id")?;
                Ok(serde_json::to_value(self.session.get_playlist_view(&id)?).unwrap_or(Value::Null))
            }
            "audio.devices" => {
                require(perms, AUDIO_DEVICES)?;
                Ok(serde_json::to_value(self.player.list_output_devices()?).unwrap_or(Value::Null))
            }
            "audio.addOutput" => {
                require(perms, AUDIO_OUTPUT)?;
                let device_id = args
                    .get("deviceId")
                    .and_then(Value::as_i64)
                    .ok_or("addOutput: missing deviceId")? as i32;
                // Optional: gain for this output alone. Defaults to full volume so
                // existing callers keep the old behaviour.
                let volume = args
                    .get("volume")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0) as f32;
                let info = self.player.add_extra_output(device_id, volume)?;
                Ok(serde_json::to_value(info).unwrap_or(Value::Null))
            }
            "audio.setOutputVolume" => {
                require(perms, AUDIO_OUTPUT)?;
                let id = string_arg(args, "id")?;
                let volume = args
                    .get("volume")
                    .and_then(Value::as_f64)
                    .ok_or("setOutputVolume: missing volume")?
                    .clamp(0.0, 1.0) as f32;
                let info = self.player.set_extra_output_volume(&id, volume)?;
                Ok(serde_json::to_value(info).unwrap_or(Value::Null))
            }
            "audio.removeOutput" => {
                require(perms, AUDIO_OUTPUT)?;
                let id = string_arg(args, "id")?;
                self.player.remove_extra_output(&id)?;
                ok()
            }
            "audio.outputs" => {
                require(perms, AUDIO_OUTPUT)?;
                Ok(serde_json::to_value(self.player.extra_outputs()?).unwrap_or(Value::Null))
            }
            "http.serve" => {
                require(perms, HTTP_LISTEN)?;
                let opts = parse_serve_opts(args, dir)?;
                let status = self.http.serve(plugin_id, opts)?;
                Ok(serde_json::to_value(status).unwrap_or(Value::Null))
            }
            "http.stop" => {
                require(perms, HTTP_LISTEN)?;
                self.http.stop(plugin_id);
                ok()
            }
            "http.status" => {
                require(perms, HTTP_LISTEN)?;
                Ok(serde_json::to_value(self.http.status(plugin_id)).unwrap_or(Value::Null))
            }
            "settings.get" => Ok(load_settings(&self.app, plugin_id)),
            "settings.set" => {
                let values = args.get("values").cloned().unwrap_or_else(|| args.clone());
                if !values.is_object() {
                    return Err("settings.set expects an object".into());
                }
                merge_settings(&self.app, plugin_id, values)
            }
            "log.info" => {
                let message = args.get("message").and_then(Value::as_str).unwrap_or("");
                crate::dev_log::push(&self.app, "info", plugin_id, message);
                ok()
            }
            "log.error" => {
                let message = args.get("message").and_then(Value::as_str).unwrap_or("");
                crate::dev_log::push(&self.app, "error", plugin_id, message);
                ok()
            }
            other => Err(format!("Unknown plugin API '{other}'")),
        }
    }
}

fn require(perms: &[String], need: &str) -> Result<(), String> {
    if perms.iter().any(|p| p == need) {
        Ok(())
    } else {
        Err(format!("Plugin is missing permission '{need}'"))
    }
}

fn string_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing '{key}'"))
}

fn ok() -> Result<Value, String> {
    Ok(json!({ "ok": true }))
}

fn plugin_settings_path(app: &AppHandle, plugin_id: &str) -> Result<PathBuf, String> {
    super::paths::plugin_data_dir(app, plugin_id).map(|dir| dir.join("settings.json"))
}

pub fn load_settings(app: &AppHandle, plugin_id: &str) -> Value {
    plugin_settings_path(app, plugin_id)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

pub fn save_settings(app: &AppHandle, plugin_id: &str, value: &Value) -> Result<(), String> {
    let path = plugin_settings_path(app, plugin_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create plugin data dir: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(value)
        .map_err(|e| format!("Failed to serialize plugin settings: {e}"))?;
    std::fs::write(&path, raw).map_err(|e| format!("Failed to write plugin settings: {e}"))
}

pub fn merge_settings(app: &AppHandle, plugin_id: &str, patch: Value) -> Result<Value, String> {
    let mut current = load_settings(app, plugin_id);
    if let (Some(obj), Some(patch_obj)) = (current.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            obj.insert(k.clone(), v.clone());
        }
    } else {
        current = patch;
    }
    save_settings(app, plugin_id, &current)?;
    Ok(current)
}

fn parse_serve_opts(args: &Value, plugin_dir: &Path) -> Result<ServeOpts, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        static_dir: Option<String>,
        #[serde(default)]
        mount: Vec<String>,
    }
    let raw: Raw =
        serde_json::from_value(args.clone()).map_err(|e| format!("http.serve options: {e}"))?;
    let static_dir = raw.static_dir.filter(|s| !s.trim().is_empty()).map(|rel| {
        let p = PathBuf::from(rel);
        if p.is_absolute() {
            p
        } else {
            plugin_dir.join(p)
        }
    });
    Ok(ServeOpts {
        port: sanitize_port(raw.port.unwrap_or(DEFAULT_HTTP_PORT)),
        static_dir,
        mount_player_api: raw.mount.iter().any(|m| m == "player-api"),
    })
}
