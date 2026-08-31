// YouTube sign-in for yt-dlp. Chrome/Edge cookies cannot be read while the
// browser is open (and Chrome 127+ uses app-bound encryption), so Muzeeka
// keeps its own WebView2 session — same pattern as VK login.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Desktop Chrome UA without `Edg/` / WebView tokens. Google blocks those and
/// serves an empty document to the login window.
const LOGIN_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

const LOGIN_URL: &str =
    "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F";

pub const YOUTUBE_LOGIN_WINDOW_LABEL: &str = "youtube-login";

const LOGIN_POLL_MS: u64 = 700;
const LOGIN_POLL_TICKS: u32 = 360;

static LOGIN_CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoutubeAuthStatus {
    pub logged_in: bool,
}

#[derive(Debug, Clone)]
struct NetscapeCookie {
    domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    expires: u64,
    name: String,
    value: String,
}

pub fn cancel_login() {
    LOGIN_CANCELLED.store(true, Ordering::SeqCst);
}

pub fn cookie_file_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("youtube_cookies.txt"))
}

/// Path to a Netscape cookie file that actually has YouTube login cookies.
///
/// yt-dlp writes the jar back to `--cookies`, so we hand it a copy and keep
/// the Muzeeka session file intact.
pub fn cookies_arg(app: &AppHandle) -> Option<String> {
    let path = cookie_file_path(app)?;
    if !path.is_file() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    let cookies = parse_netscape_cookies(&raw);
    if !has_youtube_login(&cookies) {
        eprintln!(
            "[youtube_auth] cookie file present but missing YouTube LOGIN_INFO — not passing --cookies"
        );
        return None;
    }
    let runtime = path.with_file_name("youtube_cookies.runtime.txt");
    fs::copy(&path, &runtime).ok()?;
    Some(runtime.to_string_lossy().to_string())
}

pub fn auth_status(app: &AppHandle) -> YoutubeAuthStatus {
    YoutubeAuthStatus {
        logged_in: cookies_arg(app).is_some(),
    }
}

fn logged_out() -> YoutubeAuthStatus {
    YoutubeAuthStatus { logged_in: false }
}

pub fn is_youtube_related_domain(domain: &str) -> bool {
    let d = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    d == "youtube.com"
        || d.ends_with(".youtube.com")
        || d == "youtu.be"
        || d.ends_with(".youtu.be")
        || d == "youtube-nocookie.com"
        || d.ends_with(".youtube-nocookie.com")
        || d == "googlevideo.com"
        || d.ends_with(".googlevideo.com")
        || d == "google.com"
        || d.ends_with(".google.com")
        || d == "googleapis.com"
        || d.ends_with(".googleapis.com")
        || d == "gstatic.com"
        || d.ends_with(".gstatic.com")
        || d == "ytimg.com"
        || d.ends_with(".ytimg.com")
        || d == "ggpht.com"
        || d.ends_with(".ggpht.com")
}

pub fn is_youtube_login_cookie(name: &str) -> bool {
    matches!(
        name,
        "LOGIN_INFO"
            | "SID"
            | "HSID"
            | "SSID"
            | "APISID"
            | "SAPISID"
            | "__Secure-1PSID"
            | "__Secure-3PSID"
            | "__Secure-1PAPISID"
            | "__Secure-3PAPISID"
            | "__Secure-1PSIDTS"
            | "__Secure-3PSIDTS"
    )
}

fn is_youtube_site_domain(domain: &str) -> bool {
    let d = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    d == "youtube.com" || d.ends_with(".youtube.com") || d == "youtu.be"
}

fn is_google_site_domain(domain: &str) -> bool {
    let d = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    d == "google.com" || d.ends_with(".google.com")
}

fn is_plausible_session_value(value: &str) -> bool {
    let v = value.trim();
    v.len() >= 16 && !v.eq_ignore_ascii_case("deleted") && v != "0"
}

/// YouTube's own session cookie, not merely a Google SID from accounts.google.com.
fn has_youtube_login(cookies: &[NetscapeCookie]) -> bool {
    cookies.iter().any(|c| {
        c.name == "LOGIN_INFO"
            && is_youtube_site_domain(&c.domain)
            && is_plausible_session_value(&c.value)
    })
}

fn session_expiry() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_add(400 * 24 * 3600))
        .unwrap_or(1_893_456_000)
}

