use std::sync::Arc;

use serde_json::{json, Value};

use super::bridge;
use super::model::{ImportRequest, PlayerView, PlaylistDetail, PlaylistSummary};
use muzeeka::audio::player::{ExtraOutputInfo, OutputDevice};

pub trait PlayerControl: Send + Sync {
    fn get_state(&self) -> Result<PlayerView, String>;
    fn play(&self, path: &str, playlist_id: Option<&str>) -> Result<(), String>;
    fn pause(&self) -> Result<(), String>;
    fn resume(&self) -> Result<(), String>;
    fn toggle(&self) -> Result<(), String>;
    fn next(&self) -> Result<(), String>;
    fn prev(&self) -> Result<(), String>;
    fn seek(&self, position: f64) -> Result<(), String>;
    fn set_volume(&self, volume: f32) -> Result<(), String>;
    fn playlists(&self) -> Result<Vec<PlaylistSummary>, String>;
    fn playlist(&self, id: &str) -> Result<PlaylistDetail, String>;
    fn select_playlist(&self, id: &str) -> Result<(), String>;
    fn toggle_shuffle(&self) -> Result<bool, String>;
    fn toggle_repeat(&self) -> Result<String, String>;
    fn cover_bytes(&self, path: &str) -> Result<Option<(Vec<u8>, String)>, String>;
    fn devices(&self) -> Result<Vec<OutputDevice>, String>;
    fn add_output(&self, device_id: i32, volume: f32) -> Result<ExtraOutputInfo, String>;
    fn remove_output(&self, id: &str) -> Result<(), String>;
    fn set_output_volume(&self, id: &str, volume: f32) -> Result<ExtraOutputInfo, String>;
    fn outputs(&self) -> Result<Vec<ExtraOutputInfo>, String>;
    fn create_playlist(&self, name: &str) -> Result<PlaylistSummary, String>;
    fn ensure_playlist(&self, name: &str) -> Result<PlaylistSummary, String>;
    fn rename_playlist(&self, id: &str, name: &str) -> Result<(), String>;
    fn delete_playlist(&self, id: &str) -> Result<(), String>;
    fn clear_playlist(&self, id: &str) -> Result<(), String>;
    fn remove_tracks(&self, id: &str, paths: Vec<String>) -> Result<usize, String>;
    fn delete_tracks(&self, paths: Vec<String>) -> Result<usize, String>;
    fn set_liked(&self, path: &str, liked: bool) -> Result<(), String>;
    fn tracks(&self, query: &str) -> Result<Value, String>;
    fn import(&self, request: ImportRequest) -> Result<Value, String>;
}

#[derive(Clone, Default)]
pub struct SessionControl;

impl SessionControl {
    pub fn shared() -> Arc<dyn PlayerControl> {
        Arc::new(Self)
    }
}

impl PlayerControl for SessionControl {
    fn get_state(&self) -> Result<PlayerView, String> {
        bridge::with_session(|session| Ok(session.plugin_state()))
    }

    fn play(&self, path: &str, playlist_id: Option<&str>) -> Result<(), String> {
        let path = path.to_string();
        let playlist_id = playlist_id.map(str::to_string);
        bridge::with_session(move |session| session.plugin_play(&path, playlist_id.as_deref()))
    }

    fn pause(&self) -> Result<(), String> {
        bridge::with_session(|session| {
            session.plugin_pause();
            Ok(())
        })
    }

    fn resume(&self) -> Result<(), String> {
        bridge::with_session(|session| {
            session.plugin_resume();
            Ok(())
        })
    }

    fn toggle(&self) -> Result<(), String> {
        bridge::with_session(|session| {
            session.plugin_toggle();
            Ok(())
        })
    }

    fn next(&self) -> Result<(), String> {
        bridge::with_session(|session| {
            session.next();
            Ok(())
        })
    }

    fn prev(&self) -> Result<(), String> {
        bridge::with_session(|session| {
            session.prev();
            Ok(())
        })
    }

    fn seek(&self, position: f64) -> Result<(), String> {
        bridge::with_session(move |session| {
            let duration = session
                .current_track()
                .map(|track| track.duration)
                .unwrap_or(0.0);
            if duration <= 0.0 {
                return Ok(());
            }
            let fraction = (position / duration).clamp(0.0, 1.0) as f32;
            session.seek(fraction);
            Ok(())
        })
    }

    fn set_volume(&self, volume: f32) -> Result<(), String> {
        bridge::with_session(move |session| {
            session.set_volume(volume);
            Ok(())
        })
    }

    fn playlists(&self) -> Result<Vec<PlaylistSummary>, String> {
        bridge::with_session(|session| Ok(session.plugin_playlists()))
    }

