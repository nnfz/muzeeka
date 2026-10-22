use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::ui::catalog;
use crate::ui::paths;

const DROP_QUERY: &[&str] = &[
    "si",
    "feature",
    "pp",
    "app",
    "fbclid",
    "gclid",
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_content",
    "utm_term",
    "embeds_referring_euri",
    "embeds_referring_origin",
    "source_ve_path",
];

const HOSTS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "music.youtube.com",
    "soundcloud.com",
    "bandcamp.com",
    "vimeo.com",
    "twitch.tv",
    "tiktok.com",
    "instagram.com",
    "twitter.com",
    "x.com",
    "facebook.com",
    "vk.com",
    "vk.ru",
    "m.vk.com",
    "m.vk.ru",
    "rutube.ru",
    "dailymotion.com",
    "mixcloud.com",
    "audiomack.com",
    "deezer.com",
    "spotify.com",
    "spotify.link",
    "spoti.fi",
    "nicovideo.jp",
    "bilibili.com",
];

#[derive(Clone, Debug)]
pub struct Probe {
    pub title: String,
    pub uploader: Option<String>,
    pub duration_secs: Option<f64>,
    pub thumbnail: Option<String>,
    pub is_playlist: bool,
    pub entry_count: Option<u32>,
}

pub struct JobSnap {
    pub status: String,
    pub percent: f32,
    pub pid: u32,
    pub cancel: bool,
    #[allow(dead_code)]
    pub done: Option<Result<Vec<String>, String>>,
}

impl JobSnap {
    pub(crate) fn new() -> Self {
        Self {
            status: "Starting download…".into(),
            percent: 0.0,
            pid: 0,
            cancel: false,
            done: None,
        }
    }
}

pub fn is_media_url(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.to_ascii_lowercase().starts_with("spotify:") {
        return true;
    }
    let Some(url) = normalize_media_url(trimmed) else {
        return false;
    };
    let lower = url.to_ascii_lowercase();
    HOSTS.iter().any(|host| lower.contains(host))
}

pub fn normalize_media_url(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.to_ascii_lowercase().starts_with("spotify:") {
        let mut parts = trimmed.split(':');
        let _ = parts.next();
        let kind = parts.next()?.to_ascii_lowercase();
        let id = parts.next()?;
        if !id.is_empty() && id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return Some(format!("https://open.spotify.com/{kind}/{id}"));
        }
        return None;
    }
    let with_scheme = if starts_with_ignore_ascii(trimmed, "https://")
        || starts_with_ignore_ascii(trimmed, "http://")
    {
        trimmed.to_string()
    } else if starts_with_ignore_ascii(trimmed, "www.") {
        format!("https://{trimmed}")
    } else {
        return None;
    };
    Some(strip_tracking(&with_scheme))
}

pub fn looks_like_playlist_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/playlist")
        || lower.contains("/album")
        || lower.contains("/sets/")
        || lower.contains("/music/playlist")
        || lower.contains("/music/album")
        || lower.contains("list=")
        || lower.contains("/artist/")
}

pub fn playlist_name_from_url(url: &str) -> String {
    let Some(path) = url
        .split('?')
        .next()
        .and_then(|head| head.split('#').next())
    else {
        return "Downloaded playlist".into();
    };
    let after_scheme = path.split("://").nth(1).unwrap_or(path);
    let parts: Vec<&str> = after_scheme
        .split('/')
        .filter(|part| !part.is_empty())
        .skip(1)
        .collect();
    for segment in parts.iter().rev() {
        let decoded = decode_component(segment).replace(['-', '_'], " ");
        let decoded = decoded.trim();
        if decoded.is_empty()
            || matches!(
                decoded.to_ascii_lowercase().as_str(),
                "track"
                    | "tracks"
                    | "playlist"
                    | "playlists"
                    | "album"
                    | "albums"
                    | "sets"
                    | "artist"
            )
        {
            continue;
        }
        if decoded.len() >= 10
            && !decoded.contains(' ')
            && decoded.chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            continue;
        }
        return clip(decoded, 80);
    }
    parts
        .last()
        .map(|segment| clip(&decode_component(segment), 80))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Downloaded playlist".into())
}