fn normalize_cookie_domain(raw: &str, name: &str) -> String {
    let t = raw.trim().to_ascii_lowercase();
    if t.is_empty() {
        return t;
    }
    // __Host- cookies must be host-only (no leading dot).
    if name.starts_with("__Host-") {
        return t.trim_start_matches('.').to_string();
    }
    if t.starts_with('.') || t.starts_with("www.") {
        return t;
    }
    format!(".{t}")
}

fn default_domain_for_name(name: &str) -> Option<&'static str> {
    match name {
        "LOGIN_INFO" | "VISITOR_INFO1_LIVE" | "YSC" | "PREF" | "CONSISTENCY" => {
            Some(".youtube.com")
        }
        "SID" | "HSID" | "SSID" | "APISID" | "SAPISID" | "__Secure-1PSID" | "__Secure-3PSID"
        | "__Secure-1PAPISID" | "__Secure-3PAPISID" | "__Secure-1PSIDTS" | "__Secure-3PSIDTS" => {
            Some(".google.com")
        }
        _ => None,
    }
}

fn format_netscape_line(c: &NetscapeCookie) -> Option<String> {
    if c.name.is_empty() || c.value.is_empty() {
        return None;
    }
    if c.name.contains(['\t', '\n', '\r']) || c.value.contains(['\t', '\n', '\r']) {
        return None;
    }
    let domain = c.domain.trim();
    if domain.is_empty() {
        return None;
    }
    let include_sub = if domain.starts_with('.') {
        "TRUE"
    } else {
        "FALSE"
    };
    let secure = if c.secure { "TRUE" } else { "FALSE" };
    let prefix = if c.http_only { "#HttpOnly_" } else { "" };
    let path = if c.path.is_empty() { "/" } else { c.path.as_str() };
    Some(format!(
        "{prefix}{domain}\t{include_sub}\t{path}\t{secure}\t{}\t{}\t{}",
        c.expires, c.name, c.value
    ))
}

fn write_netscape_cookies(path: &Path, cookies: &[NetscapeCookie]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create cookie dir: {e}"))?;
    }
    let mut out = String::from("# Netscape HTTP Cookie File\n# Muzeeka YouTube session\n");
    for cookie in cookies {
        if let Some(line) = format_netscape_line(cookie) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    fs::write(path, out).map_err(|e| format!("Failed to write YouTube cookies: {e}"))
}

fn parse_netscape_cookies(raw: &str) -> Vec<NetscapeCookie> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut line = line.trim();
        if line.is_empty() {
            continue;
        }
        let http_only = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
            line = rest;
            true
        } else if line.starts_with('#') {
            continue;
        } else {
            false
        };
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 7 {
            continue;
        }
        out.push(NetscapeCookie {
            domain: parts[0].to_string(),
            path: parts[2].to_string(),
            secure: parts[3].eq_ignore_ascii_case("TRUE"),
            http_only,
            expires: parts[4].parse().unwrap_or(0),
            name: parts[5].to_string(),
            value: parts[6].to_string(),
        });
    }
    out
}

fn ingest_tauri_cookie(
    map: &mut HashMap<(String, String), NetscapeCookie>,
    c: tauri::webview::Cookie<'static>,
) {
    let name = c.name().to_string();
    let value = c.value().to_string();
    if name.is_empty() || value.is_empty() || value.eq_ignore_ascii_case("deleted") {
        return;
    }

    let domain_raw = c.domain().unwrap_or("").to_string();
    let domain = if !domain_raw.trim().is_empty() {
        if !is_youtube_related_domain(&domain_raw) {
            return;
        }
        normalize_cookie_domain(&domain_raw, &name)
    } else if let Some(fallback) = default_domain_for_name(&name) {
        fallback.to_string()
    } else {
        return;
    };

    let path = c.path().unwrap_or("/").to_string();
    map.insert(
        (domain.clone(), name.clone()),
        NetscapeCookie {
            domain,
            path: if path.is_empty() {
                "/".to_string()
            } else {
                path
            },
            secure: c.secure().unwrap_or(true),
            http_only: c.http_only().unwrap_or(false),
            expires: session_expiry(),
            name,
            value,
        },
    );
}

const YOUTUBE_MIRROR_NAMES: &[&str] = &[
    "SID",
    "HSID",
    "SSID",
    "APISID",
    "SAPISID",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "__Secure-1PAPISID",
    "__Secure-3PAPISID",
    "__Secure-1PSIDCC",
    "__Secure-3PSIDCC",
    "__Secure-1PSIDTS",
    "__Secure-3PSIDTS",
];

