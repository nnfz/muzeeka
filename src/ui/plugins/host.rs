use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};

use super::control::{ok_value, PlayerControl, SessionControl};
use super::http::HttpHub;
use super::http_server::{sanitize_port, ServeOpts, DEFAULT_HTTP_PORT};
use super::manifest::{
    AUDIO_DEVICES, AUDIO_OUTPUT, HTTP_LISTEN, LIBRARY_READ, LIBRARY_WRITE, PLAYER_CONTROL,
    PLAYER_READ,
};
use super::model::ImportRequest;

#[derive(Clone)]
pub struct PluginHost {
    pub control: Arc<dyn PlayerControl>,
    pub http: Arc<HttpHub>,
}

impl PluginHost {
    pub fn new() -> Self {
        let control = SessionControl::shared();
        let http = HttpHub::new(Arc::clone(&control));
        Self { control, http }
    }
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
        let control = &self.control;

        match method {
            "player.state" => {
                require(perms, PLAYER_READ)?;
                Ok(serde_json::to_value(control.get_state()?).unwrap_or(Value::Null))
            }
            "player.play" => {
                require(perms, PLAYER_CONTROL)?;
                let path = string_arg(args, "path")?;
                let playlist_id = args
                    .get("playlistId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty());
                control.play(&path, playlist_id)?;
                Ok(ok_value())
            }
            "player.pause" => {
                require(perms, PLAYER_CONTROL)?;
                control.pause()?;
                Ok(ok_value())
            }
            "player.resume" => {
                require(perms, PLAYER_CONTROL)?;
                control.resume()?;
                Ok(ok_value())
            }
            "player.toggle" => {
                require(perms, PLAYER_CONTROL)?;
                control.toggle()?;
                Ok(ok_value())
            }
            "player.next" => {
                require(perms, PLAYER_CONTROL)?;
                control.next()?;
                Ok(ok_value())
            }
            "player.prev" => {
                require(perms, PLAYER_CONTROL)?;
                control.prev()?;
                Ok(ok_value())
            }
            "player.seek" => {
                require(perms, PLAYER_CONTROL)?;
                let position = args
                    .get("position")
                    .and_then(Value::as_f64)
                    .ok_or("seek: missing position")?;
                control.seek(position)?;
                Ok(ok_value())
            }
            "player.volume" => {
                require(perms, PLAYER_CONTROL)?;
                let volume = args.get("volume").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                control.set_volume(volume)?;
                Ok(ok_value())
            }
            "library.playlists" => {
                require(perms, LIBRARY_READ)?;
                Ok(serde_json::to_value(control.playlists()?).unwrap_or(Value::Null))
            }
            "library.playlist" => {
                require(perms, LIBRARY_READ)?;
                let id = string_arg(args, "id")?;
                Ok(serde_json::to_value(control.playlist(&id)?).unwrap_or(Value::Null))
            }
            "library.tracks" => {
                require(perms, LIBRARY_READ)?;
                let query = args.get("query").and_then(Value::as_str).unwrap_or("");
                control.tracks(query)
            }
            "library.createPlaylist" => {
                require(perms, LIBRARY_WRITE)?;
                let name = string_arg(args, "name")?;
                Ok(serde_json::to_value(control.create_playlist(&name)?).unwrap_or(Value::Null))
            }
            "library.ensurePlaylist" => {
                require(perms, LIBRARY_WRITE)?;
                let name = string_arg(args, "name")?;
                Ok(serde_json::to_value(control.ensure_playlist(&name)?).unwrap_or(Value::Null))
            }
            "library.renamePlaylist" => {
                require(perms, LIBRARY_WRITE)?;
                let id = playlist_id(args)?;
                let name = string_arg(args, "name")?;
                control.rename_playlist(&id, &name)?;
                Ok(ok_value())
            }
            "library.deletePlaylist" => {
                require(perms, LIBRARY_WRITE)?;
                let id = playlist_id(args)?;
                control.delete_playlist(&id)?;
                Ok(ok_value())
            }
            "library.clearPlaylist" => {
                require(perms, LIBRARY_WRITE)?;
                let id = playlist_id(args)?;
                control.clear_playlist(&id)?;
                Ok(ok_value())
            }
            "library.addTracks" | "library.import" => {
                require(perms, LIBRARY_WRITE)?;
                let request = import_request(args, method == "library.addTracks")?;
                control.import(request)
            }
            "library.removeTracks" => {
                require(perms, LIBRARY_WRITE)?;
                let id = playlist_id(args)?;
                let paths = paths_arg(args)?;
                let removed = control.remove_tracks(&id, paths)?;
                Ok(json!({ "ok": true, "removed": removed }))
            }
            "library.deleteTracks" => {
                require(perms, LIBRARY_WRITE)?;
                let paths = paths_arg(args)?;
                let removed = control.delete_tracks(paths)?;
                Ok(json!({ "ok": true, "removed": removed }))
            }
            "library.setLiked" => {
                require(perms, LIBRARY_WRITE)?;
                let path = string_arg(args, "path")?;
                let liked = args.get("liked").and_then(Value::as_bool).unwrap_or(true);
                control.set_liked(&path, liked)?;
                Ok(ok_value())
            }
            "audio.devices" => {
                require(perms, AUDIO_DEVICES)?;
                Ok(serde_json::to_value(control.devices()?).unwrap_or(Value::Null))
            }
            "audio.addOutput" => {
                require(perms, AUDIO_OUTPUT)?;
                let device_id = args
                    .get("deviceId")
                    .and_then(Value::as_i64)
                    .ok_or("addOutput: missing deviceId")? as i32;
                let volume = args
                    .get("volume")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0) as f32;
                Ok(serde_json::to_value(control.add_output(device_id, volume)?)
                    .unwrap_or(Value::Null))
            }
            "audio.setOutputVolume" => {
                require(perms, AUDIO_OUTPUT)?;
                let id = string_arg(args, "id")?;
                let volume = args
                    .get("volume")
                    .and_then(Value::as_f64)
                    .ok_or("setOutputVolume: missing volume")?
                    .clamp(0.0, 1.0) as f32;
                Ok(
                    serde_json::to_value(control.set_output_volume(&id, volume)?)
                        .unwrap_or(Value::Null),
                )
            }
            "audio.removeOutput" => {
                require(perms, AUDIO_OUTPUT)?;
                let id = string_arg(args, "id")?;
                control.remove_output(&id)?;
                Ok(ok_value())
            }
            "audio.outputs" => {
                require(perms, AUDIO_OUTPUT)?;
                Ok(serde_json::to_value(control.outputs()?).unwrap_or(Value::Null))
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
                Ok(ok_value())
            }
            "http.status" => {
                require(perms, HTTP_LISTEN)?;
                Ok(serde_json::to_value(self.http.status(plugin_id)).unwrap_or(Value::Null))
            }
            "plugin.getConfig" | "settings.get" => Ok(load_settings(plugin_id)),
            "plugin.setConfig" | "settings.set" => {
                let values = if method == "settings.set" {
                    args.get("values").cloned().unwrap_or_else(|| args.clone())
                } else {
                    args.clone()
                };
                if !values.is_object() {
                    return Err("settings.set expects an object".into());
                }
                merge_settings(plugin_id, values)
            }
            "log.info" => {
                let message = args.get("message").and_then(Value::as_str).unwrap_or("");
                super::log::push("info", plugin_id, message);
                Ok(ok_value())
            }
            "log.error" => {
                let message = args.get("message").and_then(Value::as_str).unwrap_or("");
                super::log::push("error", plugin_id, message);
                Ok(ok_value())
            }
            other => Err(format!("Unknown plugin API '{other}'")),
        }
    }
}

