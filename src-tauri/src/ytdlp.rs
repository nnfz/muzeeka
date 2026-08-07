// yt-dlp integration — download audio from supported URLs via external binary.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use regex::Regex;
use ureq::Agent;

use rayon::prelude::*;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri::path::BaseDirectory;

use crate::library::{self, MusicFile};
use crate::metadata;
use crate::process_util;

static SPOTIFY_NEXT_DATA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<script id="__NEXT_DATA__" type="application/json">(.*?)</script>"#)
        .expect("spotify next-data regex")
});
static SPOTIFY_OG_DESC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s+property=["']og:description["']\s+content=["'](.*?)["']"#)
        .expect("spotify og:description regex")
});

const YTDLP_HTTP_TIMEOUT: Duration = Duration::from_secs(20);
static YTDLP_HTTP_AGENT: OnceLock<Agent> = OnceLock::new();

fn http_agent() -> &'static Agent {
    YTDLP_HTTP_AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .timeout_global(Some(YTDLP_HTTP_TIMEOUT))
            .build();
        config.into()
    })
}

/// Normalize user paste: trim only. Do **not** strip `?…` — YouTube needs `?v=…`.
fn normalize_media_url(url: &str) -> &str {
    url.trim()
}

#[derive(Debug, Clone, Serialize)]
pub struct YtdlpProbeResult {
    pub title: String,
    pub uploader: Option<String>,
    pub duration_secs: Option<f64>,
    pub thumbnail: Option<String>,
    pub is_playlist: bool,
    pub entry_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YtdlpDownloadResult {
    pub files: Vec<MusicFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YtdlpProgress {
    pub status: String,
    pub percent: Option<f32>,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct YtdlpInfoJson {
    title: Option<String>,
    uploader: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    channel: Option<String>,
    creator: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YtdlpJsonEntry {
    title: Option<String>,
    uploader: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
    #[serde(default)]
    _type: Option<String>,
    #[serde(default)]
    entries: Option<Vec<YtdlpJsonEntry>>,
}

#[derive(Debug, Deserialize)]
struct SpotifyOEmbed {
    title: Option<String>,
    thumbnail_url: Option<String>,
}

static DOWNLOAD_CANCELLED: AtomicBool = AtomicBool::new(false);
static ACTIVE_CHILD: Mutex<Option<Child>> = Mutex::new(None);

pub fn cancel_download() {
    DOWNLOAD_CANCELLED.store(true, Ordering::SeqCst);
    crate::vk_audio::cancel();
    if let Ok(mut guard) = ACTIVE_CHILD.lock() {
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
        }
    }
}

fn ytdlp_binary_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

fn ffmpeg_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn ytdlp_dir_is_valid(dir: &Path) -> bool {
    dir.join(ytdlp_binary_name()).is_file()
}

/// Resolve the directory where the yt-dlp binary lives.
pub fn resolve_ytdlp_dir(app: Option<&AppHandle>) -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("bin"));
            candidates.push(parent.to_path_buf());
        }
    }

    if let Some(app) = app {
        if let Ok(resource_bin) = app.path().resolve("bin", BaseDirectory::Resource) {
            candidates.push(resource_bin);
        }
    }

    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin"));

    for dir in candidates {
        if ytdlp_dir_is_valid(&dir) {
            eprintln!("yt-dlp directory: {}", dir.display());
            return dir;
        }
    }

    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin");
    eprintln!("yt-dlp directory (fallback): {}", fallback.display());
    fallback
}

pub fn ytdlp_binary_path(app: &AppHandle) -> PathBuf {
    resolve_ytdlp_dir(Some(app)).join(ytdlp_binary_name())
}

pub fn ytdlp_available(app: &AppHandle) -> bool {
    ytdlp_binary_path(app).is_file()
}

/// Directory containing a bundled ffmpeg binary (same `bin/` folder as yt-dlp).
pub fn resolve_ffmpeg_location(app: &AppHandle) -> Option<PathBuf> {
    let bin_dir = resolve_ytdlp_dir(Some(app));
    if bin_dir.join(ffmpeg_binary_name()).is_file() {
        Some(bin_dir)
    } else {
        None
    }
}

pub fn ffmpeg_available(app: &AppHandle) -> bool {
    resolve_ffmpeg_location(app).is_some()
}

