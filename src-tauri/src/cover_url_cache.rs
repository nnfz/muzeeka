// Persistent cache of Discord RPC cover URLs (MusicBrainz CAA + imgBB).
// Avoids re-querying / re-uploading the same art across app restarts.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

static CACHE: OnceLock<Mutex<CoverUrlCache>> = OnceLock::new();

struct CoverUrlCache {
    path: PathBuf,
    entries: HashMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DiskFormat {
    #[serde(default)]
    entries: HashMap<String, String>,
}

/// Initialize the on-disk cache under the app data directory.
pub fn init(app_data_dir: PathBuf) {
    let path = app_data_dir.join("discord_cover_urls.json");
    let entries = load(&path);
    let _ = CACHE.set(Mutex::new(CoverUrlCache { path, entries }));
}

/// Return a previously stored public HTTPS cover URL, if any.
pub fn get(key: &str) -> Option<String> {
    let cache = CACHE.get()?;
    let guard = cache.lock();
    guard.entries.get(key).cloned().filter(|url| is_http_url(url))
}

/// Store a successful cover URL under `key` and flush to disk.
pub fn set(key: &str, url: &str) {
    if key.is_empty() || !is_http_url(url) {
        return;
    }
    let Some(cache) = CACHE.get() else {
        return;
    };
    let mut guard = cache.lock();
    if guard.entries.get(key).map(String::as_str) == Some(url) {
        return;
    }
    guard.entries.insert(key.to_string(), url.to_string());
    if let Err(error) = save(&guard.path, &guard.entries) {
        eprintln!("Discord cover URL cache save failed: {error}");
    }
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

fn load(path: &PathBuf) -> HashMap<String, String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<DiskFormat>(&raw) {
        Ok(disk) => disk
            .entries
            .into_iter()
            .filter(|(_, url)| is_http_url(url))
            .collect(),
        Err(error) => {
            eprintln!("Discord cover URL cache parse failed: {error}");
            HashMap::new()
        }
    }
}

fn save(path: &PathBuf, entries: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create cover URL cache dir: {e}"))?;
    }

    let payload = DiskFormat {
        entries: entries.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|e| format!("Failed to serialize cover URL cache: {e}"))?;

    let tmp_path = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp_path)
        .map_err(|e| format!("Failed to create temporary cover URL cache: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("Failed to write temporary cover URL cache: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("Failed to flush temporary cover URL cache: {e}"))?;
    drop(file);

    fs::rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed to replace cover URL cache: {e}")
    })?;

    Ok(())
}
