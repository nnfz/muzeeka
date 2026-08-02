// MusicBrainz + Cover Art Archive lookup for album cover URLs.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use ureq::Agent;

use crate::cover_url_cache::{self, CacheLookup};

const USER_AGENT: &str = "Muzeeka/0.1.0 (https://github.com/muzeeka/muzeeka)";
const MB_BASE: &str = "https://musicbrainz.org/ws/2";
const CAA_BASE: &str = "https://coverartarchive.org";
const DISK_KEY_PREFIX: &str = "mb:";
/// MusicBrainz asks for ~1 req/s with a descriptive User-Agent.
const MB_MIN_GAP: Duration = Duration::from_millis(1100);
const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const MEMORY_CACHE_CAP: usize = 128;

/// Only MusicBrainz WS is rate-limited under this timer — CAA is a different host.
static MB_RATE_LIMIT: Mutex<Option<Instant>> = Mutex::new(None);
/// Session MRU: key → cover URL (None = known miss this session, not a rate-limit).
static COVER_CACHE: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());
static HTTP_AGENT: OnceLock<Agent> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct MbRecordingSearch {
    recordings: Option<Vec<MbRecording>>,
}

#[derive(Debug, Deserialize)]
struct MbRecording {
    releases: Option<Vec<MbReleaseRef>>,
}

#[derive(Debug, Deserialize)]
struct MbReleaseRef {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CaaRelease {
    images: Option<Vec<CaaImage>>,
}

#[derive(Debug, Deserialize)]
struct CaaImage {
    #[serde(default)]
    front: bool,
    image: Option<String>,
}

/// Distinguish "nothing found" from "don't cache this, try later".
enum LookupOutcome {
    Found(String),
    /// 404 / empty / no match — safe to remember for this session.
    Miss,
    /// 429/503 or network — do not poison in-memory or disk miss caches.
    Transient,
}

fn http_agent() -> &'static Agent {
    HTTP_AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .build();
        config.into()
    })
}

fn cache_key(artist: &str, title: &str, album: Option<&str>) -> String {
    format!(
        "{}|{}|{}",
        artist.to_lowercase(),
        title.to_lowercase(),
        album.unwrap_or("").to_lowercase()
    )
}

fn memory_get(key: &str) -> Option<Option<String>> {
    let mut cache = COVER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = cache.iter().position(|(k, _)| k == key) {
        let entry = cache.remove(pos);
        let value = entry.1.clone();
        cache.push(entry);
        return Some(value);
    }
    None
}

fn memory_put(key: String, value: Option<String>) {
    let mut cache = COVER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _)| k != &key);
    if cache.len() >= MEMORY_CACHE_CAP {
        cache.remove(0);
    }
    cache.push((key, value));
}

fn mb_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn throttle_musicbrainz() {
    let mut guard = MB_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = *guard {
        let elapsed = last.elapsed();
        if elapsed < MB_MIN_GAP {
            std::thread::sleep(MB_MIN_GAP - elapsed);
        }
    }
    *guard = Some(Instant::now());
}

fn http_get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<Option<T>, LookupOutcome> {
    match http_agent()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .call()
    {
        Ok(mut response) => {
            let status = response.status();
            let code = status.as_u16();
            if matches!(code, 429 | 503) {
                eprintln!("[musicbrainz] rate limited or unavailable (HTTP {code}) for {url}");
                return Err(LookupOutcome::Transient);
            }
            if code == 404 {
                return Ok(None);
            }
            if !status.is_success() {
                eprintln!("[musicbrainz] HTTP {code} for {url}");
                return Err(LookupOutcome::Transient);
            }
            match response.body_mut().read_json::<T>() {
                Ok(body) => Ok(Some(body)),
                Err(error) => {
                    eprintln!("[musicbrainz] JSON parse failed: {error}");
                    Err(LookupOutcome::Transient)
                }
            }
        }
        Err(ureq::Error::StatusCode(code)) if matches!(code, 429 | 503) => {
            eprintln!("[musicbrainz] rate limited or unavailable (HTTP {code}) for {url}");
            Err(LookupOutcome::Transient)
        }
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(error) => {
            eprintln!("[musicbrainz] request failed: {error}");
            Err(LookupOutcome::Transient)
        }
    }
}

fn release_mbid(artist: &str, title: &str, album: Option<&str>) -> Result<Option<String>, LookupOutcome> {
    let query = match album.filter(|value| !value.trim().is_empty()) {
        Some(album) => format!(
            r#"recording:"{}" AND artist:"{}" AND release:"{}""#,
            mb_escape(title),
            mb_escape(artist),
            mb_escape(album)
        ),
        None => format!(
            r#"recording:"{}" AND artist:"{}""#,
            mb_escape(title),
            mb_escape(artist)
        ),
    };

    throttle_musicbrainz();
    let url = format!(
        "{}/recording?query={}&fmt=json&limit=1",
        MB_BASE,
        urlencoding::encode(&query)
    );

    let search: MbRecordingSearch = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };
    let recording = match search.recordings.and_then(|r| r.into_iter().next()) {
        Some(r) => r,
        None => return Ok(None),
    };
    let release = match recording.releases.and_then(|r| r.into_iter().next()) {
        Some(r) => r,
        None => return Ok(None),
    };
    if release.id.is_empty() {
        Ok(None)
    } else {
        Ok(Some(release.id))
    }
}

/// Cover Art Archive — separate host; no MusicBrainz 1 req/s throttle.
fn cover_from_release(mbid: &str) -> Result<Option<String>, LookupOutcome> {
    let url = format!("{}/release/{}", CAA_BASE, mbid);
    let payload: CaaRelease = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    let images = match payload.images {
        Some(images) if !images.is_empty() => images,
        _ => return Ok(None),
    };
    let front = images
        .iter()
        .find(|image| image.front)
        .or_else(|| images.first());

    let Some(front) = front else {
        return Ok(None);
    };

    Ok(front
        .image
        .clone()
        .filter(|u| u.starts_with("http://") || u.starts_with("https://")))
}

fn resolve_cover(artist: &str, title: &str, album: Option<&str>) -> LookupOutcome {
    match release_mbid(artist, title, album) {
        Ok(Some(mbid)) => match cover_from_release(&mbid) {
            Ok(Some(url)) => LookupOutcome::Found(url),
            Ok(None) => LookupOutcome::Miss,
            Err(outcome) => outcome,
        },
        Ok(None) => LookupOutcome::Miss,
        Err(outcome) => outcome,
    }
}

/// Look up a Cover Art Archive image URL for a track.
pub fn lookup_cover_url(artist: &str, title: &str, album: Option<&str>) -> Option<String> {
    let artist = artist.trim();
    let title = title.trim();
    if artist.is_empty() || title.is_empty() {
        return None;
    }

    let key = cache_key(artist, title, album);
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
            // Hard miss from a previous confirmed failure (not rate-limit).
            memory_put(key, None);
            return None;
        }
        CacheLookup::Miss => {}
    }

    match resolve_cover(artist, title, album) {
        LookupOutcome::Found(url) => {
            cover_url_cache::set(&disk_key, &url);
            memory_put(key, Some(url.clone()));
            Some(url)
        }
        LookupOutcome::Miss => {
            // Session-only miss — do not write disk negative cache for MB
            // (track may get a release later; 429 must never become a permanent miss).
            memory_put(key, None);
            None
        }
        LookupOutcome::Transient => {
            // Leave memory empty so a later track / retry can try again.
            None
        }
    }
}