fn require(perms: &[String], need: &str) -> Result<(), String> {
    if perms.iter().any(|perm| perm == need) {
        Ok(())
    } else {
        Err(format!("Plugin is missing permission '{need}'"))
    }
}

fn string_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing '{key}'"))
}

fn playlist_id(args: &Value) -> Result<String, String> {
    args.get("playlistId")
        .and_then(Value::as_str)
        .or_else(|| args.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "missing 'id'".to_string())
}

pub fn paths_arg(args: &Value) -> Result<Vec<String>, String> {
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        let path = path.trim();
        if !path.is_empty() {
            return Ok(vec![path.to_string()]);
        }
    }
    let Some(value) = args.get("paths") else {
        return Err("missing 'paths'".into());
    };
    if let Some(path) = value.as_str() {
        let path = path.trim();
        if path.is_empty() {
            return Err("missing 'paths'".into());
        }
        return Ok(vec![path.to_string()]);
    }
    let Some(list) = value.as_array() else {
        return Err("missing 'paths'".into());
    };
    let paths: Vec<String> = list
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
    if paths.is_empty() {
        return Err("missing 'paths'".into());
    }
    Ok(paths)
}

fn import_request(args: &Value, require_playlist: bool) -> Result<ImportRequest, String> {
    let paths = paths_arg(args)?;
    let playlist_id = args
        .get("playlistId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let playlist_name = args
        .get("playlist")
        .or_else(|| args.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if require_playlist && playlist_id.is_none() && playlist_name.is_none() {
        return Err("missing playlist id or name".into());
    }
    Ok(ImportRequest {
        playlist_id,
        playlist_name,
        paths,
        labels: labels_arg(args),
        clear: args.get("clear").and_then(Value::as_bool).unwrap_or(false),
        open: args.get("open").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn labels_arg(args: &Value) -> Vec<crate::ui::plugins::model::ImportLabel> {
    let Some(list) = args.get("labels").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let path = item.get("path").and_then(Value::as_str)?.trim();
            if path.is_empty() {
                return None;
            }
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let artist = item
                .get("artist")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let cover = item
                .get("cover")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if title.is_empty() && artist.is_empty() && cover.is_empty() {
                return None;
            }
            Some(crate::ui::plugins::model::ImportLabel {
                path: path.to_string(),
                title,
                artist,
                cover,
            })
        })
        .collect()
}

fn plugin_settings_path(plugin_id: &str) -> Result<PathBuf, String> {
    super::paths::plugin_data_dir(plugin_id).map(|dir| dir.join("settings.json"))
}

pub fn load_settings(plugin_id: &str) -> Value {
    plugin_settings_path(plugin_id)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

pub fn save_settings(plugin_id: &str, value: &Value) -> Result<(), String> {
    let path = plugin_settings_path(plugin_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create plugin data dir: {error}"))?;
    }
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("Failed to serialize plugin settings: {error}"))?;
    std::fs::write(&path, raw).map_err(|error| format!("Failed to write plugin settings: {error}"))
}

pub fn merge_settings(plugin_id: &str, patch: Value) -> Result<Value, String> {
    let mut current = load_settings(plugin_id);
    if let (Some(obj), Some(patch_obj)) = (current.as_object_mut(), patch.as_object()) {
        for (key, value) in patch_obj {
            obj.insert(key.clone(), value.clone());
        }
    } else {
        current = patch;
    }
    save_settings(plugin_id, &current)?;
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
    let raw: Raw = serde_json::from_value(args.clone())
        .map_err(|error| format!("http.serve options: {error}"))?;
    let static_dir = raw
        .static_dir
        .filter(|value| !value.trim().is_empty())
        .map(|rel| {
            let path = PathBuf::from(rel);
            if path.is_absolute() {
                path
            } else {
                plugin_dir.join(path)
            }
        });
    Ok(ServeOpts {
        port: sanitize_port(raw.port.unwrap_or(DEFAULT_HTTP_PORT)),
        static_dir,
        mount_player_api: raw.mount.iter().any(|mount| mount == "player-api"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_accept_one_string_or_a_list() {
        let one = json!({ "path": "C:/Games/a.flac" });
        assert_eq!(
            paths_arg(&one).unwrap(),
            vec!["C:/Games/a.flac".to_string()]
        );
        let many = json!({ "paths": ["a.mp3", " ", "b.mp3"] });
        assert_eq!(
            paths_arg(&many).unwrap(),
            vec!["a.mp3".to_string(), "b.mp3".to_string()]
        );
    }

    #[test]
    fn a_cover_only_label_is_kept() {
        let args = json!({
            "labels": [
                { "path": "a", "cover": "bg.jpg" },
                { "path": "b", "title": "", "artist": "" },
                { "path": "" }
            ]
        });
        let labels = labels_arg(&args);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].path, "a");
        assert_eq!(labels[0].cover, "bg.jpg");
    }
}
