use std::fs;
use std::path::PathBuf;

use crate::ui::catalog::{self, lyrics_cache_key};
pub(crate) use crate::ui::lyrics_sources::lrc_to_ttml;

const HIT_SUFFIX: &str = ".match-v1.ttml";

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

pub fn import_file(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
    path: &std::path::Path,
) -> Result<(), String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("Could not read lyrics file: {error}"))?;
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

pub fn load_or_fetch(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: Option<u32>,
) -> Option<String> {
    if title.trim().is_empty() && artist.trim().is_empty() {
        return None;
    }
    let key = cache_key(title, artist, album, duration_secs);
    if cleared_path(&key).is_file() {
        return None;
    }
    if let Ok(text) = fs::read_to_string(hit_path(&key)) {
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    let album = album.map(str::trim).filter(|album| !album.is_empty());
    let fetched = fetch_uncached(
        title.trim(),
        artist.trim(),
        album,
        duration_secs.unwrap_or(0),
    )
    .ok()
    .flatten()?;
    let _ = fs::write(hit_path(&key), &fetched);
    Some(fetched)
}

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
        .or_else(|| {
            album
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });

    let fetched = fetch_uncached(
        &query_title,
        &query_artist,
        query_album.as_deref(),
        duration_secs.unwrap_or(0),
    )?;
    if let Some(ttml) = fetched.as_deref() {
        fs::write(hit_path(&key), ttml)
            .map_err(|error| format!("Failed to write lyrics: {error}"))?;
    }
    Ok(fetched)
}

fn fetch_uncached(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    crate::ui::lyrics_sources::fetch(crate::ui::lyrics_sources::Query {
        title: title.to_string(),
        artist: artist.to_string(),
        album: album.map(str::to_string),
        duration_secs,
    })
}

pub(crate) fn identity_matches(
    title: &str,
    artist: &str,
    result_title: &str,
    result_artist: &str,
) -> bool {
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

pub(crate) fn fold(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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

pub(crate) fn plain_text_to_ttml(text: &str) -> String {
    let mut body = String::new();
    for line in text
        .split(['\n', '\r'])
        .filter(|line| !line.trim().is_empty())
    {
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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