fn build_ytdlp_args(app: &AppHandle, args: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 4);
    
    out.push("--encoding".to_string());
    out.push("utf-8".to_string());
    
    if let Some(dir) = resolve_ffmpeg_location(app) {
        out.push("--ffmpeg-location".to_string());
        out.push(dir.to_string_lossy().to_string());
    }
    out.extend(args.iter().map(|s| (*s).to_string()));
    out
}

/// Default download folder: `{app_data}/downloads`.
pub fn default_download_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {}", e))?
        .join("downloads");

    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create downloads dir: {}", e))?;

    Ok(dir)
}

pub fn resolve_download_dir(app: &AppHandle, folder: Option<&str>) -> Result<PathBuf, String> {
    let dir = match folder.filter(|s| !s.trim().is_empty()) {
        Some(path) => PathBuf::from(path.trim()),
        None => default_download_dir(app)?,
    };

    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create download folder: {}", e))?;

    Ok(dir)
}

/// Heuristic URL check for common video/audio hosting sites.
pub fn is_supported_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lower = trimmed.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return false;
    }

    let hosts = [
        "youtube.com", "youtu.be", "music.youtube.com",
        "soundcloud.com", "bandcamp.com", "vimeo.com",
        "twitch.tv", "tiktok.com", "instagram.com",
        "twitter.com", "x.com", "facebook.com",
        "vk.com", "vk.ru", "m.vk.com", "m.vk.ru",
        "rutube.ru", "dailymotion.com",
        "mixcloud.com", "audiomack.com", "deezer.com",
        "spotify.com", "nicovideo.jp", "bilibili.com",
    ];

    hosts.iter().any(|host| lower.contains(host))
}

fn run_ytdlp(app: &AppHandle, args: &[&str]) -> Result<std::process::Output, String> {
    let binary = ytdlp_binary_path(app);
    if !binary.is_file() {
        return Err(format!(
            "yt-dlp not found at {}. Place {} in src-tauri/bin/",
            binary.display(),
            ytdlp_binary_name()
        ));
    }

    let full_args = build_ytdlp_args(app, args);

    log_ytdlp(
        Some(app),
        &format!("run: {} {}", binary.display(), full_args.join(" ")),
    );

    let mut cmd = Command::new(&binary);
    cmd.args(&full_args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        
    process_util::hide_console(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        log_ytdlp(Some(app), line);
    }
    if !output.status.success() {
        log_ytdlp(
            Some(app),
            &format!("exit status: {}", output.status),
        );
    }

    Ok(output)
}

fn clean_probe_title(title: String, uploader: Option<&str>) -> String {
    let title = metadata::strip_ytdlp_id_suffix(&title);
    match uploader {
        Some(artist) => metadata::strip_redundant_artist_prefix(&title, artist).unwrap_or(title),
        None => title,
    }
}

fn parse_probe_json(raw: &str) -> Result<YtdlpProbeResult, String> {
    let entry: YtdlpJsonEntry = serde_json::from_str(raw)
        .map_err(|e| format!("Failed to parse yt-dlp response: {}", e))?;

    if let Some(entries) = entry.entries {
        let count = entries.len() as u32;
        let title = entry
            .title
            .filter(|t| !t.trim().is_empty())
            .or_else(|| entries.first().and_then(|e| e.title.clone()))
            .unwrap_or_else(|| "Playlist".to_string());
        // Playlist titles usually aren't "Artist - …"; leave as-is.
        return Ok(YtdlpProbeResult {
            title,
            uploader: entry.uploader,
            duration_secs: None,
            thumbnail: entry.thumbnail,
            is_playlist: true,
            entry_count: Some(count),
        });
    }

    let uploader = entry.uploader;
    let title = clean_probe_title(
        entry.title.unwrap_or_else(|| "Unknown".to_string()),
        uploader.as_deref(),
    );

    Ok(YtdlpProbeResult {
        title,
        uploader,
        duration_secs: entry.duration,
        thumbnail: entry.thumbnail,
        is_playlist: false,
        entry_count: None,
    })
}

fn http_get_text(url: &str, accept: &str) -> Result<String, String> {
    let mut response = http_agent()
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
        )
        .header("Accept", accept)
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .map_err(|e| format!("Request failed: {}", e))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {}", e))
}