    fn playlist(&self, id: &str) -> Result<PlaylistDetail, String> {
        let id = id.to_string();
        bridge::with_session(move |session| session.plugin_playlist(&id))
    }

    fn select_playlist(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        bridge::with_session(move |session| session.plugin_select_playlist(&id))
    }

    fn toggle_shuffle(&self) -> Result<bool, String> {
        bridge::with_session(|session| Ok(session.plugin_toggle_shuffle()))
    }

    fn toggle_repeat(&self) -> Result<String, String> {
        bridge::with_session(|session| Ok(session.plugin_cycle_repeat()))
    }

    fn cover_bytes(&self, path: &str) -> Result<Option<(Vec<u8>, String)>, String> {
        let path = path.to_string();
        bridge::with_session(move |session| session.plugin_cover_bytes(&path))
    }

    fn devices(&self) -> Result<Vec<OutputDevice>, String> {
        bridge::with_session(|session| {
            session
                .engine
                .as_ref()
                .ok_or_else(|| "Audio output is unavailable".to_string())?
                .output_devices()
        })
    }

    fn add_output(&self, device_id: i32, volume: f32) -> Result<ExtraOutputInfo, String> {
        bridge::with_session(move |session| {
            session
                .engine
                .as_ref()
                .ok_or_else(|| "Audio output is unavailable".to_string())?
                .add_output(device_id, volume)
        })
    }

    fn remove_output(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        bridge::with_session(move |session| {
            session
                .engine
                .as_ref()
                .ok_or_else(|| "Audio output is unavailable".to_string())?
                .remove_output(&id)
        })
    }

    fn set_output_volume(&self, id: &str, volume: f32) -> Result<ExtraOutputInfo, String> {
        let id = id.to_string();
        bridge::with_session(move |session| {
            session
                .engine
                .as_ref()
                .ok_or_else(|| "Audio output is unavailable".to_string())?
                .set_output_volume(&id, volume)
        })
    }

    fn outputs(&self) -> Result<Vec<ExtraOutputInfo>, String> {
        bridge::with_session(|session| {
            session
                .engine
                .as_ref()
                .ok_or_else(|| "Audio output is unavailable".to_string())?
                .outputs()
        })
    }

    fn create_playlist(&self, name: &str) -> Result<PlaylistSummary, String> {
        let name = name.to_string();
        bridge::with_session(move |session| session.plugin_create_playlist(&name))
    }

    fn ensure_playlist(&self, name: &str) -> Result<PlaylistSummary, String> {
        let name = name.to_string();
        bridge::with_session(move |session| session.plugin_ensure_playlist(&name))
    }

    fn rename_playlist(&self, id: &str, name: &str) -> Result<(), String> {
        let id = id.to_string();
        let name = name.to_string();
        bridge::with_session(move |session| session.plugin_rename_playlist(&id, &name))
    }

    fn delete_playlist(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        bridge::with_session(move |session| session.plugin_delete_playlist(&id))
    }

    fn clear_playlist(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        bridge::with_session(move |session| session.plugin_clear_playlist(&id))
    }

    fn remove_tracks(&self, id: &str, paths: Vec<String>) -> Result<usize, String> {
        let id = id.to_string();
        bridge::with_session(move |session| session.plugin_remove_tracks(&id, &paths))
    }

    fn delete_tracks(&self, paths: Vec<String>) -> Result<usize, String> {
        bridge::with_session(move |session| session.plugin_delete_tracks(&paths))
    }

    fn set_liked(&self, path: &str, liked: bool) -> Result<(), String> {
        let path = path.to_string();
        bridge::with_session(move |session| session.plugin_set_liked(&path, liked))
    }

    fn tracks(&self, query: &str) -> Result<Value, String> {
        let query = query.to_string();
        bridge::with_session(move |session| {
            serde_json::to_value(session.plugin_tracks(&query)).map_err(|error| error.to_string())
        })
    }

    fn import(&self, request: ImportRequest) -> Result<Value, String> {
        let mut files = crate::ui::import::scan_paths(&request.paths);
        let labels: Vec<crate::ui::import::BlankName<'_>> = request
            .labels
            .iter()
            .map(|label| crate::ui::import::BlankName {
                path: &label.path,
                title: &label.title,
                artist: &label.artist,
                cover: &label.cover,
            })
            .collect();
        crate::ui::import::fill_blank_names(&mut files, &labels);
        bridge::with_session(move |session| session.plugin_import(&files, &request))
    }
}

pub fn ok_value() -> Value {
    json!({ "ok": true })
}