pub fn sanitize_folder_name(name: &str) -> String {
    let mut text = name
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control()
            {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text = text.trim_end_matches(['.', ' ']).trim().to_string();
    if text.is_empty() {
        text = "Playlist".into();
    }
    clip(&text, 100)
}

pub fn ytdlp_available() -> bool {
    ytdlp_path().is_some()
}

pub fn read_download_settings() -> (Option<String>, Option<String>) {
    let Ok(raw) = fs::read_to_string(settings_path()) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return (None, None);
    };
    let folder = value
        .get("download_folder")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let playlist = value
        .get("download_playlist_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    (folder, playlist)
}

pub fn effective_download_folder() -> String {
    read_download_settings().0.unwrap_or_else(|| {
        catalog::app_data_dir()
            .join("downloads")
            .to_string_lossy()
            .to_string()
    })
}

pub fn set_download_folder(folder: Option<&str>) -> Result<(), String> {
    write_setting("download_folder", folder)
}

pub fn set_download_playlist(id: Option<&str>) -> Result<(), String> {
    write_setting("download_playlist_id", id)
}

pub fn remember_download_playlist(id: &str) {
    let _ = set_download_playlist(Some(id));
}

/// Patches one settings.json key and leaves the rest of the file alone.
pub(crate) fn merge_setting(raw: &str, key: &str, value: Option<&str>) -> Result<String, String> {
    let raw = if raw.trim().is_empty() { "{}" } else { raw };
    let mut parsed: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let object = parsed
        .as_object_mut()
        .ok_or_else(|| "settings.json is not an object".to_string())?;
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            if object.get(key).and_then(Value::as_str) == Some(text) {
                return Ok(raw.to_string());
            }
            object.insert(key.to_string(), Value::String(text.to_string()));
        }
        None => {
            if object.remove(key).is_none() {
                return Ok(raw.to_string());
            }
        }
    }
    serde_json::to_string_pretty(&parsed).map_err(|error| error.to_string())
}

fn write_setting(key: &str, value: Option<&str>) -> Result<(), String> {
    let path = settings_path();
    let current = fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
    let text = merge_setting(&current, key, value)?;
    if text == current {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, text).map_err(|error| error.to_string())
}

pub fn download_dir(named: Option<&str>) -> Result<PathBuf, String> {
    let (folder, _) = read_download_settings();
    let base = match folder {
        Some(path) => PathBuf::from(path),
        None => catalog::app_data_dir().join("downloads"),
    };
    let dir = match named
        .map(sanitize_folder_name)
        .filter(|name| !name.is_empty())
    {
        Some(name) => base.join(name),
        None => base,
    };
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Could not create the download folder: {error}"))?;
    Ok(dir)
}

pub fn probe(url: &str) -> Result<Probe, String> {
    let url = normalize_media_url(url).ok_or_else(|| "Enter a valid media URL".to_string())?;
    if !is_media_url(&url) {
        return Err("URL is not recognized as a supported media link".into());
    }
    let binary = ytdlp_path().ok_or_else(missing_ytdlp)?;
    let mut args = base_args();
    args.extend([
        "--dump-single-json".into(),
        "--no-warnings".into(),
        "--no-download".into(),
        url,
    ]);
    let output = run_captured(&binary, &args)?;
    if !output.status.success() {
        return Err(summarize_failure(&String::from_utf8_lossy(&output.stderr)));
    }
    parse_probe(&String::from_utf8_lossy(&output.stdout))
}

