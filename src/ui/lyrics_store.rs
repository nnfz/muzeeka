//! Lyrics cache used by track properties.
//!
//! Files live in `%APPDATA%\com.nnfz.muzeeka\lyrics` under the same key the
//! library already indexes: `{key}.match-v1.ttml`, plus `{key}.cleared`.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::ui::catalog::{self, lyrics_cache_key};

const LYRICS_API: &str = "https://lyrics-api.boidu.dev";
const LRCLIB_GET: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";
const HIT_SUFFIX: &str = ".match-v1.ttml";
const USER_AGENT: &str = "Muzeeka/0.1.0";

fn lyrics_dir() -> PathBuf {
    let dir = catalog::app_data_dir().join("lyrics");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn hit_path(key: &str) -> PathBuf {
    lyrics_dir().join(format!("{key}{HIT_SUFFIX}"))
}

fn cleared_path(key: &str) -> PathBuf {
    lyrics_dir().join(format!("{key}.cleared"))
}

fn cache_key(title: &str, artist: &str, album: Option<&str>, duration_secs: Option<u32>) -> String {
    let duration = duration_secs.filter(|value| *value > 0).unwrap_or(0) as f64;
    lyrics_cache_key(title, artist, album.unwrap_or(""), duration)
}

pub fn read_cached(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> Option<String> {
    let key = cache_key(title, artist, album, duration_secs);
    if cleared_path(&key).is_file() {
        return None;
    }
    let text = fs::read_to_string(hit_path(&key)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub fn save_text(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
    content: &str,
) -> Result<(), String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() && artist.is_empty() {
        return Err("Track has no title or artist for a lyrics cache key".into());
    }
    let ttml = normalize_lyrics_content(content)?;
    let key = cache_key(title, artist, album, duration_secs);
    fs::write(hit_path(&key), ttml).map_err(|error| format!("Failed to write lyrics: {error}"))?;
    let _ = fs::remove_file(cleared_path(&key));
    Ok(())
}

pub fn clear(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> Result<(), String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() && artist.is_empty() {
        return Err("Track has no title or artist for a lyrics cache key".into());
    }
    let key = cache_key(title, artist, album, duration_secs);
    fs::write(cleared_path(&key), "1")
        .map_err(|error| format!("Failed to clear lyrics: {error}"))?;
    let _ = fs::remove_file(hit_path(&key));
    Ok(())
}

/// Read a lyrics file and store it under the track's own cache key.
pub fn import_file(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
    path: &std::path::Path,
) -> Result<(), String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("Could not read lyrics file: {error}"))?;
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "lrc" {
        let ttml = lrc_to_ttml(&raw, duration_secs.unwrap_or(0).saturating_mul(1000))
            .ok_or_else(|| "Could not read that LRC file".to_string())?;
        return save_text(title, artist, album, duration_secs, &ttml);
    }
    save_text(title, artist, album, duration_secs, &raw)
}

/// Drop the cache and search providers. Overrides change the query only.
pub fn refetch(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
    search_title: Option<&str>,
    search_artist: Option<&str>,
    search_album: Option<&str>,
) -> Result<Option<String>, String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() && artist.is_empty() {
        return Err("Track has no title or artist for a lyrics cache key".into());
    }
    let key = cache_key(title, artist, album, duration_secs);
    let _ = fs::remove_file(hit_path(&key));
    let _ = fs::remove_file(cleared_path(&key));

    let pick = |override_value: Option<&str>, fallback: &str| -> String {
        override_value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback)
            .to_string()
    };
    let query_title = pick(search_title, title);
    let query_artist = pick(search_artist, artist);
    let query_album = search_album
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| album.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string));

    let fetched = fetch_uncached(
        &query_title,
        &query_artist,
        query_album.as_deref(),
        duration_secs.unwrap_or(0),
    )?;
    if let Some(ttml) = fetched.as_deref() {
        fs::write(hit_path(&key), ttml).map_err(|error| format!("Failed to write lyrics: {error}"))?;
    }
    Ok(fetched)
}

fn fetch_uncached(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    if let Some(ttml) = fetch_better_lyrics(title, artist, album, duration_secs)? {
        return Ok(Some(ttml));
    }
    fetch_lrclib(title, artist, duration_secs)
}

#[derive(Debug, Deserialize)]
struct LyricsApiResponse {
    ttml: Option<String>,
}

