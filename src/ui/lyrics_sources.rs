use std::sync::{mpsc, Arc, LazyLock, OnceLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::ui::lyrics::{self, SyncType};
use crate::ui::lyrics_store::{fold, identity_matches, plain_text_to_ttml};

const USER_AGENT: &str = "Muzeeka/0.1.0";
const ITUNES_SEARCH: &str = "https://itunes.apple.com/search";
const PAXSENIX: &str = "https://lyrics.paxsenix.org/apple-music/lyrics";
const LRC_RED: &str = "https://lrc.red/api/v1";
const LRC_RED_FILES: &str = "https://lrc.red/";
const ZARZ: &str = "https://api.zarz.moe/apple-music/lyrics";
const LYRICS_PLUS_HOSTS: [&str; 3] = [
    "https://lyricsplus.prjktla.my.id",
    "https://lyricsplus.binimum.org",
    "https://lyricsplus.prjktla.workers.dev",
];
const LYRICS_PLUS_SOURCES: &str = "apple,lyricsplus,musixmatch-word,qq,musixmatch";
const LRCLIB_GET: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";

const DURATION_SLACK_SECS: u32 = 3;
const TIMEOUT_SECS: u64 = 15;
const ZARZ_TIMEOUT_SECS: u64 = 25;
const SOURCE_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Tier {
    Word,
    Line,
    Plain,
}

#[derive(Clone, Debug)]
pub(crate) struct Query {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: u32,
}

type Outcome = Result<Option<(Tier, String)>, String>;

pub(crate) fn fetch(query: Query) -> Result<Option<String>, String> {
    if query.title.trim().is_empty() || query.artist.trim().is_empty() {
        return Ok(None);
    }
    let apple_id: Arc<OnceLock<Option<u64>>> = Arc::new(OnceLock::new());
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    for index in 0..SOURCE_COUNT {
        let tx = tx.clone();
        let query = query.clone();
        let apple_id = apple_id.clone();
        std::thread::spawn(move || {
            let outcome = run_source(index, &query, &apple_id);
            let _ = tx.send((index, outcome));
        });
    }
    drop(tx);
    let mut outcomes: Vec<Option<Outcome>> = (0..SOURCE_COUNT).map(|_| None).collect();
    while let Ok((index, outcome)) = rx.recv() {
        outcomes[index] = Some(outcome);
        if let Some(answer) = decide(&outcomes) {
            return answer;
        }
    }
    decide(&outcomes).unwrap_or(Ok(None))
}

fn decide(outcomes: &[Option<Outcome>]) -> Option<Result<Option<String>, String>> {
    let best = outcomes
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| match outcome {
            Some(Ok(Some((tier, text)))) => Some(((*tier, index), text)),
            _ => None,
        })
        .min_by_key(|(key, _)| *key);
    let mut pending = outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| outcome.is_none())
        .map(|(index, _)| index);
    match best {
        Some((key, text)) => {
            let beatable = pending.any(|index| (Tier::Word, index) < key);
            (!beatable).then(|| Ok(Some(text.clone())))
        }
        None => {
            if pending.next().is_some() {
                return None;
            }
            let all_failed = outcomes
                .iter()
                .all(|outcome| matches!(outcome, Some(Err(_))));
            let first_error = outcomes.iter().find_map(|outcome| match outcome {
                Some(Err(error)) => Some(error.clone()),
                _ => None,
            });
            Some(match (all_failed, first_error) {
                (true, Some(error)) => Err(error),
                _ => Ok(None),
            })
        }
    }
}

fn run_source(index: usize, query: &Query, apple_id: &OnceLock<Option<u64>>) -> Outcome {
    let ttml = match index {
        0 => match apple_track_id(query, apple_id) {
            Some(id) => apple_lyrics(&format!("{PAXSENIX}?id={id}"), TIMEOUT_SECS)?,
            None => None,
        },
        1 => lrc_red(query)?,
        2 => match apple_track_id(query, apple_id) {
            Some(id) => apple_lyrics(&format!("{ZARZ}?id={id}&v=1"), ZARZ_TIMEOUT_SECS)?,
            None => None,
        },
        3 => lyrics_plus(query)?,
        _ => lrclib(query)?,
    };
    Ok(ttml.and_then(|ttml| classify(&ttml, query.duration_secs).map(|tier| (tier, ttml))))
}