pub fn download(
    url: &str,
    dir: &Path,
    allow_playlist: bool,
    snap: Arc<Mutex<JobSnap>>,
) -> Result<Vec<String>, String> {
    let url = normalize_media_url(url).ok_or_else(|| "Enter a valid media URL".to_string())?;
    if lock(&snap).cancel {
        return Err("Download cancelled".into());
    }
    let binary = ytdlp_path().ok_or_else(missing_ytdlp)?;
    let before = snapshot_mp3(dir);
    let template = format!(
        "{}/%(artist,uploader)s - %(title)s.%(ext)s",
        dir.to_string_lossy().trim_end_matches(['\\', '/'])
    );
    let mut args = base_args();
    args.extend([
        "--newline".into(),
        "--progress".into(),
        "--progress-template".into(),
        "download:muzeeka-progress:%(progress._percent_str)s".into(),
        "-x".into(),
        "--audio-format".into(),
        "mp3".into(),
        "--audio-quality".into(),
        "0".into(),
        "--embed-thumbnail".into(),
        "--convert-thumbnails".into(),
        "jpg".into(),
        "--ppa".into(),
        "ThumbnailsConvertor+ffmpeg_o:-vf crop=ih:ih".into(),
        "--ppa".into(),
        "EmbedThumbnail+ffmpeg_o:-id3v2_version 3".into(),
        "--ppa".into(),
        "Metadata+ffmpeg_o:-id3v2_version 3".into(),
        "--embed-metadata".into(),
        "--parse-metadata".into(),
        "%(artist,album_artist,uploader,channel,creator)s:%(artist)s".into(),
        "-o".into(),
        template,
        "--print".into(),
        "after_move:filepath".into(),
    ]);
    if !allow_playlist {
        args.push("--no-playlist".into());
    }
    args.push(url);

    let mut command = Command::new(&binary);
    command
        .args(&args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    muzeeka::audio::process_util::hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Failed to run yt-dlp: {error}"))?;
    {
        let mut guard = lock(&snap);
        guard.pid = child.id();
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let printed = Arc::new(Mutex::new(Vec::<String>::new()));
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let out_handle = {
        let snap = snap.clone();
        let printed = printed.clone();
        std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    note_line(&snap, &printed, &line);
                }
            }
        })
    };
    let err_handle = {
        let snap = snap.clone();
        let printed = printed.clone();
        let stderr_text = stderr_text.clone();
        std::thread::spawn(move || {
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    lock(&stderr_text).push_str(&line);
                    lock(&stderr_text).push('\n');
                    note_line(&snap, &printed, &line);
                }
            }
        })
    };
    let status = child
        .wait()
        .map_err(|error| format!("yt-dlp stopped unexpectedly: {error}"))?;
    let _ = out_handle.join();
    let _ = err_handle.join();
    if lock(&snap).cancel {
        return Err("Download cancelled".into());
    }
    if !status.success() {
        return Err(summarize_failure(&lock(&stderr_text)));
    }
    let mut files = lock(&printed);
    files.extend(new_mp3s(dir, &before));
    files.sort();
    files.dedup();
    let files: Vec<String> = files
        .drain(..)
        .filter(|path| Path::new(path).is_file())
        .collect();
    if files.is_empty() {
        return Err("No audio files were downloaded".into());
    }
    Ok(files)
}

pub fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    let mut command = Command::new("taskkill");
    command
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    muzeeka::audio::process_util::hide_console(&mut command);
    let _ = command.status();
}

pub fn fetch_thumbnail(url: &str) -> Option<PathBuf> {
    if !starts_with_ignore_ascii(url, "http://") && !starts_with_ignore_ascii(url, "https://") {
        return None;
    }
    let config = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = agent.get(url).header("User-Agent", "Muzeeka").call().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.into_body().read_to_vec().ok()?;
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    let dir = catalog::app_data_dir().join("download_thumbs");
    fs::create_dir_all(&dir).ok()?;
    let name = format!(
        "{}.webp",
        paths::path_key(url).chars().take(24).collect::<String>()
    );
    let dest = dir.join(sanitize_file_name(&name));
    let image = image::load_from_memory(&bytes).ok()?;
    image
        .thumbnail(96, 96)
        .save_with_format(&dest, image::ImageFormat::WebP)
        .ok()?;
    dest.is_file().then_some(dest)
}

fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn note_line(snap: &Mutex<JobSnap>, printed: &Mutex<Vec<String>>, line: &str) {
    let clean = strip_ansi(line);
    let clean = clean.trim();
    if clean.is_empty() {
        return;
    }
    if let Some(percent) = parse_progress(clean) {
        let mut guard = lock(snap);
        guard.percent = percent;
        guard.status = format!("Downloading… {percent:.0}%");
        return;
    }
    if looks_like_path(clean) {
        lock(printed).push(unquote(clean));
        return;
    }
    if clean.starts_with("ERROR:") || clean.starts_with("WARNING:") {
        return;
    }
    if clean.len() < 180 {
        lock(snap).status = clean.to_string();
    }
}

fn parse_progress(line: &str) -> Option<f32> {
    if let Some(rest) = line.strip_prefix("muzeeka-progress:") {
        return rest
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| (0.0..=100.0).contains(value));
    }
    let rest = line.strip_prefix("download:").unwrap_or(line).trim();
    if let Some(rest) = rest.strip_prefix("muzeeka-progress:") {
        return rest
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| (0.0..=100.0).contains(value));
    }
    if line.contains("[download]") && line.contains('%') {
        let percent_at = line.find('%')?;
        let before = &line[..percent_at];
        let start = before
            .rfind(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .map(|index| index + 1)
            .unwrap_or(0);
        return before[start..]
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| (0.0..=100.0).contains(value));
    }
    None
}

fn looks_like_path(line: &str) -> bool {
    let line = unquote(line);
    if line.contains('\n') || line.len() < 3 || line.len() > 520 {
        return false;
    }
    Path::new(&line).is_file()
        || line.contains(":\\")
        || line.starts_with("\\\\")
        || line.starts_with('/')
}

fn unquote(line: &str) -> String {
    line.trim().trim_matches('"').trim().to_string()
}

fn snapshot_mp3(dir: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return set;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
        {
            set.insert(paths::path_key(&path.to_string_lossy()));
        }
    }
    set
}

fn new_mp3s(dir: &Path, before: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
        {
            let key = paths::path_key(&path.to_string_lossy());
            if !before.contains(&key) {
                out.push(path.to_string_lossy().to_string());
            }
        }
    }
    out
}

