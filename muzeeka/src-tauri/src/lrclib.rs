//! LRCLIB API client — fallback lyrics source (LRC, converted to TTML).

use serde::Deserialize;

use crate::lrc::lrc_to_ttml;
use crate::lyrics::http_get_json;

const LRCLIB_GET: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";

#[derive(Debug, Deserialize)]
struct LrclibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    duration: Option<f64>,
}

fn synced_lyrics_to_ttml(synced: &str, duration_secs: u32) -> Option<String> {
    let synced = synced.trim();
    if synced.is_empty() {
        return None;
    }

    let song_duration_ms = duration_secs.saturating_mul(1000);
    lrc_to_ttml(synced, song_duration_ms)
}

fn duration_candidates(duration_secs: u32) -> Vec<u32> {
    let mut candidates = vec![duration_secs];
    for delta in [1u32, 2] {
        if duration_secs > delta {
            candidates.push(duration_secs - delta);
        }
        candidates.push(duration_secs.saturating_add(delta));
    }
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

fn try_lrclib_get(title: &str, artist: &str, duration_secs: u32) -> Result<Option<String>, String> {
    let url = format!(
        "{LRCLIB_GET}?track_name={}&artist_name={}&duration={duration_secs}",
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );

    let body: LrclibResponse = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    let Some(synced) = body.synced_lyrics else {
        return Ok(None);
    };

    let resolved_duration = body
        .duration
        .map(|value| value.round().max(1.0) as u32)
        .unwrap_or(duration_secs);

    Ok(synced_lyrics_to_ttml(&synced, resolved_duration))
}

fn fetch_lrclib_get(title: &str, artist: &str, duration_secs: u32) -> Result<Option<String>, String> {
    for duration in duration_candidates(duration_secs) {
        if let Some(ttml) = try_lrclib_get(title, artist, duration)? {
            return Ok(Some(ttml));
        }
    }

    Ok(None)
}

fn fetch_lrclib_search(
    title: &str,
    artist: &str,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    let url = format!(
        "{LRCLIB_SEARCH}?track_name={}&artist_name={}",
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );

    let results: Vec<LrclibResponse> = match http_get_json(&url)? {
        Some(results) => results,
        None => return Ok(None),
    };

    let mut best: Option<(i32, String, u32)> = None;

    for result in results {
        let Some(synced) = result
            .synced_lyrics
            .filter(|lyrics| !lyrics.trim().is_empty())
        else {
            continue;
        };

        let result_duration = result
            .duration
            .map(|value| value.round().max(1.0) as u32)
            .unwrap_or(duration_secs);

        let distance = if duration_secs > 0 {
            (result_duration as i32 - duration_secs as i32).abs()
        } else {
            0
        };

        let replace = match &best {
            None => true,
            Some((best_distance, _, _)) => distance < *best_distance,
        };

        if replace {
            best = Some((distance, synced, result_duration));
        }
    }

    let Some((_, synced, resolved_duration)) = best else {
        return Ok(None);
    };

    Ok(synced_lyrics_to_ttml(&synced, resolved_duration))
}

pub fn fetch_lrclib_ttml(
    title: &str,
    artist: &str,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() || artist.is_empty() {
        return Ok(None);
    }

    if duration_secs > 0 {
        if let Some(ttml) = fetch_lrclib_get(title, artist, duration_secs)? {
            return Ok(Some(ttml));
        }
    }

    fetch_lrclib_search(title, artist, duration_secs)
}

#[cfg(test)]
mod tests {
    use super::{duration_candidates, fetch_lrclib_ttml};

    #[test]
    fn duration_candidates_include_tolerance() {
        let candidates = duration_candidates(267);
        assert!(candidates.contains(&267));
        assert!(candidates.contains(&266));
        assert!(candidates.contains(&268));
        assert!(candidates.len() <= 5);
    }

    #[test]
    #[ignore = "hits live LRCLib API; slow and occasionally times out"]
    fn fetch_hotline_bling_via_lrclib() {
        let ttml = fetch_lrclib_ttml("Hotline Bling", "Drake", 266)
            .expect("lrclib fetch should not error")
            .expect("hotline bling should be available on lrclib");
        assert!(ttml.contains("<p"), "expected TTML paragraphs in response");
    }
}