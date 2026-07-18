//! Unison (unison.boidu.dev) — crowdsourced lyrics fallback for Better Lyrics.
//! Public read API; no API key required for GET.

use serde::Deserialize;

use crate::lrc::lrc_to_ttml;
use crate::lyrics::http_get_json;

const UNISON_API: &str = "https://unison.boidu.dev";

#[derive(Debug, Deserialize)]
struct UnisonEnvelope<T> {
    success: bool,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct UnisonLyrics {
    lyrics: Option<String>,
    format: Option<String>,
    duration: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct UnisonSearchHit {
    id: Option<u64>,
    duration: Option<u32>,
    format: Option<String>,
    #[serde(rename = "matchScore")]
    match_score: Option<f64>,
    #[serde(rename = "effectiveScore")]
    effective_score: Option<f64>,
}

fn lyrics_to_ttml(lyrics: &str, format: &str, duration_secs: u32) -> Option<String> {
    let lyrics = lyrics.trim();
    if lyrics.is_empty() {
        return None;
    }

    match format.to_ascii_lowercase().as_str() {
        "ttml" => {
            if lyrics.contains("<p") || lyrics.contains("<tt") {
                Some(lyrics.to_string())
            } else {
                None
            }
        }
        "lrc" => lrc_to_ttml(lyrics, duration_secs.saturating_mul(1000)),
        // Unsynced plain text is not useful for the timed lyrics UI.
        _ => None,
    }
}

fn try_unison_get(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    let mut url = format!(
        "{UNISON_API}/lyrics?song={}&artist={}",
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );

    if let Some(album) = album.filter(|value| !value.is_empty()) {
        url.push_str("&album=");
        url.push_str(&urlencoding::encode(album));
    }

    if duration_secs > 0 {
        url.push_str(&format!("&duration={duration_secs}"));
    }

    let body: UnisonEnvelope<UnisonLyrics> = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    if !body.success {
        return Ok(None);
    }

    let Some(data) = body.data else {
        return Ok(None);
    };

    let Some(lyrics) = data.lyrics.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };

    let format = data.format.as_deref().unwrap_or("ttml");
    let resolved_duration = data.duration.filter(|value| *value > 0).unwrap_or(duration_secs);

    Ok(lyrics_to_ttml(&lyrics, format, resolved_duration))
}

fn try_unison_by_id(id: u64, duration_secs: u32) -> Result<Option<String>, String> {
    let url = format!("{UNISON_API}/lyrics/{id}");
    let body: UnisonEnvelope<UnisonLyrics> = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    if !body.success {
        return Ok(None);
    }

    let Some(data) = body.data else {
        return Ok(None);
    };

    let Some(lyrics) = data.lyrics.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };

    let format = data.format.as_deref().unwrap_or("ttml");
    let resolved_duration = data.duration.filter(|value| *value > 0).unwrap_or(duration_secs);

    Ok(lyrics_to_ttml(&lyrics, format, resolved_duration))
}

fn fetch_unison_search(
    title: &str,
    artist: &str,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    let url = format!(
        "{UNISON_API}/lyrics/search?song={}&artist={}",
        urlencoding::encode(title),
        urlencoding::encode(artist),
    );

    let body: UnisonEnvelope<Vec<UnisonSearchHit>> = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    if !body.success {
        return Ok(None);
    }

    let hits = body.data.unwrap_or_default();
    if hits.is_empty() {
        return Ok(None);
    }

    // Prefer timed formats, then duration closeness, then community score.
    let mut ranked: Vec<(i32, f64, f64, u64)> = Vec::new();
    for hit in hits {
        let Some(id) = hit.id else {
            continue;
        };

        let format = hit.format.as_deref().unwrap_or("").to_ascii_lowercase();
        let format_rank = match format.as_str() {
            "ttml" => 0,
            "lrc" => 1,
            _ => 2,
        };
        if format_rank == 2 {
            continue;
        }

        let result_duration = hit.duration.unwrap_or(duration_secs);
        let distance = if duration_secs > 0 {
            (result_duration as i32 - duration_secs as i32).abs()
        } else {
            0
        };

        let match_score = hit.match_score.unwrap_or(0.0);
        let effective = hit.effective_score.unwrap_or(0.0);

        ranked.push((format_rank + distance * 10, -match_score, -effective, id));
    }

    ranked.sort_by(|a, b| {
        use std::cmp::Ordering;
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
            .then(a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal))
    });

    for (_, _, _, id) in ranked.into_iter().take(3) {
        if let Some(ttml) = try_unison_by_id(id, duration_secs)? {
            return Ok(Some(ttml));
        }
    }

    Ok(None)
}

/// Fetch lyrics from Unison and normalize to TTML for the player UI.
pub fn fetch_unison_ttml(
    title: &str,
    artist: &str,
    album: Option<&str>,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() || artist.is_empty() {
        return Ok(None);
    }

    if let Some(ttml) = try_unison_get(title, artist, album, duration_secs)? {
        return Ok(Some(ttml));
    }

    // Exact get often 404s on sparse corpus; search is more forgiving.
    fetch_unison_search(title, artist, duration_secs)
}

#[cfg(test)]
mod tests {
    use super::{fetch_unison_ttml, lyrics_to_ttml};

    #[test]
    fn ttml_passthrough() {
        let ttml = r#"<tt><body><div><p begin="1.0" end="2.0"><span>Hi</span></p></div></body></tt>"#;
        let out = lyrics_to_ttml(ttml, "ttml", 120).expect("ttml");
        assert!(out.contains("<p"));
    }

    #[test]
    fn plain_is_skipped() {
        assert!(lyrics_to_ttml("just words", "plain", 120).is_none());
    }

    #[test]
    #[ignore = "hits live Unison API"]
    fn fetch_give_it_up_via_unison() {
        let ttml = fetch_unison_ttml("Give It Up", "Don Toliver", None, 131)
            .expect("unison fetch should not error")
            .expect("give it up should be available on unison");
        assert!(ttml.contains("<p"), "expected TTML paragraphs in response");
    }
}