/// yt-dlp examples put Google auth cookies on `.youtube.com` as well as `.google.com`.
fn mirror_google_auth_onto_youtube(cookies: &mut Vec<NetscapeCookie>) {
    let google: Vec<NetscapeCookie> = cookies
        .iter()
        .filter(|c| is_google_site_domain(&c.domain) && YOUTUBE_MIRROR_NAMES.contains(&c.name.as_str()))
        .cloned()
        .collect();
    for src in google {
        let already = cookies
            .iter()
            .any(|c| is_youtube_site_domain(&c.domain) && c.name == src.name);
        if already {
            continue;
        }
        cookies.push(NetscapeCookie {
            domain: ".youtube.com".to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: src.http_only,
            expires: src.expires,
            name: src.name,
            value: src.value,
        });
    }
}

fn collect_youtube_cookies_from_window(
    window: &tauri::WebviewWindow,
) -> Vec<NetscapeCookie> {
    let mut map = HashMap::<(String, String), NetscapeCookie>::new();

    match window.cookies() {
        Ok(cookies) => {
            eprintln!("[youtube_auth] cookies() count={}", cookies.len());
            for c in cookies {
                ingest_tauri_cookie(&mut map, c);
            }
        }
        Err(e) => eprintln!("[youtube_auth] cookies() error: {e}"),
    }

    for raw in [
        "https://www.youtube.com/",
        "https://youtube.com/",
        "https://m.youtube.com/",
        "https://music.youtube.com/",
        "https://accounts.google.com/",
        "https://www.google.com/",
        "https://google.com/",
    ] {
        if let Ok(url) = raw.parse() {
            if let Ok(cookies) = window.cookies_for_url(url) {
                for c in cookies {
                    ingest_tauri_cookie(&mut map, c);
                }
            }
        }
    }

    map.into_values().collect()
}

async fn open_login_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(existing) = app.get_webview_window(YOUTUBE_LOGIN_WINDOW_LABEL) {
        let _ = existing.close();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let url: tauri::Url = LOGIN_URL
        .parse()
        .map_err(|e| format!("Invalid YouTube URL: {e}"))?;

    let window =
        WebviewWindowBuilder::new(app, YOUTUBE_LOGIN_WINDOW_LABEL, WebviewUrl::External(url.clone()))
            .title("YouTube Login — Muzeeka")
            .inner_size(920.0, 740.0)
            .resizable(true)
            .center()
            .visible(true)
            .focused(true)
            .background_color(tauri::webview::Color(255, 255, 255, 255))
            .user_agent(LOGIN_USER_AGENT)
            .on_navigation(|url| {
                eprintln!("[youtube_auth] navigate {url}");
                true
            })
            .build()
            .map_err(|e| format!("Failed to open YouTube login window: {e}"))?;

    // WebView2 sometimes creates the HWND first and only then loads; poke navigation.
    let _ = window.navigate(url);
    Ok(window)
}

fn close_login_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(YOUTUBE_LOGIN_WINDOW_LABEL) {
        let _ = window.close();
    }
}

fn emit_status(app: &AppHandle, status: &YoutubeAuthStatus) {
    let _ = app.emit("youtube:auth-changed", status);
}