fn is_playlist_like(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("/playlist/") || lower.contains("/album/") || lower.contains("/artist/")
}

fn unescape_html(s: &str) -> String {
    s.replace("&amp;", "&")
    .replace("&#39;", "'")
    .replace("&quot;", "\"")
}


fn extract_spotify_playlist_queries(url: &str) -> (Vec<String>, Option<String>) {
    let mut queries = Vec::new();
    let mut first_cover = None;
    
    let embed_url = url
        .replace("open.spotify.com/playlist/", "open.spotify.com/embed/playlist/")
        .replace("open.spotify.com/album/", "open.spotify.com/embed/album/");
        
    let html = match http_get_text(&embed_url, "text/html") {
        Ok(h) => h,
        Err(_) => return (queries, first_cover),
    };

    if let Some(caps) = SPOTIFY_NEXT_DATA_RE.captures(&html) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&caps[1]) {
            if let Some(track_list) = json
                .get("props")
                .and_then(|p| p.get("pageProps"))
                .and_then(|p| p.get("state"))
                .and_then(|p| p.get("data"))
                .and_then(|p| p.get("entity"))
                .and_then(|p| p.get("trackList"))
                .and_then(|p| p.as_array())
            {
                for track in track_list {
                    let title = track.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    let subtitle = track.get("subtitle").and_then(|s| s.as_str()).unwrap_or("");
                    
                    if first_cover.is_none() {
                        if let Some(cover_url) = track
                            .get("coverArt")
                            .and_then(|c| c.get("sources"))
                            .and_then(|s| s.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|src| src.get("url"))
                            .and_then(|u| u.as_str())
                        {
                            first_cover = Some(cover_url.to_string());
                        }
                    }
                    
                    if !title.is_empty() {
                        let clean_title = unescape_html(title);
                        let clean_artist = unescape_html(subtitle);
                        
                        let query = if clean_artist.is_empty() {
                            format!("ytsearch1:{} \"Topic\"", clean_title)
                        } else {
                            format!("ytsearch1:{} {} \"Topic\"", clean_artist, clean_title)
                        };
                        queries.push(query);
                    }
                }
            }
        }
    }
    (queries, first_cover)
}

fn probe_spotify(url: &str) -> Result<YtdlpProbeResult, String> {
    let is_playlist = is_playlist_like(url);

    let oembed = format!(
        "https://open.spotify.com/oembed?url={}",
        urlencoding::encode(url)
    );
    let oembed_raw = http_get_text(&oembed, "application/json").unwrap_or_default();
    let data: Option<SpotifyOEmbed> = serde_json::from_str(&oembed_raw).ok();

    let title = data.as_ref()
        .and_then(|d| d.title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Unknown".to_string());

    let mut thumbnail = data.and_then(|d| d.thumbnail_url);
    let mut artist = None;
    let mut entry_count = None;

    if is_playlist {
        let (queries, first_track_cover) = extract_spotify_playlist_queries(url);
        if !queries.is_empty() {
            entry_count = Some(queries.len() as u32);
        }
        if thumbnail.is_none() {
            thumbnail = first_track_cover;
        }
    } else {
        let html = http_get_text(url, "text/html").unwrap_or_default();
        if let Some(caps) = SPOTIFY_OG_DESC_RE.captures(&html) {
            let desc = caps[1].trim();
            let without_prefix = if let Some(idx) = desc.find("Spotify. ") {
                &desc[idx + 9..]
            } else {
                desc
            };
            let final_artist = without_prefix.split('·').next().unwrap_or(without_prefix).trim();
            if !final_artist.is_empty() && final_artist.to_lowercase() != "song" {
                artist = Some(unescape_html(final_artist));
            }
        }
    }

    Ok(YtdlpProbeResult {
        title: unescape_html(&title),
        uploader: artist,
        duration_secs: None,
        thumbnail,
        is_playlist,
        entry_count,
    })
}

pub fn probe(app: &AppHandle, url: &str) -> Result<YtdlpProbeResult, String> {
    // Keep query string: youtube.com/watch?v=ID must not become /watch.
    let trimmed = normalize_media_url(url);

    if !is_supported_url(trimmed) {
        return Err("URL is not recognized as a supported media link".to_string());
    }

    let lower_url = trimmed.to_lowercase();
    if lower_url.contains("spotify.com") || lower_url.contains("spoti.fi") {
        return probe_spotify(trimmed);
    }


    let output = run_ytdlp(
        app,
        &["--dump-single-json", "--no-warnings", "--no-download", trimmed],
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("yt-dlp probe failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_probe_json(stdout.trim())
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

/// Dump a yt-dlp line to the process stderr (cargo/tauri terminal) and, when
/// an AppHandle is available, forward it to the webview as `ytdlp:log` so it
/// also shows up in DevTools.
fn log_ytdlp(app: Option<&AppHandle>, line: &str) {
    let cleaned = strip_ansi(line);
    let cleaned = cleaned.trim_end_matches(['\r', '\n']);
    if cleaned.is_empty() {
        return;
    }

    let _ = writeln!(std::io::stderr(), "[yt-dlp] {cleaned}");
    let _ = std::io::stderr().flush();

    if let Some(app) = app {
        let _ = app.emit("ytdlp:log", cleaned.to_string());
    }
}

fn log_ytdlp_banner(app: Option<&AppHandle>, title: &str) {
    log_ytdlp(app, &format!("========== {title} =========="));
}

/// True for machine progress / redraw noise we still print live, but skip when
/// re-dumping a failure summary.
fn is_progress_noise(line: &str) -> bool {
    let line = strip_ansi(line);
    let line = line.trim();
    if line.is_empty() {
        return true;
    }
    if line.starts_with("muzeeka-progress:") {
        return true;
    }
    parse_progress_line(line).is_some()
}

fn normalize_path_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
        .to_ascii_lowercase()
}

fn snapshot_mp3_dir(dir: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) {
                set.insert(normalize_path_key(&path));
            }
        }
    }
    set
}

