// MusicBrainz + Cover Art Archive lookup for album cover URLs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

const USER_AGENT: &str = "Muzeeka/0.1.0 (https://github.com/muzeeka/muzeeka)";
const MB_BASE: &str = "https://musicbrainz.org/ws/2";
const CAA_BASE: &str = "https://coverartarchive.org";

static RATE_LIMIT: Mutex<Option<Instant>> = Mutex::new(None);
static COVER_CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

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

fn cache() -> std::sync::MutexGuard<'static, Option<HashMap<String, Option<String>>>> {
    let mut guard = COVER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

fn cache_key(artist: &str, title: &str, album: Option<&str>) -> String {
    format!(
        "{}|{}|{}",
        artist.to_lowercase(),
        title.to_lowercase(),
        album.unwrap_or("").to_lowercase()
    )
}

fn mb_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn throttle_mb() {
    let mut guard = RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = *guard {
        let elapsed = last.elapsed();
        if elapsed < Duration::from_millis(1100) {
            std::thread::sleep(Duration::from_millis(1100) - elapsed);
        }
    }
    *guard = Some(Instant::now());
}

fn http_get_json<T: serde::de::DeserializeOwned>(url: &str) -> Option<T> {
    let mut response = ::ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .call()
        .ok()?;
    response.body_mut().read_json::<T>().ok()
}

fn release_mbid(artist: &str, title: &str, album: Option<&str>) -> Option<String> {
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

    throttle_mb();
    let url = format!(
        "{}/recording?query={}&fmt=json&limit=1",
        MB_BASE,
        urlencoding::encode(&query)
    );

    let search: MbRecordingSearch = http_get_json(&url)?;
    let recording = search.recordings?.into_iter().next()?;
    let release = recording.releases?.into_iter().next()?;
    if release.id.is_empty() {
        None
    } else {
        Some(release.id)
    }
}

fn cover_from_release(mbid: &str) -> Option<String> {
    throttle_mb();
    let url = format!("{}/release/{}", CAA_BASE, mbid);
    let payload: CaaRelease = http_get_json(&url)?;

    let images = payload.images?;
    let front = images
        .iter()
        .find(|image| image.front)
        .or_else(|| images.first())?;

    front
        .image
        .clone()
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
}

/// Look up a Cover Art Archive image URL for a track.
pub fn lookup_cover_url(artist: &str, title: &str, album: Option<&str>) -> Option<String> {
    let artist = artist.trim();
    let title = title.trim();
    if artist.is_empty() || title.is_empty() {
        return None;
    }

    let key = cache_key(artist, title, album);
    {
        let mut guard = cache();
        if let Some(map) = guard.as_mut() {
            if let Some(cached) = map.get(&key) {
                return cached.clone();
            }
        }
    }

    let result = release_mbid(artist, title, album).and_then(|mbid| cover_from_release(&mbid));

    let mut guard = cache();
    if let Some(map) = guard.as_mut() {
        map.insert(key, result.clone());
    }

    result
}