fn parse_probe(raw: &str) -> Result<Probe, String> {
    let raw = raw.trim();
    let json = raw.find('{').map(|index| &raw[index..]).unwrap_or(raw);
    let value: Value = serde_json::from_str(json)
        .map_err(|error| format!("Failed to parse yt-dlp response: {error}"))?;
    let entries = value.get("entries").and_then(Value::as_array);
    if let Some(entries) = entries {
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .or_else(|| {
                entries
                    .first()
                    .and_then(|entry| entry.get("title"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Playlist".into());
        let count = value
            .get("playlist_count")
            .or_else(|| value.get("n_entries"))
            .and_then(Value::as_u64)
            .map(|count| count as u32)
            .or(Some(entries.len() as u32));
        return Ok(Probe {
            title,
            uploader: string_field(&value, "uploader"),
            duration_secs: None,
            thumbnail: string_field(&value, "thumbnail"),
            is_playlist: true,
            entry_count: count.filter(|count| *count > 0),
        });
    }
    let uploader = string_field(&value, "uploader");
    let title = clean_title(
        string_field(&value, "title").unwrap_or_else(|| "Unknown".into()),
        uploader.as_deref(),
    );
    Ok(Probe {
        title,
        uploader,
        duration_secs: value
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|secs| *secs > 0.0),
        thumbnail: string_field(&value, "thumbnail"),
        is_playlist: false,
        entry_count: None,
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn clean_title(title: String, uploader: Option<&str>) -> String {
    let title = strip_id_suffix(&title);
    match uploader {
        Some(artist) => strip_artist_prefix(&title, artist).unwrap_or(title),
        None => title,
    }
}

fn strip_id_suffix(value: &str) -> String {
    let trimmed = value.trim();
    let Some(open) = trimmed.rfind(" [") else {
        return trimmed.to_string();
    };
    if !trimmed.ends_with(']') {
        return trimmed.to_string();
    }
    let inside = &trimmed[open + 2..trimmed.len() - 1];
    if inside.is_empty() || !inside.chars().all(|ch| ch.is_ascii_digit()) {
        return trimmed.to_string();
    }
    trimmed[..open].trim().to_string()
}

fn strip_artist_prefix(title: &str, artist: &str) -> Option<String> {
    let title = title.trim();
    let artist = artist.trim();
    if title.is_empty() || artist.is_empty() {
        return None;
    }
    for separator in [" - ", " — ", " – ", " − ", " | "] {
        let prefix = format!("{artist}{separator}");
        if let Some(rest) = strip_prefix_ignore_case(title, &prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn strip_prefix_ignore_case<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    let mut end = 0usize;
    let mut hay = haystack.char_indices();
    let mut pin = needle.chars();
    loop {
        match (hay.next(), pin.next()) {
            (Some((index, left)), Some(right)) => {
                if left.to_lowercase().ne(right.to_lowercase()) {
                    return None;
                }
                end = index + left.len_utf8();
            }
            (_, Some(_)) => return None,
            (Some(_), None) | (None, None) => return Some(&haystack[end..]),
        }
    }
}

fn summarize_failure(stderr: &str) -> String {
    let mut last_error = None;
    let mut bot_check = false;
    let mut no_js = false;
    for line in stderr.lines() {
        let line = strip_ansi(line);
        let lower = line.to_ascii_lowercase();
        if lower.contains("sign in to confirm") || lower.contains("not a bot") {
            bot_check = true;
        }
        if lower.contains("no supported javascript runtime") {
            no_js = true;
        }
        if let Some(rest) = line.trim().strip_prefix("ERROR:") {
            let rest = rest.trim();
            if !rest.is_empty() {
                last_error = Some(rest.to_string());
            }
        }
    }
    if bot_check {
        return "YouTube blocked the download (bot check). Sign in to YouTube and try again."
            .into();
    }
    if no_js {
        return "YouTube requires Deno or Node.js to download. Muzeeka could not find either."
            .into();
    }
    last_error
        .map(|error| {
            let cut = error.split(" See  http").next().unwrap_or(&error).trim();
            if cut.len() > 220 {
                format!("{}…", &cut[..220])
            } else if cut.is_empty() {
                "yt-dlp download failed".into()
            } else {
                cut.to_string()
            }
        })
        .unwrap_or_else(|| "yt-dlp download failed".into())
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if ch != '\r' {
            out.push(ch);
        }
    }
    out
}

fn base_args() -> Vec<String> {
    let mut args = vec!["--encoding".into(), "utf-8".into()];
    if let Some(dir) = ffmpeg_dir() {
        args.push("--ffmpeg-location".into());
        args.push(dir.to_string_lossy().to_string());
    }
    if let Some((runtime, path)) = js_runtime() {
        args.push("--js-runtimes".into());
        args.push(format!("{}:{}", runtime, path.display()));
    }
    args
}

fn run_captured(binary: &Path, args: &[String]) -> Result<std::process::Output, String> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    muzeeka::audio::process_util::hide_console(&mut command);
    command
        .output()
        .map_err(|error| format!("Failed to run yt-dlp: {error}"))
}

fn ytdlp_path() -> Option<PathBuf> {
    find_tool("yt-dlp")
}

fn ffmpeg_dir() -> Option<PathBuf> {
    find_tool("ffmpeg").and_then(|path| path.parent().map(Path::to_path_buf))
}

fn js_runtime() -> Option<(String, PathBuf)> {
    if let Some(path) = find_tool("deno") {
        return Some(("deno".into(), path));
    }
    let node = find_tool("node")?;
    let major = node_major(&node)?;
    (major >= 20).then_some(("node".into(), node))
}

fn node_major(path: &Path) -> Option<u32> {
    let mut command = Command::new(path);
    command
        .arg("-v")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    muzeeka::audio::process_util::hide_console(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim().trim_start_matches('v');
    text.split('.').next()?.parse().ok()
}

fn find_tool(stem: &str) -> Option<PathBuf> {
    let file_name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    };
    for dir in tool_dirs() {
        let candidate = dir.join(&file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which(&file_name)
}

/// ffmpeg, ffprobe, and yt-dlp live in `bin` next to the executable.
/// A copy beside the exe still works. `src/bin` is only searched in debug builds.
fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("bin"));
            dirs.push(dir.to_path_buf());
        }
    }
    if cfg!(debug_assertions) {
        dirs.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("bin"),
        );
        dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin"));
    }
    dirs
}

fn which(file_name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn missing_ytdlp() -> String {
    "yt-dlp was not found. Place yt-dlp.exe in bin next to Muzeeka.".into()
}

fn settings_path() -> PathBuf {
    catalog::app_data_dir().join("settings.json")
}

fn strip_tracking(url: &str) -> String {
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, query),
        None => return url.to_string(),
    };
    let (query, fragment) = match query.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (query, None),
    };
    let mut kept = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let key = pair.split('=').next().unwrap_or(pair);
        let decoded = decode_component(key).to_ascii_lowercase();
        if DROP_QUERY.contains(&decoded.as_str()) || decoded.starts_with("utm_") {
            continue;
        }
        kept.push(pair);
    }
    let mut out = head.to_string();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn decode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[index + 1..index + 3], 16) {
                out.push(value as char);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            out.push(' ');
        } else {
            out.push(bytes[index] as char);
        }
        index += 1;
    }
    out
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn starts_with_ignore_ascii(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_url_keeps_youtube_id_and_drops_tracking() {
        let url =
            normalize_media_url("https://www.youtube.com/watch?v=abc123&utm_source=share&si=xyz")
                .expect("url");
        assert!(url.contains("v=abc123"));
        assert!(!url.contains("utm_source"));
        assert!(!url.contains("si="));
        assert!(is_media_url(&url));
    }

    #[test]
    fn spotify_uri_becomes_https() {
        let url = normalize_media_url("spotify:track:0123456789abcDEF").expect("spotify");
        assert_eq!(url, "https://open.spotify.com/track/0123456789abcDEF");
        assert!(is_media_url("spotify:track:0123456789abcDEF"));
    }

    #[test]
    fn plain_http_is_not_a_media_link() {
        assert!(!is_media_url("https://example.com/file"));
    }

    #[test]
    fn playlist_query_counts() {
        assert!(looks_like_playlist_url(
            "https://www.youtube.com/playlist?list=PL123"
        ));
    }

    #[test]
    fn download_setting_patch_keeps_other_keys() {
        let raw = r#"{"discord_rpc_enabled":false,"download_folder":"D:\\in","dsp_chain":[]}"#;
        let out = merge_setting(raw, "download_playlist_id", Some("pl-1")).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["discord_rpc_enabled"], false);
        assert_eq!(value["download_folder"], "D:\\in");
        assert_eq!(value["download_playlist_id"], "pl-1");
        assert!(value["dsp_chain"].is_array());
        let cleared = merge_setting(&out, "download_folder", None).unwrap();
        let value: Value = serde_json::from_str(&cleared).unwrap();
        assert!(value.get("download_folder").is_none());
        assert_eq!(value["download_playlist_id"], "pl-1");
    }
}