fn classify(ttml: &str, duration_secs: u32) -> Option<Tier> {
    let parsed = lyrics::parse(ttml, duration_secs.saturating_mul(1000))?;
    if parsed
        .lines
        .iter()
        .all(|line| line.instrumental || line.words.trim().is_empty())
    {
        return None;
    }
    Some(match parsed.sync {
        SyncType::Rich => Tier::Word,
        SyncType::Line => Tier::Line,
        SyncType::None => Tier::Plain,
    })
}

// ---------------------------------------------------------------- HTTP

enum Fetched {
    Body(String),
    Missing,
    Busy,
}

fn get_text(url: &str, timeout_secs: u64) -> Result<Option<String>, String> {
    Ok(match get_raw(url, timeout_secs)? {
        Fetched::Body(body) => Some(body),
        Fetched::Missing | Fetched::Busy => None,
    })
}

fn get_raw(url: &str, timeout_secs: u64) -> Result<Fetched, String> {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .user_agent(USER_AGENT)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) if code == 429 || code >= 500 => {
            return Ok(Fetched::Busy);
        }
        Err(ureq::Error::StatusCode(code)) if matches!(code, 400 | 403 | 404 | 410 | 422) => {
            return Ok(Fetched::Missing);
        }
        Err(error) => return Err(format!("Lyrics request failed: {error}")),
    };
    response
        .body_mut()
        .read_to_string()
        .map(Fetched::Body)
        .map_err(|error| format!("Lyrics response failed: {error}"))
}

fn get_json<T: DeserializeOwned>(url: &str, timeout_secs: u64) -> Result<Option<T>, String> {
    let Some(body) = get_text(url, timeout_secs)? else {
        return Ok(None);
    };
    if body.trim_start().starts_with('<') {
        return Ok(None);
    }
    serde_json::from_str(&body)
        .map(Some)
        .map_err(|error| format!("Invalid lyrics response: {error}"))
}

fn enc(value: &str) -> String {
    urlencoding::encode(value.trim()).into_owned()
}

fn duration_distance(wanted: u32, found: Option<u32>) -> Option<u32> {
    match found {
        Some(found) if wanted > 0 && found > 0 => {
            let distance = wanted.abs_diff(found);
            (distance <= DURATION_SLACK_SECS).then_some(distance)
        }
        _ => Some(0),
    }
}

fn ms(value: Option<f64>) -> Option<u64> {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as u64)
}

// ---------------------------------------------------------------- Apple Music

