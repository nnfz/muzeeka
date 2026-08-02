//! Unison (unison.boidu.dev) — crowdsourced lyrics fallback for Better Lyrics.
//! Public read API; no API key required for GET.

use serde::Deserialize;

use crate::lrc::lrc_to_ttml;
use crate::lyrics::{
    duration_close_enough, http_get_json, track_identity_matches,
};

const UNISON_API: &str = "https://unison.boidu.dev";
/// Unison search is very fuzzy; require duration within this window when known.
const SEARCH_DURATION_TOLERANCE_SECS: u32 = 8;
/// How many search hits to materialize via `/lyrics/{id}` (each is an extra HTTP call).
const MAX_BY_ID_FETCHES: usize = 1;

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
    song: Option<String>,
    artist: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UnisonSearchHit {
    id: Option<u64>,
    song: Option<String>,
    artist: Option<String>,
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

/// Shared path for GET /lyrics and GET /lyrics/{id} responses.
fn extract_matching_lyrics(
    body: UnisonEnvelope<UnisonLyrics>,
    query_title: &str,
    query_artist: &str,
    duration_secs: u32,
) -> Option<String> {
    if !body.success {
        return None;
    }

    let data = body.data?;

    // Unison is fuzzy and often returns a completely different track.
    let result_title = data.song.as_deref().unwrap_or("");
    let result_artist = data.artist.as_deref().unwrap_or("");
    if result_title.is_empty()
        || result_artist.is_empty()
        || !track_identity_matches(query_title, query_artist, result_title, result_artist)
    {
        return None;
    }

    if !duration_close_enough(duration_secs, data.duration, SEARCH_DURATION_TOLERANCE_SECS) {
        return None;
    }

    let lyrics = data.lyrics.filter(|value| !value.trim().is_empty())?;
    let format = data.format.as_deref().unwrap_or("ttml");
    let resolved_duration = data
        .duration
        .filter(|value| *value > 0)
        .unwrap_or(duration_secs);

    lyrics_to_ttml(&lyrics, format, resolved_duration)
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

    Ok(extract_matching_lyrics(body, title, artist, duration_secs))
}

fn try_unison_by_id(
    id: u64,
    query_title: &str,
    query_artist: &str,
    duration_secs: u32,
) -> Result<Option<String>, String> {
    let url = format!("{UNISON_API}/lyrics/{id}");
    let body: UnisonEnvelope<UnisonLyrics> = match http_get_json(&url)? {
        Some(body) => body,
        None => return Ok(None),
    };

    Ok(extract_matching_lyrics(
        body,
        query_title,
        query_artist,
        duration_secs,
    ))
}

/// Rank key for search hits (lexicographic ascending = better first).
/// - format_rank: 0 = TTML, 1 = LRC (unsynced already filtered out)
/// - distance: |Δduration| in seconds, 0..=SEARCH_DURATION_TOLERANCE_SECS
/// - -match_score / -effective: higher API scores sort first (negated for ascending)
#[derive(Debug, Clone, Copy)]
struct SearchRank {
    format_rank: i32,
    duration_distance: i32,
    neg_match_score: f64,
    neg_effective: f64,
    id: u64,
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
    // Always require title+artist identity — Unison search returns unrelated tracks.
    let mut ranked: Vec<SearchRank> = Vec::new();
    for hit in hits {
        let Some(id) = hit.id else {
            continue;
        };

        let result_title = hit.song.as_deref().unwrap_or("");
        let result_artist = hit.artist.as_deref().unwrap_or("");
        if result_title.is_empty()
            || result_artist.is_empty()
            || !track_identity_matches(title, artist, result_title, result_artist)
        {
            continue;
        }

        // matchScore is often missing; when present, ignore clearly weak hits.
        if let Some(score) = hit.match_score {
            if score < 0.55 {
                continue;
            }
        }

        let format = hit.format.as_deref().unwrap_or("").to_ascii_lowercase();
        let format_rank = match format.as_str() {
            "ttml" => 0,
            "lrc" => 1,
            _ => continue, // plain / unknown — not useful for timed UI
        };

        if !duration_close_enough(duration_secs, hit.duration, SEARCH_DURATION_TOLERANCE_SECS) {
            continue;
        }

        let result_duration = hit.duration.unwrap_or(duration_secs);
        let duration_distance = if duration_secs > 0 {
            result_duration.abs_diff(duration_secs) as i32
        } else {
            0
        };

        let match_score = hit.match_score.unwrap_or(0.0);
        let effective = hit.effective_score.unwrap_or(0.0);

        ranked.push(SearchRank {
            format_rank,
            duration_distance,
            neg_match_score: -match_score,
            neg_effective: -effective,
            id,
        });
    }

    ranked.sort_by(|a, b| {
        use std::cmp::Ordering;
        // format first (ttml < lrc), then closer duration; scores break ties.
        // No *10 weighting: fields are compared in order and do not need packing.
        a.format_rank
            .cmp(&b.format_rank)
            .then(a.duration_distance.cmp(&b.duration_distance))
            .then(
                a.neg_match_score
                    .partial_cmp(&b.neg_match_score)
                    .unwrap_or(Ordering::Equal),
            )
            .then(
                a.neg_effective
                    .partial_cmp(&b.neg_effective)
                    .unwrap_or(Ordering::Equal),
            )
    });

    // Cap extra HTTP: only materialize the best search hit.
    for hit in ranked.into_iter().take(MAX_BY_ID_FETCHES) {
        if let Some(ttml) = try_unison_by_id(hit.id, title, artist, duration_secs)? {
            return Ok(Some(ttml));
        }
    }

    Ok(None)
}

/// Fetch lyrics from Unison and normalize to TTML for the player UI.
///
/// At most two HTTP calls for a clean miss: one GET (optional) + one search.
/// A hit may add one more `/lyrics/{id}` fetch.
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

    // Skip vague GET without duration — it is extremely fuzzy and often wastes a round-trip.
    if duration_secs > 0 {
        if let Some(ttml) = try_unison_get(title, artist, album, duration_secs)? {
            return Ok(Some(ttml));
        }
    }

    // Exact get often 404s on sparse corpus; search is more forgiving but validated.
    fetch_unison_search(title, artist, duration_secs)
}

#[cfg(test)]
mod tests {
    use super::{fetch_unison_ttml, lyrics_to_ttml};
    use crate::lyrics::track_identity_matches;

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
    fn rejects_unrelated_identity() {
        assert!(!track_identity_matches(
            "Погано",
            "Пу Пу Пу",
            "Сильно",
            "СЛИВНЯКА"
        ));
        assert!(!track_identity_matches(
            "Погано",
            "Пу Пу Пу",
            "風と行く道",
            "大原ゆい子"
        ));
    }

    #[test]
    #[ignore = "hits live Unison API"]
    fn fetch_give_it_up_via_unison() {
        let ttml = fetch_unison_ttml("Give It Up", "Don Toliver", None, 131)
            .expect("unison fetch should not error")
            .expect("give it up should be available on unison");
        assert!(ttml.contains("<p"), "expected TTML paragraphs in response");
    }

    #[test]
    #[ignore = "hits live Unison API"]
    fn fetch_pogano_has_no_false_positive() {
        let ttml = fetch_unison_ttml("Погано", "Пу Пу Пу", None, 180)
            .expect("unison fetch should not error");
        assert!(ttml.is_none(), "must not invent lyrics for unknown tracks");
    }
}
