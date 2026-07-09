// yt-dlp integration — download audio from supported URLs via external binary.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri::path::BaseDirectory;

use crate::library::{self, MusicFile};
use crate::metadata;

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

static DOWNLOAD_CANCELLED: AtomicBool = AtomicBool::new(false);
static ACTIVE_CHILD: Mutex<Option<Child>> = Mutex::new(None);

pub fn cancel_download() {
    DOWNLOAD_CANCELLED.store(true, Ordering::SeqCst);
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
    let mut out = Vec::with_capacity(args.len() + 2);
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
        "vk.com", "rutube.ru", "dailymotion.com",
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

    Command::new(&binary)
        .args(&full_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))
}

fn parse_probe_json(raw: &str) -> Result<YtdlpProbeResult, String> {
    let entry: YtdlpJsonEntry = serde_json::from_str(raw)
        .map_err(|e| format!("Failed to parse yt-dlp response: {}", e))?;

    if let Some(entries) = entry.entries {
        let count = entries.len() as u32;
        let first_title = entries
            .first()
            .and_then(|e| e.title.clone())
            .unwrap_or_else(|| "Playlist".to_string());

        return Ok(YtdlpProbeResult {
            title: first_title,
            uploader: entry.uploader,
            duration_secs: None,
            thumbnail: entry.thumbnail,
            is_playlist: true,
            entry_count: Some(count),
        });
    }

    Ok(YtdlpProbeResult {
        title: entry.title.unwrap_or_else(|| "Unknown".to_string()),
        uploader: entry.uploader,
        duration_secs: entry.duration,
        thumbnail: entry.thumbnail,
        is_playlist: false,
        entry_count: None,
    })
}

pub fn probe(app: &AppHandle, url: &str) -> Result<YtdlpProbeResult, String> {
    let trimmed = url.trim();
    if !is_supported_url(trimmed) {
        return Err("URL is not recognized as a supported media link".to_string());
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

fn parse_progress_line(line: &str) -> Option<f32> {
    // [download]  45.3% of ...
    if !line.contains("[download]") {
        return None;
    }
    let pct_pos = line.find('%')?;
    let before = &line[..pct_pos];
    let num_start = before
        .rfind(|c: char| !c.is_ascii_digit() && c != '.')
        .map(|i| i + 1)
        .unwrap_or(0);
    before[num_start..].trim().parse().ok()
}

pub fn download(
    app: &AppHandle,
    url: &str,
    output_dir: Option<&str>,
) -> Result<YtdlpDownloadResult, String> {
    let trimmed = url.trim();
    if !is_supported_url(trimmed) {
        return Err("URL is not recognized as a supported media link".to_string());
    }

    DOWNLOAD_CANCELLED.store(false, Ordering::SeqCst);

    let dir = resolve_download_dir(app, output_dir)?;
    let dir_str = dir.to_string_lossy().to_string();

    let output_template = format!("{}/%(title)s.%(ext)s", dir_str);

    let binary = ytdlp_binary_path(app);
    if !binary.is_file() {
        return Err(format!(
            "yt-dlp not found at {}. Place {} in src-tauri/bin/",
            binary.display(),
            ytdlp_binary_name()
        ));
    }

    emit_progress(app, trimmed, "Starting download…", None);

    let mut cmd_args = build_ytdlp_args(app, &[]);
    cmd_args.extend([
        "--newline".to_string(),
        "--no-warnings".to_string(),
        "-x".to_string(),
        "--audio-format".to_string(),
        "mp3".to_string(),
        "--audio-quality".to_string(),
        "0".to_string(),
        "--embed-thumbnail".to_string(),
        "--embed-metadata".to_string(),
        "--parse-metadata".to_string(),
        "%(artist,album_artist,uploader,channel,creator)s:%(artist)s".to_string(),
        "--write-info-json".to_string(),
        "-o".to_string(),
        output_template,
        "--print".to_string(),
        "after_move:filepath".to_string(),
        trimmed.to_string(),
    ]);

    let mut child = Command::new(&binary)
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start yt-dlp: {}", e))?;

    let stderr = child.stderr.take();
    let stdout = child.stdout.take();

    if let Ok(mut guard) = ACTIVE_CHILD.lock() {
        *guard = Some(child);
    }

    let app_stderr = app.clone();
    let url_stderr = trimmed.to_string();
    let stderr_handle = stderr.map(|stderr| {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(pct) = parse_progress_line(&line) {
                    emit_progress(&app_stderr, &url_stderr, "Downloading…", Some(pct));
                } else if line.contains("ExtractAudio") || line.contains("ffmpeg") {
                    emit_progress(&app_stderr, &url_stderr, "Converting to MP3…", None);
                }
            }
        })
    });

    let mut downloaded_paths: Vec<String> = Vec::new();
    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            let path = line.trim().to_string();
            if !path.is_empty() && Path::new(&path).is_file() {
                downloaded_paths.push(path);
            }
        }
    }

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

    if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
        return Err("Download cancelled".to_string());
    }

    if !status.success() {
        return Err("yt-dlp download failed".to_string());
    }

    // Fallback: scan output dir for new mp3 files if --print didn't yield paths
    if downloaded_paths.is_empty() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) {
                    downloaded_paths.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    if downloaded_paths.is_empty() {
        return Err("Download finished but no audio files were found".to_string());
    }

    emit_progress(app, trimmed, "Done", Some(100.0));

    let mut files = library::fetch_metadata(&downloaded_paths)?;
    enrich_downloaded_metadata(&mut files);
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
    let title = title
        .map(|value| metadata::strip_ytdlp_id_suffix(&value))
        .filter(|value| !value.is_empty());
    let artist = artist
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

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

fn enrich_downloaded_metadata(files: &mut [MusicFile]) {
    for file in files.iter_mut() {
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
        }

        apply_metadata_to_file(file, title, artist);
    }
}