fn fetch_better_lyrics(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    if title.trim().is_empty() || artist.trim().is_empty() {
        return Ok(None);
    }
    let mut url = format!(
        "{LYRICS_API}/getLyrics?s={}&a={}",
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );
    if let Some(album) = album.filter(|album| !album.is_empty()) {
        url.push_str("&al=");
        url.push_str(&urlencoding::encode(album));
    }
    if duration_secs > 0 {
        url.push_str(&format!("&d={duration_secs}"));
    }
    let body: LyricsApiResponse = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };
    Ok(body.ttml.filter(|ttml| {
        let trimmed = ttml.trim();
        !trimmed.is_empty() && trimmed != "__NO_LYRICS__"
    }))
}

#[derive(Debug, Deserialize)]
struct LrclibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    duration: Option<f64>,
    #[serde(rename = "trackName")]
    track_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name: Option<String>,
}

fn fetch_lrclib(title: &str, artist: &str, duration_secs: u32) -> Result<Option<String>, String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() || artist.is_empty() {
        return Ok(None);
    }
    if duration_secs > 0 {
        let url = format!(
            "{LRCLIB_GET}?track_name={}&artist_name={}&duration={duration_secs}",
            urlencoding::encode(title),
            urlencoding::encode(artist),
        );
        if let Some(body) = http_get_json::<LrclibResponse>(&url)? {
            if identity_matches(
                title,
                artist,
                body.track_name.as_deref().unwrap_or(""),
                body.artist_name.as_deref().unwrap_or(""),
            ) {
                if let Some(synced) = body.synced_lyrics.filter(|text| !text.trim().is_empty()) {
                    let duration = body
                        .duration
                        .map(|value| value.round().max(1.0) as u32)
                        .unwrap_or(duration_secs);
                    if let Some(ttml) = lrc_to_ttml(&synced, duration.saturating_mul(1000)) {
                        return Ok(Some(ttml));
                    }
                }
            }
        }
    }

    let url = format!(
        "{LRCLIB_SEARCH}?track_name={}&artist_name={}",
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );
    let Some(results) = http_get_json::<Vec<LrclibResponse>>(&url)? else {
        return Ok(None);
    };
    let mut best: Option<(u32, String, u32)> = None;
    for result in results {
        let result_title = result.track_name.as_deref().unwrap_or("");
        let result_artist = result.artist_name.as_deref().unwrap_or("");
        if !identity_matches(title, artist, result_title, result_artist) {
            continue;
        }
        let Some(synced) = result
            .synced_lyrics
            .filter(|text| !text.trim().is_empty())
        else {
            continue;
        };
        let result_duration = result
            .duration
            .map(|value| value.round().max(1.0) as u32)
            .unwrap_or(duration_secs);
        if duration_secs > 0 && result_duration.abs_diff(duration_secs) > 8 {
            continue;
        }
        let distance = duration_secs.abs_diff(result_duration);
        let replace = best.as_ref().is_none_or(|(best_distance, _, _)| distance < *best_distance);
        if replace {
            best = Some((distance, synced, result_duration));
        }
    }
    let Some((_, synced, duration)) = best else {
        return Ok(None);
    };
    Ok(lrc_to_ttml(&synced, duration.saturating_mul(1000)))
}

fn identity_matches(title: &str, artist: &str, result_title: &str, result_artist: &str) -> bool {
    let title = fold(title);
    let artist = fold(artist);
    let result_title = fold(result_title);
    let result_artist = fold(result_artist);
    if title.is_empty() || artist.is_empty() || result_title.is_empty() || result_artist.is_empty()
    {
        return false;
    }
    contains_either(&result_title, &title) && contains_either(&result_artist, &artist)
}

fn contains_either(left: &str, right: &str) -> bool {
    left == right || left.contains(right) || right.contains(left)
}

fn fold(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn http_get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<Option<T>, String> {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(USER_AGENT)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = match agent.get(url).header("Accept", "application/json").call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) if matches!(code, 404 | 429 | 502 | 503 | 504) => {
            return Ok(None);
        }
        Err(error) => return Err(format!("Lyrics request failed: {error}")),
    };
    response
        .body_mut()
        .read_json::<T>()
        .map(Some)
        .map_err(|error| format!("Invalid lyrics response: {error}"))
}