fn collect_new_mp3_files(dir: &Path, before: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) {
                let key = normalize_path_key(&path);
                if !before.contains(&key) {
                    out.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    out
}

fn sanitize_printed_path(line: &str) -> String {
    let trimmed = line.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(trimmed)
        .to_string()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC[ ... letter
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        // yt-dlp sometimes uses bare CR for progress redraws
        if c == '\r' {
            continue;
        }
        out.push(c);
    }
    out
}

fn parse_progress_line(line: &str) -> Option<f32> {
    let line = strip_ansi(line);
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    // Machine-readable: muzeeka-progress:45.3
    if let Some(rest) = line.strip_prefix("muzeeka-progress:") {
        let num = rest.trim().trim_end_matches('%').trim();
        return num.parse().ok().filter(|p| *p >= 0.0 && *p <= 100.0);
    }

    // [download]  45.3% of  4.50MiB at ...
    // [download] 100% of 1.23MiB in 00:01
    if line.contains("[download]") && line.contains('%') {
        let pct_pos = line.find('%')?;
        let before = &line[..pct_pos];
        let num_start = before
            .rfind(|c: char| !c.is_ascii_digit() && c != '.')
            .map(|i| i + 1)
            .unwrap_or(0);
        if let Ok(p) = before[num_start..].trim().parse::<f32>() {
            if (0.0..=100.0).contains(&p) {
                return Some(p);
            }
        }
    }

    None
}

pub fn download(
    app: &AppHandle,
    url: &str,
    output_dir: Option<&str>,
    allow_playlist: bool,
) -> Result<YtdlpDownloadResult, String> {
    let trimmed = normalize_media_url(url);
    if !is_supported_url(trimmed) {
        return Err("URL is not recognized as a supported media link".to_string());
    }

    DOWNLOAD_CANCELLED.store(false, Ordering::SeqCst);

    let lower_url = trimmed.to_lowercase();
    let is_spotify = lower_url.contains("spotify.com") || lower_url.contains("spoti.fi");

    let mut batch_file_path = None;

    let target_query = if is_spotify {
        emit_progress(app, trimmed, "Fetching Spotify metadata…", Some(5.0));
        
        let is_playlist = is_playlist_like(trimmed);
        
        if is_playlist {
            // Просто передаем ссылку, парсер сам сходит на embed-страницу и вытащит треки
            let (queries, _) = extract_spotify_playlist_queries(trimmed);
            
            if queries.is_empty() {
                return Err("Плейлист пуст или скрыт. Поддерживаются только открытые плейлисты.".to_string());
            }

            let temp_name = format!("muzeeka_spotify_{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
            let temp_path = std::env::temp_dir().join(temp_name);

            let mut file_content = String::from("\u{FEFF}");
            file_content.push_str(&queries.join("\n"));
            
            fs::write(&temp_path, file_content).map_err(|e| format!("Failed to write batch file: {}", e))?;
            batch_file_path = Some(temp_path);
            
            String::new()
        } else {
            let meta = probe_spotify(trimmed)?;
            let artist = meta.uploader.unwrap_or_else(|| "Unknown Artist".to_string());
            format!("ytsearch1:{} {} \"Topic\"", artist, meta.title)
        }
    } else {
        trimmed.to_string()
    };

    let dir = resolve_download_dir(app, output_dir)?;
    let dir_str = dir.to_string_lossy().to_string();
    let before_snapshot = snapshot_mp3_dir(&dir);

    let output_template = format!("{}/%(artist,uploader)s - %(title)s.%(ext)s", dir_str);

    let binary = ytdlp_binary_path(app);
    if !binary.is_file() {
        return Err(format!(
            "yt-dlp not found at {}. Place {} in src-tauri/bin/",
            binary.display(),
            ytdlp_binary_name()
        ));
    }

    emit_progress(app, trimmed, "Starting download…", Some(10.0));

    let mut cmd_args = build_ytdlp_args(app, &[]);
    cmd_args.extend([
        // "--ignore-errors".to_string(),
        "--newline".to_string(),
        // Keep warnings/errors visible in the console (do not silence yt-dlp).
        "--progress".to_string(),
        "--progress-template".to_string(),
        "download:muzeeka-progress:%(progress._percent_str)s".to_string(),
        "-x".to_string(),
        "--audio-format".to_string(),
        "mp3".to_string(),
        "--audio-quality".to_string(),
        "0".to_string(),
        "--embed-thumbnail".to_string(),
        "--convert-thumbnails".to_string(),
        "jpg".to_string(),
        "--ppa".to_string(),
        "ThumbnailsConvertor+ffmpeg_o:-vf crop=ih:ih".to_string(),
        "--ppa".to_string(),
        "EmbedThumbnail+ffmpeg_o:-id3v2_version 3".to_string(),
        "--ppa".to_string(),
        "Metadata+ffmpeg_o:-id3v2_version 3".to_string(),
        "--embed-metadata".to_string(),
        "--parse-metadata".to_string(),
        "%(artist,album_artist,uploader,channel,creator)s:%(artist)s".to_string(),
        "-o".to_string(),
        output_template,
        "--print".to_string(),
        "after_move:filepath".to_string(),
    ]);
    
    if !allow_playlist && batch_file_path.is_none() {
        cmd_args.push("--no-playlist".to_string());
    }
    
    if let Some(batch_path) = &batch_file_path {
        cmd_args.push("--batch-file".to_string());
        cmd_args.push(batch_path.to_string_lossy().to_string());
    } else {
        cmd_args.push(target_query);
    }

    log_ytdlp_banner(Some(app), "yt-dlp download start");
    log_ytdlp(
        Some(app),
        &format!("binary: {}", binary.display()),
    );
    log_ytdlp(
        Some(app),
        &format!("args: {}", cmd_args.join(" ")),
    );
    log_ytdlp(
        Some(app),
        &format!("output dir: {}", dir_str),
    );

    let mut cmd = Command::new(&binary);
    cmd.args(&cmd_args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process_util::hide_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start yt-dlp: {}", e))?;

    let stderr = child.stderr.take();
    let stdout = child.stdout.take();

    if let Ok(mut guard) = ACTIVE_CHILD.lock() {
        *guard = Some(child);
    }

    // Collect every stderr line so we can re-dump non-progress diagnostics on failure.
    let collected_stderr: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let app_stderr = app.clone();
    let url_stderr = trimmed.to_string();
    let collected_for_stderr = Arc::clone(&collected_stderr);
    let stderr_handle = stderr.map(|stderr| {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut last_emitted = -1.0f32;
            let mut saw_download = false;
            for line in reader.lines().map_while(Result::ok) {
                if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
                    break;
                }
                for part in line.split(['\r', '\n']) {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    log_ytdlp(Some(&app_stderr), part);
                    if let Ok(mut buf) = collected_for_stderr.lock() {
                        buf.push(part.to_string());
                    }
                    if let Some(pct) = parse_progress_line(part) {
                        saw_download = true;
                        if (pct - last_emitted).abs() >= 0.5 || pct >= 99.5 || last_emitted < 0.0 {
                            last_emitted = pct;
                            emit_progress(
                                &app_stderr,
                                &url_stderr,
                                "Downloading…",
                                Some(pct.clamp(0.0, 100.0)),
                            );
                        }
                    } else if part.contains("[download] Destination")
                        || part.contains("[download] Destination:")
                    {
                        emit_progress(&app_stderr, &url_stderr, "Downloading…", Some(0.0));
                    } else if part.contains("ExtractAudio")
                        || part.contains("[ExtractAudio]")
                        || (part.to_ascii_lowercase().contains("ffmpeg")
                            && (part.contains("Destination") || part.contains("Merging")
                                || part.contains("Post-process")))
                    {
                        emit_progress(
                            &app_stderr,
                            &url_stderr,
                            "Converting to MP3…",
                            Some(if saw_download { 97.0 } else { 90.0 }),
                        );
                    } else if part.contains("[Metadata]") || part.contains("Embedding") {
                        emit_progress(&app_stderr, &url_stderr, "Writing tags…", Some(99.0));
                    }
                }
            }
        })
    });

    let app_stdout = app.clone();
    let stdout_handle = stdout.map(|stdout| {
        thread::spawn(move || {
            let mut paths = Vec::new();
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let raw = line.trim();
                if !raw.is_empty() {
                    log_ytdlp(Some(&app_stdout), &format!("stdout: {raw}"));
                }
                let path = sanitize_printed_path(&line);
                if !path.is_empty() && Path::new(&path).is_file() {
                    paths.push(path);
                }
            }
            paths
        })
    });

    if let Some(handle) = stderr_handle {
        let _ = handle.join();
    }

    let status = {
        let mut guard = ACTIVE_CHILD
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let mut child = guard
            .take()
            .ok_or_else(|| "Download cancelled".to_string())?;
        child
            .wait()
            .map_err(|e| format!("yt-dlp process error: {}", e))?
    };

    log_ytdlp(Some(app), &format!("exit status: {status}"));

    if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
        log_ytdlp_banner(Some(app), "yt-dlp cancelled");
        return Err("Download cancelled".to_string());
    }

    let dump_stderr_summary = |app: &AppHandle, reason: &str| {
        log_ytdlp_banner(Some(app), reason);
        let lines = collected_stderr
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        let interesting: Vec<&String> = lines
            .iter()
            .filter(|l| !is_progress_noise(l))
            .collect();
        if interesting.is_empty() {
            log_ytdlp(Some(app), "(no yt-dlp stderr lines captured)");
        } else {
            log_ytdlp(
                Some(app),
                &format!("--- yt-dlp stderr ({} lines, progress filtered) ---", interesting.len()),
            );
            for line in interesting {
                log_ytdlp(Some(app), line);
            }
        }
        log_ytdlp_banner(Some(app), "end yt-dlp dump");
    };

    if !status.success() {
        dump_stderr_summary(app, "yt-dlp download FAILED");
        return Err("yt-dlp download failed".to_string());
    }

    let mut downloaded_paths = stdout_handle
        .map(|handle| handle.join())
        .transpose()
        .map_err(|_| "Failed to read yt-dlp output".to_string())?
        .unwrap_or_default();

    downloaded_paths.retain(|path| Path::new(path).is_file());

    if downloaded_paths.is_empty() {
        downloaded_paths = collect_new_mp3_files(&dir, &before_snapshot);
    }

    if downloaded_paths.is_empty() {
        dump_stderr_summary(app, "yt-dlp finished but NO audio files found");
        return Err("Download finished but no audio files were found".to_string());
    }

    log_ytdlp(
        Some(app),
        &format!("ok: {} file(s) downloaded", downloaded_paths.len()),
    );

    emit_progress(app, trimmed, "Processing files…", Some(100.0));

    let mut files = library::fetch_metadata(&downloaded_paths)?;
    enrich_downloaded_metadata(&mut files);

    emit_progress(app, trimmed, "Done", Some(100.0));
    if let Some(batch_path) = batch_file_path {
        let _ = fs::remove_file(batch_path);
    }
    Ok(YtdlpDownloadResult { files })
}

