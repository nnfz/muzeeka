// imgBB image upload for Discord Rich Presence cover art fallback.
// Imgur no longer issues new API keys (registration page redirects away).
//
// API key: put `IMGBB_API_KEY=...` in the project-root `.env` (or export it).
// Free keys: https://api.imgbb.com/ — never commit real keys.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use ureq::Agent;

use crate::cover_url_cache::{self, CacheLookup};

const IMGBB_UPLOAD_URL: &str = "https://api.imgbb.com/1/upload";
const DISK_KEY_PREFIX: &str = "imgbb:";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_REQUEST_GAP: Duration = Duration::from_millis(1200);
/// Session upload cache (path hash → URL / None). Bounded MRU.
const MEMORY_CACHE_CAP: usize = 128;

/// In-memory MRU: most recently used at the end.
static UPLOAD_CACHE: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());
static RATE_LIMIT: Mutex<Option<Instant>> = Mutex::new(None);
static HTTP_AGENT: OnceLock<Agent> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct ImgbbResponse {
    success: Option<bool>,
    status: Option<u16>,
    data: Option<ImgbbData>,
}

#[derive(Debug, Deserialize)]
struct ImgbbData {
    url: Option<String>,
    display_url: Option<String>,
}

fn http_agent() -> &'static Agent {
    HTTP_AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build();
        config.into()
    })
}

/// Load API key from the environment only — never hardcode secrets in the binary.
fn api_key() -> Option<String> {
    std::env::var("IMGBB_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn memory_get(key: &str) -> Option<Option<String>> {
    let mut cache = UPLOAD_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = cache.iter().position(|(k, _)| k == key) {
        let entry = cache.remove(pos);
        let value = entry.1.clone();
        cache.push(entry);
        return Some(value);
    }
    None
}

fn memory_put(key: String, value: Option<String>) {
    let mut cache = UPLOAD_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _)| k != &key);
    if cache.len() >= MEMORY_CACHE_CAP {
        cache.remove(0);
    }
    cache.push((key, value));
}

fn cache_key(path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let modified = std::fs::metadata(&canonical)
        .and_then(|meta| meta.modified())
        .ok()
        .map(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let mut hasher = DefaultHasher::new();
    canonical.to_string_lossy().hash(&mut hasher);
    modified.hash(&mut hasher);
    Some(format!("{:016x}", hasher.finish()))
}

fn throttle() {
    let mut guard = RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = *guard {
        let elapsed = last.elapsed();
        if elapsed < MIN_REQUEST_GAP {
            std::thread::sleep(MIN_REQUEST_GAP - elapsed);
        }
    }
    *guard = Some(Instant::now());
}

/// Upload a local image file to imgBB and return a public HTTPS URL.
pub fn upload_image(path: &Path) -> Option<String> {
    let api_key = api_key()?;

    if !path.is_file() {
        return None;
    }

    let key = cache_key(path)?;
    if let Some(cached) = memory_get(&key) {
        return cached;
    }

    let disk_key = format!("{DISK_KEY_PREFIX}{key}");
    match cover_url_cache::lookup(&disk_key) {
        CacheLookup::Url(url) => {
            memory_put(key, Some(url.clone()));
            return Some(url);
        }
        CacheLookup::Failed => {
            memory_put(key, None);
            return None;
        }
        CacheLookup::Miss => {}
    }

    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    // base64 expands ~1.33×; fine for cover art (usually ≪ 2 MB).
    let encoded = STANDARD.encode(bytes);
    let body = format!(
        "key={}&image={}",
        urlencoding::encode(&api_key),
        urlencoding::encode(&encoded)
    );

    throttle();

    let result = match http_agent()
        .post(IMGBB_UPLOAD_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body.as_bytes())
    {
        Ok(mut response) => {
            let payload: ImgbbResponse = match response.body_mut().read_json() {
                Ok(p) => p,
                Err(error) => {
                    eprintln!("imgBB response parse failed: {error}");
                    // Treat as hard failure for this file (bad payload / oversized / rejected).
                    memory_put(key.clone(), None);
                    cover_url_cache::set_failed(&disk_key);
                    return None;
                }
            };
            if payload.success == Some(true) || payload.status == Some(200) {
                payload
                    .data
                    .and_then(|data| data.display_url.or(data.url))
                    .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            } else {
                None
            }
        }
        Err(error) => {
            // Transient network / timeout — memory-only miss so next session can retry.
            eprintln!("imgBB upload failed: {error}");
            memory_put(key, None);
            return None;
        }
    };

    if let Some(url) = result.as_deref() {
        cover_url_cache::set(&disk_key, url);
    } else {
        // API rejected the image (or returned no URL) — skip re-upload next launch.
        cover_url_cache::set_failed(&disk_key);
    }

    memory_put(key, result.clone());
    result
}