pub fn normalize_lyrics_content(content: &str) -> Result<String, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err("Lyrics text is empty".into());
    }
    let lower = content.to_ascii_lowercase();
    if lower.contains("<tt") || lower.contains("<transcript") {
        return Ok(content.to_string());
    }
    Ok(plain_text_to_ttml(content))
}

fn plain_text_to_ttml(text: &str) -> String {
    let mut body = String::new();
    for line in text.split(['\n', '\r']).filter(|line| !line.trim().is_empty()) {
        let escaped = xml_escape(line.trim());
        body.push_str(&format!(
            r#"      <p begin="0.0s" end="36000.0s">{escaped}</p>
"#
        ));
    }
    if body.trim().is_empty() {
        body.push_str("      <p begin=\"0.0s\" end=\"36000.0s\"></p>\n");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body>
    <div>
{body}    </div>
  </body>
</tt>
"#
    )
}

pub(crate) fn lrc_to_ttml(lrc: &str, song_duration_ms: u32) -> Option<String> {
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let raw = raw.trim();
        let Some(open) = raw.find('[') else {
            continue;
        };
        let Some(rel_close) = raw[open..].find(']') else {
            continue;
        };
        let close = open + rel_close;
        let tag = &raw[open + 1..close];
        if tag.contains(':') && tag.chars().next().is_some_and(|ch| !ch.is_ascii_digit()) {
            continue;
        }
        let Some(start_ms) = parse_lrc_timestamp(tag) else {
            continue;
        };
        let text = raw[close + 1..].trim();
        if text.is_empty() {
            continue;
        }
        lines.push((start_ms, text.to_string()));
    }
    lines.sort_by_key(|line| line.0);
    lines.dedup_by_key(|line| line.0);
    if lines.is_empty() {
        return None;
    }
    let song_end = song_duration_ms.max(lines.last().map(|line| line.0 + 1).unwrap_or(1));
    let mut body = String::new();
    for (index, (start, text)) in lines.iter().enumerate() {
        let end = lines
            .get(index + 1)
            .map(|next| next.0)
            .unwrap_or(song_end);
        if end <= *start {
            continue;
        }
        body.push_str(&format!(
            "<p begin=\"{:.3}\" end=\"{:.3}\"><span begin=\"{:.3}\" end=\"{:.3}\">{}</span></p>",
            *start as f64 / 1000.0,
            end as f64 / 1000.0,
            *start as f64 / 1000.0,
            end as f64 / 1000.0,
            xml_escape(text)
        ));
    }
    if body.is_empty() {
        return None;
    }
    Some(format!(
        "<tt xmlns=\"http://www.w3.org/ns/ttml\" xml:lang=\"en\"><body>{body}</body></tt>"
    ))
}

fn parse_lrc_timestamp(raw: &str) -> Option<u32> {
    let parts: Vec<&str> = raw.trim().split(':').collect();
    let (hours, minutes, seconds_part) = match parts.as_slice() {
        [minutes, seconds_part] => (0, minutes.parse::<u32>().ok()?, *seconds_part),
        [hours, minutes, seconds_part] => (
            hours.parse().ok()?,
            minutes.parse().ok()?,
            *seconds_part,
        ),
        _ => return None,
    };
    let (seconds, millis) = if let Some((secs, frac)) = seconds_part.split_once('.') {
        let secs = secs.parse::<u32>().ok()?;
        let frac: String = frac.chars().take(3).collect();
        let millis = match frac.len() {
            0 => 0,
            1 => frac.parse::<u32>().ok()? * 100,
            2 => frac.parse::<u32>().ok()? * 10,
            _ => frac.parse().ok()?,
        };
        (secs, millis)
    } else {
        (seconds_part.parse().ok()?, 0)
    };
    Some(hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + millis)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{lrc_to_ttml, parse_lrc_timestamp};

    #[test]
    fn parses_lrc_timestamp() {
        assert_eq!(parse_lrc_timestamp("00:02.12"), Some(2120));
        assert_eq!(parse_lrc_timestamp("01:02:03"), Some(3_723_000));
    }

    #[test]
    fn converts_lrc_to_ttml() {
        let lrc = "[00:02.12] Hello world\n[00:05.00] Second line";
        let ttml = lrc_to_ttml(lrc, 10_000).expect("ttml");
        assert!(ttml.contains("begin=\"2.120\""));
        assert!(ttml.contains("Hello world"));
        assert!(ttml.contains("Second line"));
    }
}