fn info_json_path(audio_path: &Path) -> Option<PathBuf> {
    let stem = audio_path.file_stem()?.to_str()?;
    Some(audio_path.with_file_name(format!("{stem}.info.json")))
}

fn pick_artist(info: &YtdlpInfoJson) -> Option<String> {
    [
        &info.artist,
        &info.album_artist,
        &info.uploader,
        &info.channel,
        &info.creator,
    ]
    .into_iter()
    .filter_map(|value| value.as_ref())
    .map(|s| s.trim())
    .find(|s| !s.is_empty())
    .map(|s| s.to_string())
}

fn parse_artist_title(title: &str) -> Option<(String, String)> {
    let clean = metadata::strip_ytdlp_id_suffix(title);

    for sep in [" - ", " — ", " – ", " | "] {
        let Some(pos) = clean.find(sep) else {
            continue;
        };

        let artist = clean[..pos].trim();
        let song = clean[pos + sep.len()..].trim();
        if artist.is_empty() || song.is_empty() || artist.len() > 120 {
            continue;
        }

        return Some((artist.to_string(), song.to_string()));
    }

    None
}

fn apply_metadata_to_file(
    file: &mut MusicFile,
    title: Option<String>,
    artist: Option<String>,
) {
    let path = Path::new(&file.path);
    let artist = artist
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let title = title
        .map(|value| metadata::strip_ytdlp_id_suffix(&value))
        .filter(|value| !value.is_empty())
        .map(|t| match artist.as_deref() {
            Some(a) => metadata::strip_redundant_artist_prefix(&t, a).unwrap_or(t),
            None => t,
        });

    if artist.is_some() || title.is_some() {
        if let Err(err) = metadata::write_track_tags(path, title.as_deref(), artist.as_deref()) {
            eprintln!("Failed to write tags to {}: {}", file.path, err);
        }
    }

    let meta = metadata::read_metadata(path, &file.file_name);
    file.title = meta.title.or(title);
    file.artist = meta.artist.or(artist);
    file.album = meta.album.or_else(|| file.album.clone());
    file.duration_secs = meta.duration_secs.or(file.duration_secs);
    file.year = meta.year.or(file.year);
    file.track_number = meta.track_number.or(file.track_number);
    file.genre = meta.genre.or_else(|| file.genre.clone());
    file.cover_path = meta.cover_path.or_else(|| file.cover_path.clone());
}

