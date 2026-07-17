// Spotify downloads via bundled spotDL (https://github.com/spotDL/spotify-downloader).
//
// spotDL resolves Spotify metadata, finds matching audio on YouTube (etc.),
// downloads with yt-dlp/ffmpeg, and embeds tags + cover art.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;

use regex::Regex;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri::path::BaseDirectory;

use crate::library;
use crate::ytdlp::{self, YtdlpDownloadResult, YtdlpProbeResult, YtdlpProgress};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static ACTIVE_CHILD: Mutex<Option<Child>> = Mutex::new(None);

pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
    if let Ok(mut guard) = ACTIVE_CHILD.lock() {
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
        }
    }
}

fn check_cancel() -> Result<(), String> {
    if CANCELLED.load(Ordering::SeqCst) {
        Err("Download cancelled".to_string())
    } else {
        Ok(())
    }
}

/// Spotify track / album / playlist / artist / episode / show links.
pub fn is_spotify_url(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.starts_with("spotify:") {
        return true;
    }
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    lower.contains("spotify.com")
        || lower.contains("spotify.link")
        || lower.contains("spoti.fi")
}

fn is_playlist_like(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("/playlist/")
        || lower.contains("/album/")
        || lower.contains("/artist/")
        || lower.contains("/collection/")
        || lower.contains(":playlist:")
        || lower.contains(":album:")
        || lower.contains(":artist:")
}

fn spotdl_binary_candidates(app: Option<&AppHandle>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("bin"));
            dirs.push(parent.to_path_buf());
        }
    }
    if let Some(app) = app {
        if let Ok(resource_bin) = app.path().resolve("bin", BaseDirectory::Resource) {
            dirs.push(resource_bin);
        }
    }
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin"));

    let mut out = Vec::new();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        // Prefer exact names first
        for name in ["spotdl.exe", "spotdl", "spotdl-4.5.0.exe"] {
            let p = dir.join(name);
            if p.is_file() {
                out.push(p);
            }
        }
        // Any spotdl*.exe
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("spotdl")
                    && (name.ends_with(".exe") || !cfg!(windows))
                    && entry.path().is_file()
                {
                    let p = entry.path();
                    if !out.iter().any(|x| x == &p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

pub fn spotdl_binary_path(app: &AppHandle) -> Option<PathBuf> {
    spotdl_binary_candidates(Some(app)).into_iter().next()
}

pub fn spotdl_available(app: &AppHandle) -> bool {
    spotdl_binary_path(app).is_some()
}

fn ffmpeg_for_spotdl(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = ytdlp::resolve_ffmpeg_location(app)
        .ok_or_else(|| "ffmpeg not found (required by spotDL)".to_string())?;
    let bin = dir.join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    if !bin.is_file() {
        return Err(format!("ffmpeg not found at {}", bin.display()));
    }
    Ok(bin)
}

fn emit_progress(app: &AppHandle, url: &str, status: &str, percent: Option<f32>) {
    let _ = app.emit(
        "ytdlp:progress",
        YtdlpProgress {
            status: status.to_string(),
            percent,
            url: url.to_string(),
        },
    );
}

// ── Probe via Spotify oEmbed (no spotDL network to YouTube required) ─────────

#[derive(Debug, Deserialize)]
struct SpotifyOEmbed {
    title: Option<String>,
    thumbnail_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeezerSearch {
    data: Option<Vec<DeezerTrack>>,
}

#[derive(Debug, Deserialize)]
struct DeezerTrack {
    title: Option<String>,
    title_short: Option<String>,
    artist: Option<DeezerArtist>,
}

#[derive(Debug, Deserialize)]
struct DeezerArtist {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItunesSearch {
    results: Option<Vec<ItunesTrack>>,
}

#[derive(Debug, Deserialize)]
struct ItunesTrack {
    track_name: Option<String>,
    #[serde(rename = "trackName")]
    track_name_camel: Option<String>,
    artist_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name_camel: Option<String>,
}

fn http_get_text(url: &str, accept: &str) -> Result<String, String> {
    let mut response = ureq::get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .header("Accept", accept)
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .map_err(|e| format!("Request failed: {e}"))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))
}

fn titles_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    };
    let na = norm(a);
    let nb = norm(b);
    !na.is_empty() && (na == nb || na.contains(&nb) || nb.contains(&na))
}

