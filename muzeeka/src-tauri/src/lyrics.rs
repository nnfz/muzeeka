// Lyrics providers — Better Lyrics API + LRCLIB fallback, with on-disk cache.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const LYRICS_API: &str = "https://lyrics-api.boidu.dev";
const USER_AGENT: &str = "Muzeeka/0.1.0 (https://github.com/muzeeka/muzeeka)";
const NO_LYRICS_SENTINEL: &str = "__NO_LYRICS__";
const MISS_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Bump when new lyrics providers are added so stale negative cache entries are ignored.
const MISS_CACHE_VERSION: &str = "unison-v2";

static LYRICS_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

const LYRICS_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

static LYRICS_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(LYRICS_HTTP_TIMEOUT))
        .timeout_recv_body(Some(LYRICS_HTTP_TIMEOUT))
        .user_agent(USER_AGENT)
        .build();
    ureq::Agent::new_with_config(config)
});

#[derive(Debug, Deserialize)]
struct LyricsApiResponse {
    ttml: Option<String>,
}

/// Initialize on-disk lyrics cache under the app data directory.
pub fn init_lyrics_cache(app_data_dir: PathBuf) {
    let lyrics = app_data_dir.join("lyrics");
    let _ = fs::create_dir_all(&lyrics);
    let _ = LYRICS_CACHE_DIR.set(lyrics);
}

fn is_soft_http_status(code: u16) -> bool {
    matches!(code, 401 | 404 | 429 | 502 | 503 | 504)
}

pub(crate) fn http_get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<Option<T>, String> {
    let mut response = match LYRICS_AGENT
        .get(url)
        .header("Accept", "application/json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) if is_soft_http_status(code) => return Ok(None),
        Err(error) => return Err(format!("Lyrics request failed: {error}")),
    };

    let status = response.status();
    if is_soft_http_status(status.as_u16()) {
        return Ok(None);
    }

    if !status.is_success() {
        return Err(format!("Lyrics API returned HTTP {status}"));
    }

    response
        .body_mut()
        .read_json::<T>()
        .map_err(|e| format!("Invalid lyrics response: {e}"))
        .map(Some)
}

fn cache_key(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> String {
    let album = album.unwrap_or("");
    let duration = duration_secs
        .filter(|value| *value > 0)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let normalized = format!(
        "{}\0{}\0{}\0{}",
        title.trim().to_lowercase(),
        artist.trim().to_lowercase(),
        album.trim().to_lowercase(),
        duration
    );

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn hit_cache_path(key: &str) -> Option<PathBuf> {
    LYRICS_CACHE_DIR
        .get()
        .map(|dir| dir.join(format!("{key}.ttml")))
}

fn miss_cache_path(key: &str) -> Option<PathBuf> {
    LYRICS_CACHE_DIR
        .get()
        .map(|dir| dir.join(format!("{key}.{MISS_CACHE_VERSION}.miss")))
}

fn read_cached_hit(key: &str) -> Option<String> {
    let hit_path = hit_cache_path(key)?;
    if !hit_path.is_file() {
        return None;
    }

    let ttml = fs::read_to_string(&hit_path).ok()?;
    let ttml = ttml.trim();
    if ttml.is_empty() {
        return None;
    }

    Some(ttml.to_string())
}

fn miss_cache_is_fresh(key: &str) -> bool {
    let Some(miss_path) = miss_cache_path(key) else {
        return false;
    };
    if !miss_path.is_file() {
        return false;
    }

    let Ok(modified) = fs::metadata(&miss_path).and_then(|meta| meta.modified()) else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return false;
    };

    if age > MISS_CACHE_TTL {
        let _ = fs::remove_file(&miss_path);
        return false;
    }

    true
}

fn write_cached_hit(key: &str, ttml: &str) -> Result<(), String> {
    let hit_path = hit_cache_path(key).ok_or_else(|| "Lyrics cache unavailable".to_string())?;
    fs::write(&hit_path, ttml).map_err(|e| format!("Failed to write lyrics cache: {e}"))?;

    if let Some(miss_path) = miss_cache_path(key) {
        let _ = fs::remove_file(miss_path);
    }

    Ok(())
}

fn write_cached_miss(key: &str) -> Result<(), String> {
    let miss_path = miss_cache_path(key).ok_or_else(|| "Lyrics cache unavailable".to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System time error: {e}"))?
        .as_secs();
    fs::write(&miss_path, now.to_string())
        .map_err(|e| format!("Failed to write lyrics miss cache: {e}"))?;
    Ok(())
}

fn fetch_from_better_lyrics(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> Result<Option<String>, String> {
    let mut url = format!(
        "{}/getLyrics?s={}&a={}",
        LYRICS_API,
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );

    if let Some(album) = album.filter(|value| !value.is_empty()) {
        url.push_str("&al=");
        url.push_str(&urlencoding::encode(album));
    }

    if let Some(duration) = duration_secs.filter(|value| *value > 0) {
        url.push_str(&format!("&d={duration}"));
    }

    let body: LyricsApiResponse = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    Ok(body
        .ttml
        .filter(|ttml| !ttml.trim().is_empty() && ttml != NO_LYRICS_SENTINEL))
}

fn fetch_uncached(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> Result<Option<String>, String> {
    // 1) Better Lyrics API — word-level TTML when cache-hit (or with API key).
    if let Some(ttml) = fetch_from_better_lyrics(title, artist, album, duration_secs)? {
        return Ok(Some(ttml));
    }

    let duration = duration_secs.unwrap_or(0);

    // 2) LRCLIB — free LRC line-sync fallback.
    if let Some(ttml) = crate::lrclib::fetch_lrclib_ttml(title, artist, duration)? {
        return Ok(Some(ttml));
    }

    // 3) Unison — crowdsourced public read API (ttml/lrc).
    if let Some(ttml) = crate::unison::fetch_unison_ttml(title, artist, album, duration)? {
        return Ok(Some(ttml));
    }

    Ok(None)
}

pub fn fetch_lyrics_ttml(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> Result<Option<String>, String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() && artist.is_empty() {
        return Ok(None);
    }

    let album = album.map(str::trim).filter(|value| !value.is_empty());
    let key = cache_key(title, artist, album, duration_secs);

    if let Some(cached) = read_cached_hit(&key) {
        return Ok(Some(cached));
    }

    if miss_cache_is_fresh(&key) {
        return Ok(None);
    }

    let fetched = fetch_uncached(title, artist, album, duration_secs)?;

    match fetched.as_deref() {
        Some(ttml) => {
            let _ = write_cached_hit(&key, ttml);
        }
        None => {
            let _ = write_cached_miss(&key);
        }
    }

    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::{cache_key, fetch_lyrics_ttml};

    #[test]
    fn cache_key_is_stable_for_same_track() {
        let a = cache_key("Hotline Bling", "Drake", None, Some(267));
        let b = cache_key(" hotline bling ", " DRAKE ", None, Some(267));
        assert_eq!(a, b);
    }

    #[test]
    fn fetch_hotline_bling_has_lines() {
        let result = fetch_lyrics_ttml("Hotline Bling", "Drake", None, Some(267))
            .expect("lyrics fetch should not error");
        let ttml = result.expect("hotline bling should be available");
        assert!(ttml.contains("<p"), "expected TTML paragraphs in response");
    }
}