// Persistent cache of Discord RPC cover URLs (MusicBrainz CAA + imgBB).
// Avoids re-querying / re-uploading the same art across app restarts.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Negative-cache marker for uploads that failed (not a real URL).
pub const FAILED_MARKER: &str = "__failed__";

static CACHE: OnceLock<Mutex<CoverUrlCache>> = OnceLock::new();

struct CoverUrlCache {
    path: PathBuf,
    entries: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct DiskFormat {
    #[serde(default)]
    entries: HashMap<String, String>,
}

/// Serialize view — avoids cloning the map just to write JSON.
#[derive(Serialize)]
struct DiskFormatRef<'a> {
    entries: &'a HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    Url(String),
    /// Previous attempt failed; callers should not retry until the entry is cleared.
    Failed,
    Miss,
}

/// Initialize the on-disk cache under the app data directory.
pub fn init(app_data_dir: PathBuf) {
    let path = app_data_dir.join("discord_cover_urls.json");
    let entries = load(&path);
    let _ = CACHE.set(Mutex::new(CoverUrlCache { path, entries }));
}

/// Return a previously stored public HTTPS cover URL, if any.
pub fn get(key: &str) -> Option<String> {
    match lookup(key) {
        CacheLookup::Url(url) => Some(url),
        CacheLookup::Failed | CacheLookup::Miss => None,
    }
}

/// Full lookup including negative-cache hits.
pub fn lookup(key: &str) -> CacheLookup {
    let Some(cache) = CACHE.get() else {
        return CacheLookup::Miss;
    };
    let guard = cache.lock();
    match guard.entries.get(key).map(String::as_str) {
        Some(url) if is_http_url(url) => CacheLookup::Url(url.to_string()),
        Some(v) if is_failed_marker(v) => CacheLookup::Failed,
        _ => CacheLookup::Miss,
    }
}

/// Store a successful cover URL under `key` and flush to disk.
pub fn set(key: &str, url: &str) {
    if key.is_empty() || !is_http_url(url) {
        return;
    }
    write_entry(key, url);
}

/// Remember a failed upload so we do not hammer the network every session.
pub fn set_failed(key: &str) {
    if key.is_empty() {
        return;
    }
    write_entry(key, FAILED_MARKER);
}

fn write_entry(key: &str, value: &str) {
    let Some(cache) = CACHE.get() else {
        return;
    };

    // Mutate under the lock, then release before disk I/O so get() isn't blocked on fsync.
    let (path, entries) = {
        let mut guard = cache.lock();
        if guard.entries.get(key).map(String::as_str) == Some(value) {
            return;
        }
        guard.entries.insert(key.to_string(), value.to_string());
        (guard.path.clone(), guard.entries.clone())
    };

    if let Err(error) = save(&path, &entries) {
        eprintln!("Discord cover URL cache save failed: {error}");
    }
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

fn is_failed_marker(value: &str) -> bool {
    value == FAILED_MARKER
}

fn is_persistable_value(value: &str) -> bool {
    is_http_url(value) || is_failed_marker(value)
}

fn load(path: &Path) -> HashMap<String, String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<DiskFormat>(&raw) {
        Ok(disk) => disk
            .entries
            .into_iter()
            .filter(|(_, value)| is_persistable_value(value))
            .collect(),
        Err(error) => {
            eprintln!("Discord cover URL cache parse failed: {error}");
            HashMap::new()
        }
    }
}

fn save(path: &Path, entries: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create cover URL cache dir: {e}"))?;
    }

    let payload = DiskFormatRef { entries };
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