#[derive(Deserialize)]
struct ItunesResults {
    #[serde(default)]
    results: Vec<ItunesTrack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesTrack {
    track_id: Option<u64>,
    track_name: Option<String>,
    artist_name: Option<String>,
    collection_name: Option<String>,
    track_time_millis: Option<f64>,
}

fn apple_track_id(query: &Query, cell: &OnceLock<Option<u64>>) -> Option<u64> {
    *cell.get_or_init(|| itunes_lookup(query).ok().flatten())
}

fn itunes_lookup(query: &Query) -> Result<Option<u64>, String> {
    let title = query.title.trim();
    let artist = query.artist.trim();
    let mut searches = vec![
        (format!("{title} {artist}"), ""),
        (title.to_owned(), "&attribute=songTerm"),
    ];
    if let Some(album) = query
        .album
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        searches.push((format!("{title} {album}"), ""));
    }
    let mut best: Option<((bool, bool, u32), u64)> = None;
    let mut last_error = None;
    for country in ["", "&country=ru"] {
        for (term, attribute) in &searches {
            let url = format!(
                "{ITUNES_SEARCH}?term={}&entity=song&limit=50{attribute}{country}",
                enc(term)
            );
            match get_json::<ItunesResults>(&url, 10) {
                Ok(Some(found)) => {
                    if let Some(candidate) = itunes_best(query, found) {
                        if best.is_none_or(|current| candidate.0 < current.0) {
                            best = Some(candidate);
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
            if best.is_some_and(|(key, _)| key == (false, false, 0)) {
                return Ok(best.map(|(_, id)| id));
            }
        }
    }
    match (best, last_error) {
        (None, Some(error)) => Err(error),
        (best, _) => Ok(best.map(|(_, id)| id)),
    }
}

fn itunes_best(query: &Query, found: ItunesResults) -> Option<((bool, bool, u32), u64)> {
    let title = fold(&query.title);
    let album = query.album.as_deref().map(fold);
    found
        .results
        .into_iter()
        .filter_map(|track| {
            let id = track.track_id?;
            let name = track.track_name.as_deref()?;
            let artist = track.artist_name.as_deref()?;
            if !identity_matches(&query.title, &query.artist, name, artist) {
                return None;
            }
            let secs = ms(track.track_time_millis).map(|ms| ((ms + 500) / 1000) as u32);
            let distance = duration_distance(query.duration_secs, secs)?;
            let title_differs = fold(name) != title;
            let album_differs = match (&album, track.collection_name.as_deref()) {
                (Some(wanted), Some(found)) => fold(found) != *wanted,
                _ => false,
            };
            Some(((title_differs, album_differs, distance), id))
        })
        .min_by_key(|(key, _)| *key)
}

#[derive(Deserialize)]
struct AppleLyrics {
    #[serde(rename = "type")]
    kind: Option<String>,
    content: Option<Vec<AppleLine>>,
    plain: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleLine {
    timestamp: Option<f64>,
    endtime: Option<f64>,
    text: Option<AppleText>,
    background_text: Option<Vec<AppleSyllable>>,
    opposite_turn: Option<bool>,
    agent: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AppleText {
    Syllables(Vec<AppleSyllable>),
    Plain(String),
}

#[derive(Deserialize)]
struct AppleSyllable {
    text: Option<String>,
    timestamp: Option<f64>,
    endtime: Option<f64>,
    part: Option<bool>,
}

fn apple_lyrics(url: &str, timeout_secs: u64) -> Result<Option<String>, String> {
    const ATTEMPTS: u64 = 3;
    for attempt in 1..=ATTEMPTS {
        let busy = match get_raw(url, timeout_secs)? {
            Fetched::Missing => return Ok(None),
            Fetched::Busy => true,
            Fetched::Body(body) => {
                let trimmed = body.trim_start();
                if trimmed.starts_with('<') {
                    return Ok(None);
                }
                let value: serde_json::Value = serde_json::from_str(trimmed)
                    .map_err(|error| format!("Invalid lyrics response: {error}"))?;
                if value.get("error").and_then(|e| e.as_bool()) == Some(true) {
                    let message = value.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    if !message.to_lowercase().contains("busy") {
                        return Ok(None);
                    }
                    true
                } else {
                    let lyrics: AppleLyrics = serde_json::from_value(value)
                        .map_err(|error| format!("Invalid lyrics response: {error}"))?;
                    return Ok(apple_to_ttml(lyrics));
                }
            }
        };
        if busy && attempt < ATTEMPTS {
            std::thread::sleep(Duration::from_millis(1000 * attempt));
        }
    }
    Ok(None)
}

fn apple_words(syllables: &[AppleSyllable]) -> Vec<Word> {
    syllables
        .iter()
        .filter_map(|syllable| {
            let text = syllable.text.as_deref().filter(|text| !text.is_empty())?;
            let start = ms(syllable.timestamp)?;
            let end = ms(syllable.endtime).unwrap_or(start);
            let gap = if syllable.part == Some(true) { "" } else { " " };
            Some(Word {
                start_ms: start,
                end_ms: end,
                text: format!("{text}{gap}"),
            })
        })
        .collect()
}

fn apple_to_ttml(body: AppleLyrics) -> Option<String> {
    let kind = body.kind.unwrap_or_default().to_ascii_lowercase();
    let content = body.content.unwrap_or_default();
    if kind == "unsynced" || content.is_empty() {
        return body.plain.as_deref().and_then(plain_to_ttml);
    }
    let word_synced = kind == "syllable" || kind == "word";
    let lines: Vec<Line> = content
        .into_iter()
        .filter_map(|line| {
            let (words, text) = match line.text {
                Some(AppleText::Syllables(syllables)) => {
                    let words = apple_words(&syllables);
                    let text = words.iter().map(|word| word.text.as_str()).collect();
                    (words, text)
                }
                Some(AppleText::Plain(text)) => (Vec::new(), text),
                None => (Vec::new(), String::new()),
            };
            let background = apple_words(line.background_text.as_deref().unwrap_or_default());
            let start = ms(line.timestamp).or_else(|| words.first().map(|word| word.start_ms))?;
            let end = ms(line.endtime)
                .or_else(|| words.last().map(|word| word.end_ms))
                .unwrap_or(start);
            let agent = line
                .agent
                .or_else(|| (line.opposite_turn == Some(true)).then(|| "v2".to_string()));
            Some(Line {
                start_ms: start,
                end_ms: end,
                text,
                words: if word_synced { words } else { Vec::new() },
                background: if word_synced { background } else { Vec::new() },
                agent,
            })
        })
        .collect();
    lines_to_ttml(&lines)
}

// ---------------------------------------------------------------- lrc.red

#[derive(Deserialize)]
struct LrcRedSearch {
    #[serde(default)]
    results: Vec<LrcRedHit>,
}

#[derive(Deserialize)]
struct LrcRedHit {
    track_name: Option<String>,
    artist_name: Option<String>,
    duration: Option<f64>,
    #[serde(rename = "lyricsUrl")]
    lyrics_url: Option<String>,
    timing_type: Option<String>,
}

fn lrc_red(query: &Query) -> Result<Option<String>, String> {
    let term = format!("{} {}", query.title.trim(), query.artist.trim());
    let url = format!("{LRC_RED}?q={}", enc(&term));
    let Some(found) = get_json::<LrcRedSearch>(&url, TIMEOUT_SECS)? else {
        return Ok(None);
    };
    let best = found
        .results
        .into_iter()
        .filter_map(|hit| {
            let lyrics_url = hit
                .lyrics_url
                .filter(|url| url.starts_with(LRC_RED_FILES))?;
            if !identity_matches(
                &query.title,
                &query.artist,
                hit.track_name.as_deref().unwrap_or(""),
                hit.artist_name.as_deref().unwrap_or(""),
            ) {
                return None;
            }
            let secs = hit.duration.map(|secs| secs.round().max(0.0) as u32);
            let distance = duration_distance(query.duration_secs, secs)?;
            let rank = match hit.timing_type.as_deref() {
                Some("word") => 0,
                Some("line") => 1,
                _ => 2,
            };
            Some(((rank, distance), lyrics_url))
        })
        .min_by_key(|(key, _)| *key);
    let Some((_, lyrics_url)) = best else {
        return Ok(None);
    };
    Ok(get_text(&lyrics_url, TIMEOUT_SECS)?.filter(|text| text.contains("<tt")))
}

// ---------------------------------------------------------------- LyricsPlus

#[derive(Deserialize)]
struct PlusLyrics {
    #[serde(rename = "type")]
    kind: Option<String>,
    lyrics: Option<Vec<PlusLine>>,
}

#[derive(Deserialize)]
struct PlusLine {
    time: Option<f64>,
    duration: Option<f64>,
    text: Option<String>,
    syllabus: Option<Vec<PlusSyllable>>,
    element: Option<PlusElement>,
}

#[derive(Deserialize)]
struct PlusSyllable {
    time: Option<f64>,
    duration: Option<f64>,
    text: Option<String>,
    #[serde(rename = "isBackground")]
    is_background: Option<bool>,
}

#[derive(Deserialize)]
struct PlusElement {
    singer: Option<String>,
}

fn lyrics_plus(query: &Query) -> Result<Option<String>, String> {
    let mut params = format!(
        "title={}&artist={}&source={LYRICS_PLUS_SOURCES}",
        enc(&query.title),
        enc(&query.artist),
    );
    if let Some(album) = query
        .album
        .as_deref()
        .filter(|album| !album.trim().is_empty())
    {
        params.push_str(&format!("&album={}", enc(album)));
    }
    if query.duration_secs > 0 {
        params.push_str(&format!("&duration={}", query.duration_secs));
    }
    let mut last = Ok(None);
    for host in LYRICS_PLUS_HOSTS {
        match get_json::<PlusLyrics>(&format!("{host}/v2/lyrics/get?{params}"), TIMEOUT_SECS) {
            Ok(Some(body)) => return Ok(plus_to_ttml(body)),
            other => last = other.map(|_| None),
        }
    }
    last
}

fn plus_to_ttml(body: PlusLyrics) -> Option<String> {
    let kind = body.kind.unwrap_or_default().to_ascii_lowercase();
    let lines = body.lyrics.unwrap_or_default();
    let word_synced = kind == "word" || kind == "syllable";
    if !word_synced && kind != "line" {
        let text: Vec<String> = lines.into_iter().filter_map(|line| line.text).collect();
        return plain_to_ttml(&text.join("\n"));
    }
    let lines: Vec<Line> = lines
        .into_iter()
        .filter_map(|line| {
            let start = ms(line.time)?;
            let end = start + ms(line.duration).unwrap_or(0);
            let mut words = Vec::new();
            let mut background = Vec::new();
            for syllable in line.syllabus.unwrap_or_default() {
                let (Some(text), Some(word_start)) = (syllable.text, ms(syllable.time)) else {
                    continue;
                };
                let word = Word {
                    start_ms: word_start,
                    end_ms: word_start + ms(syllable.duration).unwrap_or(0),
                    text,
                };
                if syllable.is_background == Some(true) {
                    background.push(word);
                } else {
                    words.push(word);
                }
            }
            Some(Line {
                start_ms: start,
                end_ms: end,
                text: line.text.unwrap_or_default(),
                words: if word_synced { words } else { Vec::new() },
                background: if word_synced { background } else { Vec::new() },
                agent: line.element.and_then(|element| element.singer),
            })
        })
        .collect();
    lines_to_ttml(&lines)
}

// ---------------------------------------------------------------- LRCLIB

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibTrack {
    track_name: Option<String>,
    artist_name: Option<String>,
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

fn lrclib(query: &Query) -> Result<Option<String>, String> {
    let title = enc(&query.title);
    let artist = enc(&query.artist);
    let mut best: Option<((Tier, u32), String)> = None;
    fn consider(best: &mut Option<((Tier, u32), String)>, track: LrclibTrack, query: &Query) {
        if let Some(candidate) = lrclib_candidate(track, query) {
            if best.as_ref().is_none_or(|(key, _)| candidate.0 < *key) {
                *best = Some(candidate);
            }
        }
    }
    if query.duration_secs > 0 {
        let mut url = format!(
            "{LRCLIB_GET}?track_name={title}&artist_name={artist}&duration={}",
            query.duration_secs
        );
        if let Some(album) = query
            .album
            .as_deref()
            .filter(|album| !album.trim().is_empty())
        {
            url.push_str(&format!("&album_name={}", enc(album)));
        }
        if let Some(track) = get_json::<LrclibTrack>(&url, TIMEOUT_SECS)? {
            consider(&mut best, track, query);
        }
    }
    if !best
        .as_ref()
        .is_some_and(|((tier, _), _)| *tier <= Tier::Line)
    {
        let url = format!("{LRCLIB_SEARCH}?track_name={title}&artist_name={artist}");
        for track in get_json::<Vec<LrclibTrack>>(&url, TIMEOUT_SECS)?.unwrap_or_default() {
            consider(&mut best, track, query);
        }
    }
    Ok(best.map(|(_, ttml)| ttml))
}

fn lrclib_candidate(track: LrclibTrack, query: &Query) -> Option<((Tier, u32), String)> {
    if track.instrumental == Some(true)
        || !identity_matches(
            &query.title,
            &query.artist,
            track.track_name.as_deref().unwrap_or(""),
            track.artist_name.as_deref().unwrap_or(""),
        )
    {
        return None;
    }
    let secs = track.duration.map(|secs| secs.round().max(0.0) as u32);
    let distance = duration_distance(query.duration_secs, secs)?;
    let song_ms = secs
        .filter(|secs| *secs > 0)
        .unwrap_or(query.duration_secs)
        .saturating_mul(1000);
    let ttml = track
        .synced_lyrics
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| lrc_to_ttml(text, song_ms))
        .or_else(|| track.plain_lyrics.as_deref().and_then(plain_to_ttml))?;
    let tier = classify(&ttml, query.duration_secs)?;
    Some(((tier, distance), ttml))
}

// ---------------------------------------------------------------- TTML output

pub(crate) struct Word {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

pub(crate) struct Line {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Vec<Word>,
    pub background: Vec<Word>,
    pub agent: Option<String>,
}

fn secs(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn push_words(out: &mut String, words: &[Word]) {
    for word in words.iter().filter(|word| !word.text.trim().is_empty()) {
        out.push_str(&format!(
            "<span begin=\"{}\" end=\"{}\">{}</span>",
            secs(word.start_ms),
            secs(word.end_ms.max(word.start_ms)),
            xml_escape(&word.text),
        ));
    }
}

pub(crate) fn lines_to_ttml(lines: &[Line]) -> Option<String> {
    let mut body = String::new();
    for line in lines {
        let has_words = line
            .words
            .iter()
            .chain(&line.background)
            .any(|word| !word.text.trim().is_empty());
        if !has_words && line.text.trim().is_empty() {
            continue;
        }
        let end = line.end_ms.max(line.start_ms + 1);
        body.push_str(&format!(
            "<p begin=\"{}\" end=\"{}\"",
            secs(line.start_ms),
            secs(end)
        ));
        if let Some(agent) = line.agent.as_deref().filter(|agent| !agent.is_empty()) {
            body.push_str(&format!(" ttm:agent=\"{}\"", xml_escape(agent)));
        }
        body.push('>');
        if has_words {
            push_words(&mut body, &line.words);
            if !line.background.is_empty() {
                body.push_str("<span ttm:role=\"x-bg\">");
                push_words(&mut body, &line.background);
                body.push_str("</span>");
            }
        } else {
            body.push_str(&xml_escape(line.text.trim()));
        }
        body.push_str("</p>");
    }
    if body.is_empty() {
        return None;
    }
    Some(format!(
        "<tt xmlns=\"http://www.w3.org/ns/ttml\" xmlns:ttm=\"http://www.w3.org/ns/ttml#metadata\" xml:lang=\"en\"><body><div>{body}</div></body></tt>"
    ))
}

fn plain_to_ttml(text: &str) -> Option<String> {
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !(line.starts_with('[') && line.ends_with(']')))
        .collect();
    (!kept.is_empty()).then(|| plain_text_to_ttml(&kept.join("\n")))
}

// ---------------------------------------------------------------- LRC

static WORD_TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<(\d+:\d+(?:[.:]\d+)?)>").expect("regex"));

pub(crate) fn lrc_to_ttml(lrc: &str, song_duration_ms: u32) -> Option<String> {
    let mut rows: Vec<(u64, String)> = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some(close) = tail.find(']') else {
                break;
            };
            let Some(start) = parse_lrc_timestamp(&tail[..close]) else {
                break;
            };
            stamps.push(start as u64);
            rest = tail[close + 1..].trim_start();
        }
        if stamps.is_empty() || WORD_TAG.replace_all(rest, "").trim().is_empty() {
            continue;
        }
        for start in stamps {
            rows.push((start, rest.trim_end().to_string()));
        }
    }
    rows.sort_by_key(|row| row.0);
    rows.dedup_by_key(|row| row.0);
    let last_start = rows.last()?.0;
    let song_end = if song_duration_ms > 0 {
        (song_duration_ms as u64).max(last_start + 1)
    } else {
        last_start + 5000
    };
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, (start, text))| {
            let end = rows.get(index + 1).map(|next| next.0).unwrap_or(song_end);
            Line {
                start_ms: *start,
                end_ms: end,
                text: WORD_TAG.replace_all(text, "").trim().to_string(),
                words: enhanced_words(text, end),
                background: Vec::new(),
                agent: None,
            }
        })
        .collect();
    lines_to_ttml(&lines)
}

fn enhanced_words(text: &str, line_end: u64) -> Vec<Word> {
    let marks: Vec<(u64, usize, usize)> = WORD_TAG
        .captures_iter(text)
        .filter_map(|capture| {
            let whole = capture.get(0)?;
            let start = parse_lrc_timestamp(capture.get(1)?.as_str())?;
            Some((start as u64, whole.start(), whole.end()))
        })
        .collect();
    let mut words = Vec::new();
    for (index, (start, _, text_from)) in marks.iter().enumerate() {
        let next = marks.get(index + 1);
        let text_to = next.map(|mark| mark.1).unwrap_or(text.len());
        let piece = &text[*text_from..text_to];
        if piece.trim().is_empty() {
            continue;
        }
        let end = next.map(|mark| mark.0).unwrap_or(line_end).max(start + 1);
        words.push(Word {
            start_ms: *start,
            end_ms: end,
            text: piece.to_string(),
        });
    }
    words
}

pub(crate) fn parse_lrc_timestamp(raw: &str) -> Option<u32> {
    let parts: Vec<&str> = raw.trim().split(':').collect();
    let (hours, minutes, seconds_part) = match parts.as_slice() {
        [minutes, seconds_part] => (0, minutes.parse::<u32>().ok()?, *seconds_part),
        [hours, minutes, seconds_part] => {
            (hours.parse().ok()?, minutes.parse().ok()?, *seconds_part)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(tier: Tier) -> Option<Outcome> {
        Some(Ok(Some((tier, format!("{tier:?}")))))
    }

    #[test]
    fn parses_lrc_timestamp() {
        assert_eq!(parse_lrc_timestamp("00:02.12"), Some(2120));
        assert_eq!(parse_lrc_timestamp("01:02:03"), Some(3_723_000));
        assert_eq!(parse_lrc_timestamp("ar:Someone"), None);
    }

    #[test]
    fn converts_lrc_to_ttml() {
        let lrc = "[ar:Someone]\n[00:02.12] Hello world\n[00:05.00] Second line";
        let ttml = lrc_to_ttml(lrc, 10_000).expect("ttml");
        assert!(ttml.contains("begin=\"2.120\""));
        let parsed = lyrics::parse(&ttml, 10_000).expect("parsed");
        assert_eq!(parsed.sync, SyncType::Line);
        assert_eq!(parsed.lines[0].words, "Hello world");
        assert_eq!(parsed.lines[1].words, "Second line");
    }

    #[test]
    fn enhanced_lrc_keeps_word_timing() {
        let lrc = "[00:01.00]<00:01.00>Hello <00:01.50>wor<00:01.80>ld<00:02.40>\n[00:03.00]Next";
        let ttml = lrc_to_ttml(lrc, 5_000).expect("ttml");
        let parsed = lyrics::parse(&ttml, 5_000).expect("parsed");
        assert_eq!(parsed.sync, SyncType::Rich);
        let first = &parsed.lines[0];
        assert_eq!(first.words, "Hello world");
        assert_eq!(first.parts.len(), 3);
        assert_eq!(first.parts[1].start_ms, 1500);
        assert_eq!(first.parts[2].start_ms + first.parts[2].duration_ms, 2400);
    }

    #[test]
    fn apple_syllables_join_into_words() {
        let json = r#"{"type":"Syllable","content":[
            {"timestamp":1000,"endtime":3000,"agent":"v2","text":[
                {"text":"Not","timestamp":1000,"endtime":1400,"part":false},
                {"text":"e","timestamp":1400,"endtime":1600,"part":true},
                {"text":"nough","timestamp":1600,"endtime":2200,"part":false}],
             "backgroundText":[{"text":"(oh)","timestamp":2200,"endtime":3000,"part":false}]},
            {"timestamp":4000,"endtime":5000,"text":[{"text":"Bye","timestamp":4000,"endtime":5000,"part":false}]}
        ]}"#;
        let body: AppleLyrics = serde_json::from_str(json).expect("json");
        let ttml = apple_to_ttml(body).expect("ttml");
        let parsed = lyrics::parse(&ttml, 6_000).expect("parsed");
        assert_eq!(parsed.sync, SyncType::Rich);
        let first = &parsed.lines[0];
        assert_eq!(first.words, "Not enough (oh)");
        assert_eq!(first.agent.as_deref(), Some("v2"));
        assert!(first.parts.iter().any(|part| part.background));
    }

    #[test]
    fn apple_unsynced_uses_plain_text() {
        let json = r#"{"type":"Unsynced","content":[],"plain":"[Verse]\nOne\n\nTwo"}"#;
        let body: AppleLyrics = serde_json::from_str(json).expect("json");
        let ttml = apple_to_ttml(body).expect("ttml");
        assert_eq!(classify(&ttml, 0), Some(Tier::Plain));
        let parsed = lyrics::parse(&ttml, 0).expect("parsed");
        let words: Vec<&str> = parsed
            .lines
            .iter()
            .map(|line| line.words.as_str())
            .collect();
        assert_eq!(words, ["One", "Two"]);
    }

    #[test]
    fn lyrics_plus_word_and_line_types() {
        let word = r#"{"type":"Word","lyrics":[{"time":1000,"duration":1000,"text":"I been",
            "syllabus":[{"time":1000,"duration":300,"text":"I "},{"time":1300,"duration":700,"text":"been"}],
            "element":{"singer":"v1"}}]}"#;
        let ttml = plus_to_ttml(serde_json::from_str(word).expect("json")).expect("ttml");
        assert_eq!(classify(&ttml, 3), Some(Tier::Word));
        let line = r#"{"type":"Line","lyrics":[{"time":1000,"duration":1000,"text":"I been"},
            {"time":2000,"duration":1000,"text":"calling"}]}"#;
        let ttml = plus_to_ttml(serde_json::from_str(line).expect("json")).expect("ttml");
        assert_eq!(classify(&ttml, 3), Some(Tier::Line));
    }

    #[test]
    fn waits_for_earlier_sources_that_could_still_win() {
        let outcomes = vec![
            None,
            Some(Ok(None)),
            Some(Ok(None)),
            Some(Ok(None)),
            hit(Tier::Line),
        ];
        assert!(decide(&outcomes).is_none());
        let outcomes = vec![hit(Tier::Word), None, None, None, None];
        assert_eq!(decide(&outcomes), Some(Ok(Some("Word".into()))));
        let outcomes = vec![hit(Tier::Line), None, None, hit(Tier::Word), None];
        assert!(decide(&outcomes).is_none());
        let outcomes = vec![
            hit(Tier::Line),
            Some(Ok(None)),
            Some(Ok(None)),
            hit(Tier::Word),
            None,
        ];
        assert_eq!(decide(&outcomes), Some(Ok(Some("Word".into()))));
    }

    #[test]
    fn reports_an_error_only_when_every_source_failed() {
        let failed = || Some(Err("down".to_string()));
        let outcomes = vec![failed(), failed(), failed(), failed(), failed()];
        assert_eq!(decide(&outcomes), Some(Err("down".into())));
        let outcomes = vec![failed(), Some(Ok(None)), failed(), failed(), failed()];
        assert_eq!(decide(&outcomes), Some(Ok(None)));
    }
}