/// Open YouTube in a webview and wait until login cookies appear (or the window closes).
/// `force` re-opens the window even if a cookie file already exists (stale session).
pub async fn login(app: AppHandle, force: bool) -> Result<YoutubeAuthStatus, String> {
    if !force {
        let current = auth_status(&app);
        if current.logged_in {
            return Ok(current);
        }
    } else if let Some(path) = cookie_file_path(&app) {
        let _ = fs::remove_file(path);
    }

    LOGIN_CANCELLED.store(false, Ordering::SeqCst);
    let _window = open_login_window(&app).await?;
    let mut last_err = String::from("Waiting for YouTube sign-in…");

    for tick in 0..LOGIN_POLL_TICKS {
        tokio::time::sleep(Duration::from_millis(LOGIN_POLL_MS)).await;

        if LOGIN_CANCELLED.load(Ordering::SeqCst) {
            close_login_window(&app);
            return Err("YouTube sign-in cancelled".to_string());
        }

        let Some(window) = app.get_webview_window(YOUTUBE_LOGIN_WINDOW_LABEL) else {
            return Err("YouTube login window was closed".to_string());
        };

        let mut cookies = collect_youtube_cookies_from_window(&window);
        if !has_youtube_login(&cookies) {
            last_err = "Waiting for YouTube homepage after sign-in…".to_string();
            if tick % 5 == 0 {
                let names: Vec<&str> = cookies.iter().map(|c| c.name.as_str()).collect();
                eprintln!(
                    "[youtube_auth] tick={tick} {} cookies; names={}",
                    cookies.len(),
                    names.join(", ")
                );
            }
            continue;
        }

        mirror_google_auth_onto_youtube(&mut cookies);

        let path = cookie_file_path(&app).ok_or_else(|| "No app data dir".to_string())?;
        write_netscape_cookies(&path, &cookies)?;
        let yt_names: Vec<&str> = cookies
            .iter()
            .filter(|c| is_youtube_site_domain(&c.domain))
            .map(|c| c.name.as_str())
            .collect();
        eprintln!(
            "[youtube_auth] saved {} cookies (LOGIN_INFO ok); youtube names: {}",
            cookies.len(),
            yt_names.join(", ")
        );

        let status = YoutubeAuthStatus { logged_in: true };
        emit_status(&app, &status);
        close_login_window(&app);
        return Ok(status);
    }

    close_login_window(&app);
    Err(format!(
        "YouTube sign-in timed out. {last_err} Sign in, then stay on youtube.com until this window closes."
    ))
}

pub async fn logout(app: AppHandle) -> Result<YoutubeAuthStatus, String> {
    if let Some(path) = cookie_file_path(&app) {
        let _ = fs::remove_file(path);
    }

    if let Some(window) = app.get_webview_window(YOUTUBE_LOGIN_WINDOW_LABEL) {
        let _ = window.clear_all_browsing_data();
        let _ = window.close();
    } else {
        let url = "https://www.youtube.com/"
            .parse()
            .map_err(|e| format!("Invalid URL: {e}"))?;
        if let Ok(window) = WebviewWindowBuilder::new(
            &app,
            YOUTUBE_LOGIN_WINDOW_LABEL,
            WebviewUrl::External(url),
        )
        .title("YouTube Logout")
        .inner_size(1.0, 1.0)
        .visible(false)
        .build()
        {
            let w = window.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || w.clear_all_browsing_data()).await;
            let _ = window.close();
        }
    }

    let status = logged_out();
    emit_status(&app, &status);
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, name: &str, value: &str, http_only: bool) -> NetscapeCookie {
        NetscapeCookie {
            domain: domain.to_string(),
            path: "/".to_string(),
            secure: true,
            http_only,
            expires: 0,
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn related_domains() {
        assert!(is_youtube_related_domain(".youtube.com"));
        assert!(is_youtube_related_domain("www.youtube.com"));
        assert!(is_youtube_related_domain(".google.com"));
        assert!(!is_youtube_related_domain("example.com"));
        assert!(is_youtube_login_cookie("LOGIN_INFO"));
        assert!(is_youtube_login_cookie("SAPISID"));
        assert!(!is_youtube_login_cookie("VISITOR_INFO1_LIVE"));
    }

    #[test]
    fn login_detection_needs_youtube_login_info() {
        let visitor = vec![cookie(".youtube.com", "VISITOR_INFO1_LIVE", "abc", false)];
        assert!(!has_youtube_login(&visitor));

        let google_only = vec![
            cookie(".google.com", "SID", "sid-value-at-least-16", true),
            cookie(".google.com", "SAPISID", "sapisid-value-16", true),
            cookie(".google.com", "__Secure-1PSID", "psid-value-at-16", true),
        ];
        assert!(!has_youtube_login(&google_only));

        let login = vec![cookie(
            ".youtube.com",
            "LOGIN_INFO",
            "AFmmF2swRQIhANexamplevalue12",
            true,
        )];
        assert!(has_youtube_login(&login));
    }

    #[test]
    fn netscape_line_httponly() {
        let line = format_netscape_line(&cookie(".youtube.com", "LOGIN_INFO", "abc", true)).unwrap();
        assert!(line.starts_with("#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tLOGIN_INFO\tabc"));
    }

    #[test]
    fn netscape_roundtrip() {
        let raw = "# Netscape HTTP Cookie File\n#HttpOnly_.google.com\tTRUE\t/\tTRUE\t0\tSID\tsecret\n";
        let parsed = parse_netscape_cookies(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "SID");
        assert!(parsed[0].http_only);
        assert!(!has_youtube_login(&parsed));
    }
}