/// Spotify web pages no longer expose track meta to plain HTTP scrapers.
/// Resolve artist via free music search APIs using the oEmbed title.
fn lookup_artist_by_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }

    // 1) Deezer
    let deezer_url = format!(
        "https://api.deezer.com/search?q={}&limit=10",
        urlencoding::encode(title)
    );
    if let Ok(raw) = http_get_text(&deezer_url, "application/json") {
        if let Ok(search) = serde_json::from_str::<DeezerSearch>(&raw) {
            if let Some(tracks) = search.data {
                // Prefer exact-ish title match
                for t in &tracks {
                    let t_title = t
                        .title
                        .as_deref()
                        .or(t.title_short.as_deref())
                        .unwrap_or("");
                    if titles_match(title, t_title) {
                        if let Some(name) = t.artist.as_ref().and_then(|a| a.name.clone()) {
                            let name = name.trim();
                            if !name.is_empty() && !name.eq_ignore_ascii_case("spotify") {
                                return Some(name.to_string());
                            }
                        }
                    }
                }
                // Fallback: first result with an artist
                for t in &tracks {
                    if let Some(name) = t.artist.as_ref().and_then(|a| a.name.clone()) {
                        let name = name.trim();
                        if !name.is_empty() && !name.eq_ignore_ascii_case("spotify") {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
    }

    // 2) iTunes Search
    let itunes_url = format!(
        "https://itunes.apple.com/search?term={}&entity=song&limit=10",
        urlencoding::encode(title)
    );
    if let Ok(raw) = http_get_text(&itunes_url, "application/json") {
        if let Ok(search) = serde_json::from_str::<ItunesSearch>(&raw) {
            if let Some(results) = search.results {
                for t in &results {
                    let t_title = t
                        .track_name_camel
                        .as_deref()
                        .or(t.track_name.as_deref())
                        .unwrap_or("");
                    let artist = t
                        .artist_name_camel
                        .as_deref()
                        .or(t.artist_name.as_deref())
                        .unwrap_or("")
                        .trim();
                    if artist.is_empty() || artist.eq_ignore_ascii_case("spotify") {
                        continue;
                    }
                    if titles_match(title, t_title) {
                        return Some(artist.to_string());
                    }
                }
                for t in &results {
                    if let Some(artist) = t
                        .artist_name_camel
                        .as_deref()
                        .or(t.artist_name.as_deref())
                    {
                        let artist = artist.trim();
                        if !artist.is_empty() && !artist.eq_ignore_ascii_case("spotify") {
                            return Some(artist.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

pub fn probe(app: &AppHandle, url: &str) -> Result<YtdlpProbeResult, String> {
    let trimmed = url.trim();
    if !is_spotify_url(trimmed) {
        return Err("Not a Spotify URL".to_string());
    }
    if !spotdl_available(app) {
        return Err(
            "spotDL not found. Place spotdl.exe (or spotdl-*.exe) in src-tauri/bin/".to_string(),
        );
    }

    let oembed = format!(
        "https://open.spotify.com/oembed?url={}",
        urlencoding::encode(trimmed)
    );
    let raw = http_get_text(&oembed, "application/json")?;
    let data: SpotifyOEmbed = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse Spotify metadata: {e}"))?;

    let title = data
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Unknown".to_string());

    // Never show "Spotify" as artist. For tracks, resolve real artist by title.
    let artist = if is_playlist_like(trimmed) {
        None
    } else {
        lookup_artist_by_title(&title)
    };

    Ok(YtdlpProbeResult {
        title,
        uploader: artist,
        duration_secs: None,
        thumbnail: data.thumbnail_url,
        is_playlist: is_playlist_like(trimmed),
        entry_count: None,
    })
}

// ── Download ─────────────────────────────────────────────────────────────────

fn normalize_path_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
        .to_ascii_lowercase()
}

fn snapshot_audio_dir(dir: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| {
                let e = e.to_string_lossy().to_ascii_lowercase();
                matches!(e.as_str(), "mp3" | "m4a" | "opus" | "flac" | "ogg" | "wav")
            }) {
                set.insert(normalize_path_key(&path));
            }
        }
    }
    set
}

fn collect_new_audio_files(dir: &Path, before: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| {
                let e = e.to_string_lossy().to_ascii_lowercase();
                matches!(e.as_str(), "mp3" | "m4a" | "opus" | "flac" | "ogg" | "wav")
            }) {
                let key = normalize_path_key(&path);
                if !before.contains(&key) {
                    out.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn re_found_songs() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)Found\s+(\d+)\s+songs?").expect("found re"))
}

fn re_downloaded() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(Downloaded|Skipping|Processing|Downloading)\b").expect("dl re")
    })
}

fn parse_spotdl_progress(line: &str, total: &mut Option<u32>, done: &mut u32) -> Option<(String, f32)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    if let Some(caps) = re_found_songs().captures(line) {
        if let Ok(n) = caps[1].parse::<u32>() {
            *total = Some(n.max(1));
            return Some((format!("Found {n} tracks…"), 2.0));
        }
    }

    // Explicit percent if present
    if let Some(pos) = line.find('%') {
        let before = &line[..pos];
        let num_start = before
            .rfind(|c: char| !c.is_ascii_digit() && c != '.')
            .map(|i| i + 1)
            .unwrap_or(0);
        if let Ok(p) = before[num_start..].trim().parse::<f32>() {
            if (0.0..=100.0).contains(&p) {
                return Some(("Downloading…".into(), p));
            }
        }
    }

    if re_downloaded().is_match(line) {
        if line.to_ascii_lowercase().contains("downloaded")
            || line.to_ascii_lowercase().contains("skipping")
        {
            *done = done.saturating_add(1);
        }
        if let Some(t) = *total {
            let pct = ((*done as f32) / (t as f32) * 92.0 + 5.0).min(95.0);
            return Some((
                format!("Downloading… ({done}/{t})"),
                pct,
            ));
        }
        // Unknown total — ease forward slowly
        let pct = (5.0 + (*done as f32) * 3.0).min(90.0);
        return Some((format!("Downloading… ({done})"), pct));
    }

    None
}

pub fn download(
    app: &AppHandle,
    url: &str,
    output_dir: Option<&str>,
    _allow_playlist: bool,
) -> Result<YtdlpDownloadResult, String> {
    CANCELLED.store(false, Ordering::SeqCst);

    let trimmed = url.trim();
    if !is_spotify_url(trimmed) {
        return Err("Not a Spotify URL".to_string());
    }

    let binary = spotdl_binary_path(app).ok_or_else(|| {
        "spotDL not found. Place spotdl.exe (or spotdl-*.exe) in src-tauri/bin/".to_string()
    })?;
    let ffmpeg = ffmpeg_for_spotdl(app)?;

    let dir = ytdlp::resolve_download_dir(app, output_dir)?;
    let dir_str = dir.to_string_lossy().to_string();
    let before = snapshot_audio_dir(&dir);

    // spotDL output template — writes into download folder with artist/title.
    let output_template = format!(
        "{}{}{{artists}} - {{title}}.{{output-ext}}",
        dir_str.trim_end_matches(['/', '\\']),
        std::path::MAIN_SEPARATOR
    );

    emit_progress(app, trimmed, "Starting Spotify download…", Some(0.0));

    let mut child = Command::new(&binary)
        .args([
            "download",
            trimmed,
            "--ffmpeg",
            &ffmpeg.to_string_lossy(),
            "--format",
            "mp3",
            "--bitrate",
            "320k",
            "--output",
            &output_template,
            "--overwrite",
            "force",
            "--threads",
            "2",
            "--log-level",
            "INFO",
            "--print-errors",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start spotDL: {e}"))?;

    let stderr = child.stderr.take();
    let stdout = child.stdout.take();

    if let Ok(mut guard) = ACTIVE_CHILD.lock() {
        *guard = Some(child);
    }

    let app_log = app.clone();
    let url_log = trimmed.to_string();
    let log_handle = {
        let app_log = app_log.clone();
        let url_log = url_log.clone();
        thread::spawn(move || {
            let mut total: Option<u32> = None;
            let mut done: u32 = 0;
            let mut last_pct = -1.0f32;

            let mut handle_line = |line: String| {
                if CANCELLED.load(Ordering::SeqCst) {
                    return;
                }
                eprintln!("[spotdl] {line}");
                if let Some((status, pct)) = parse_spotdl_progress(&line, &mut total, &mut done) {
                    if (pct - last_pct).abs() >= 0.4 || pct >= 95.0 {
                        last_pct = pct;
                        emit_progress(&app_log, &url_log, &status, Some(pct));
                    }
                }
            };

            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    handle_line(line);
                }
            }
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    handle_line(line);
                }
            }
        })
    };

    let status = {
        let mut guard = ACTIVE_CHILD
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        let mut child = guard
            .take()
            .ok_or_else(|| "Download cancelled".to_string())?;
        child
            .wait()
            .map_err(|e| format!("spotDL process error: {e}"))?
    };

    let _ = log_handle.join();

    if CANCELLED.load(Ordering::SeqCst) {
        return Err("Download cancelled".to_string());
    }

    if !status.success() {
        return Err(
            "spotDL download failed. Check the Spotify link and network, or try again.".to_string(),
        );
    }

    check_cancel()?;

    let mut paths = collect_new_audio_files(&dir, &before);
    // Fallback: any mp3 modified in the last few minutes matching folder
    if paths.is_empty() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) {
                    paths.push(path.to_string_lossy().to_string());
                }
            }
            // only keep "new" ones if we had a snapshot — already empty means keep all is wrong
            // better: leave empty
            if !before.is_empty() {
                paths.retain(|p| !before.contains(&normalize_path_key(Path::new(p))));
            } else {
                // if folder was empty, all files are new
            }
        }
    }

    if paths.is_empty() {
        return Err("spotDL finished but no audio files were found".to_string());
    }

    emit_progress(app, trimmed, "Processing files…", Some(98.0));
    let files = library::fetch_metadata(&paths)?;
    emit_progress(app, trimmed, "Done", Some(100.0));
    Ok(YtdlpDownloadResult { files })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_spotify_urls() {
        assert!(is_spotify_url(
            "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"
        ));
        assert!(is_spotify_url(
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M"
        ));
        assert!(is_spotify_url("spotify:track:4cOdK2wGLETKBW3PvgPWqT"));
        assert!(is_spotify_url(
            "https://open.spotify.com/intl-ru/track/abc"
        ));
        assert!(!is_spotify_url("https://youtube.com/watch?v=x"));
        assert!(!is_spotify_url("https://vk.com/audio1_2"));
    }

    #[test]
    fn playlist_like() {
        assert!(is_playlist_like(
            "https://open.spotify.com/album/xxx"
        ));
        assert!(!is_playlist_like(
            "https://open.spotify.com/track/xxx"
        ));
    }
}
