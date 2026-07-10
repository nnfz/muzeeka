// imgBB image upload for Discord Rich Presence cover art fallback.
// Imgur no longer issues new API keys (registration page redirects away).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;

/// imgBB API key — get one at https://api.imgbb.com/ (free, takes ~1 minute).
pub const IMGBB_API_KEY: &str = "fc29c1706c2f35e26c49a805cb6effdd";

const IMGBB_UPLOAD_URL: &str = "https://api.imgbb.com/1/upload";

static UPLOAD_CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);
static RATE_LIMIT: Mutex<Option<Instant>> = Mutex::new(None);

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

fn cache() -> std::sync::MutexGuard<'static, Option<HashMap<String, Option<String>>>> {
    let mut guard = UPLOAD_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

fn cache_key(path: &Path) -> Option<String> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let modified = fs::metadata(&canonical)
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
        if elapsed < Duration::from_millis(1200) {
            std::thread::sleep(Duration::from_millis(1200) - elapsed);
        }
    }
    *guard = Some(Instant::now());
}

/// Upload a local image file to imgBB and return a public HTTPS URL.
pub fn upload_image(path: &Path) -> Option<String> {
    let api_key = IMGBB_API_KEY.trim();
    if api_key.is_empty() {
        return None;
    }

    if !path.is_file() {
        return None;
    }

    let key = cache_key(path)?;
    {
        let mut guard = cache();
        if let Some(map) = guard.as_mut() {
            if let Some(cached) = map.get(&key) {
                return cached.clone();
            }
        }
    }

    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let encoded = STANDARD.encode(bytes);
    let body = format!(
        "key={}&image={}",
        urlencoding::encode(api_key),
        urlencoding::encode(&encoded)
    );

    throttle();

    let mut response = ::ureq::post(IMGBB_UPLOAD_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body.as_bytes())
        .ok()?;

    let payload: ImgbbResponse = response.body_mut().read_json().ok()?;
    let result = if payload.success == Some(true) || payload.status == Some(200) {
        payload
            .data
            .and_then(|data| data.display_url.or(data.url))
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
    } else {
        None
    };

    let mut guard = cache();
    if let Some(map) = guard.as_mut() {
        map.insert(key, result.clone());
    }

    result
}