fn enrich_downloaded_file(file: &mut MusicFile) {
    let path = Path::new(&file.path);
    let mut title = file.title.clone();
    let mut artist = file.artist.clone();

    if let Some(json_path) = info_json_path(path) {
        if json_path.is_file() {
            if let Ok(raw) = fs::read_to_string(&json_path) {
                if let Ok(info) = serde_json::from_str::<YtdlpInfoJson>(&raw) {
                    if let Some(parsed_artist) = pick_artist(&info) {
                        artist = Some(parsed_artist);
                    }
                    if let Some(parsed_title) = info.title.filter(|t| !t.trim().is_empty()) {
                        title = Some(metadata::strip_ytdlp_id_suffix(&parsed_title));
                    }
                }
            }
            let _ = fs::remove_file(&json_path);
        }
    }

    if artist.is_none() {
        if let Some(ref current_title) = title {
            if let Some((parsed_artist, parsed_title)) = parse_artist_title(current_title) {
                artist = Some(parsed_artist);
                title = Some(parsed_title);
            }
        }
    } else if let (Some(ref a), Some(ref t)) = (&artist, &title) {
        // Uploader/channel already set artist, but title is still "Artist - Song".
        if let Some(stripped) = metadata::strip_redundant_artist_prefix(t, a) {
            title = Some(stripped);
        }
    }

    apply_metadata_to_file(file, title, artist);
}

fn enrich_downloaded_metadata(files: &mut [MusicFile]) {
    files.par_iter_mut().for_each(enrich_downloaded_file);
}
