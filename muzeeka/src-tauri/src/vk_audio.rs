// VK Music downloader — based on VK Music Saver / vk_api audio pipeline.
//
// Flow:
// 1. Parse vk.com / vk.ru audio & playlist URLs
// 2. Load session cookies from app login (Settings) / vk_cookies.txt / browser
// 3. Playlist catalog via m.vk load_section; streams via session WebView
//    audio.getById / al_audio (plain HTTP reload_audio returns antibot stubs)
// 4. Decode audio_api_unavailable links, rewrite m3u8 → mp3 when possible
// 5. Download (direct or ffmpeg HLS) into the same download folder as yt-dlp

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::library;
use crate::metadata;
use crate::ytdlp::{self, YtdlpDownloadResult, YtdlpProbeResult, YtdlpProgress};

const USER_AGENT: &str =
    "Mozilla/5.0 (Linux; Android 13; Pixel 6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";
const DESKTOP_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

const VK_STR: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN0PQRSTUVWXYZO123456789+/=";

static CANCELLED: AtomicBool = AtomicBool::new(false);
static HTTP: OnceLock<ureq::Agent> = OnceLock::new();

fn http() -> &'static ureq::Agent {
    HTTP.get_or_init(|| {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(45)))
            .build();
        ureq::Agent::new_with_config(config)
    })
}

pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}

fn check_cancel() -> Result<(), String> {
    // Only our flag — DOWNLOAD_CANCELLED is shared with yt-dlp and must not
    // bleed a previous cancel into a new VK download.
    if CANCELLED.load(Ordering::SeqCst) {
        Err("Download cancelled".to_string())
    } else {
        Ok(())
    }
}

/// True for VK music links that yt-dlp cannot handle (audio/playlist), not plain videos.
pub fn is_vk_audio_url(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    if !(lower.contains("vk.com") || lower.contains("vk.ru")) {
        return false;
    }
    // Video URLs stay on yt-dlp.
    if lower.contains("/video") || lower.contains("z=video") || lower.contains("video_ext.php") {
        return false;
    }
    lower.contains("/audio")
        || lower.contains("z=audio")
        || lower.contains("/music/")
        || lower.contains("audio_playlist")
        || lower.contains("/audios")
}

#[derive(Debug, Clone)]
enum VkTarget {
    Track {
        owner_id: i64,
        audio_id: u64,
        access_key: Option<String>,
    },
    Playlist {
        owner_id: i64,
        playlist_id: u64,
        access_hash: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct VkTrack {
    owner_id: i64,
    id: u64,
    title: String,
    artist: String,
    duration: u32,
    url: String,
    covers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ReloadResponse {
    data: Option<Vec<Value>>,
}

fn re_track() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // access_key is hex-ish but may include full alphanumeric
        Regex::new(r"(?i)(?:^|[/&=?])audio(-?\d+)_(\d+)(?:_([0-9a-zA-Z]+))?").expect("track re")
    })
}

fn re_playlist() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:music/(?:album|playlist)/|audio_playlist|playlist/)(-?\d+)_(\d+)(?:_([0-9a-zA-Z]+))?",
        )
        .expect("playlist re")
    })
}

fn re_m3u8_to_mp3() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"/[0-9a-f]+(/audios)?/([0-9a-f]+)/index\.m3u8").expect("m3u8 re")
    })
}

fn re_user_id() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Prefer explicit vk.id / window.vk id — avoid matching random "id" fields on guest pages.
    RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            (?:
                window\.vk\s*=\s*\{[^}]{0,400}?"id"\s*:\s*(\d+)
                | vk\.id\s*[:=]\s*(\d+)
                | "vk_user_id"\s*:\s*(\d+)
                | "uid"\s*:\s*(\d+)
            )
            "#,
        )
        .expect("user id re")
    })
}

fn extract_user_id_from_html(html: &str) -> Option<i64> {
    // Guest pages often have id:0 — never treat that as logged in.
    for caps in re_user_id().captures_iter(html) {
        for i in 1..=caps.len().saturating_sub(1) {
            if let Some(m) = caps.get(i) {
                if let Ok(id) = m.as_str().parse::<i64>() {
                    if id > 0 {
                        return Some(id);
                    }
                }
            }
        }
    }
    None
}

fn looks_like_login_page(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("name=\"email\"")
        || lower.contains("name=\"pass\"")
        || lower.contains("data-test-id=\"login\"")
        || lower.contains("act=login")
        || lower.contains("oauth.vk.com")
        || lower.contains("id.vk.com/auth")
        || (lower.contains("login") && lower.contains("password") && !lower.contains("logout"))
}

fn parse_target(url: &str) -> Result<VkTarget, String> {
    let trimmed = url.trim();

    // Prefer playlist patterns first (music/album overlaps less with track).
    if let Some(caps) = re_playlist().captures(trimmed) {
        let owner_id: i64 = caps[1]
            .parse()
            .map_err(|_| "Invalid VK playlist owner id".to_string())?;
        let playlist_id: u64 = caps[2]
            .parse()
            .map_err(|_| "Invalid VK playlist id".to_string())?;
        let access_hash = caps.get(3).map(|m| m.as_str().to_string());
        return Ok(VkTarget::Playlist {
            owner_id,
            playlist_id,
            access_hash,
        });
    }

    if let Some(caps) = re_track().captures(trimmed) {
        let owner_id: i64 = caps[1]
            .parse()
            .map_err(|_| "Invalid VK audio owner id".to_string())?;
        let audio_id: u64 = caps[2]
            .parse()
            .map_err(|_| "Invalid VK audio id".to_string())?;
        let access_key = caps.get(3).map(|m| m.as_str().to_string());
        return Ok(VkTarget::Track {
            owner_id,
            audio_id,
            access_key,
        });
    }

    Err(
        "Unsupported VK music URL. Use a track link (vk.com/audio…) or playlist/album link."
            .to_string(),
    )
}

// ── Auth / cookies ───────────────────────────────────────────────────────────

pub const VK_LOGIN_WINDOW_LABEL: &str = "vk-login";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkAuthStatus {
    pub logged_in: bool,
    pub user_id: Option<i64>,
    pub user_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VkSessionMeta {
    user_id: i64,
    user_name: Option<String>,
}

fn cookie_file_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("vk_cookies.txt"))
}

fn session_meta_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("vk_session.json"))
}

fn load_session_meta(app: &AppHandle) -> Option<VkSessionMeta> {
    let path = session_meta_path(app)?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_session_meta(app: &AppHandle, meta: &VkSessionMeta) -> Result<(), String> {
    let path = session_meta_path(app).ok_or_else(|| "No app data dir".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create app data: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    fs::write(&path, raw).map_err(|e| format!("Failed to save VK session: {e}"))
}

fn clear_session_meta(app: &AppHandle) {
    if let Some(path) = session_meta_path(app) {
        let _ = fs::remove_file(path);
    }
}

/// Real VK auth cookies only. Tracking cookies like remixstid / remixstlid do NOT mean login.
fn is_auth_cookie_name(name: &str) -> bool {
    name == "remixsid" || name == "remixnsid"
}

fn is_plausible_session_value(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("deleted") || v == "0" || v == "null" {
        return false;
    }
    // Guest / placeholder values are short; real remixsid is a long opaque string.
    v.len() >= 16
}

fn has_session_cookie_pairs(pairs: &[(String, String)]) -> bool {
    pairs.iter().any(|(k, v)| is_auth_cookie_name(k) && is_plausible_session_value(v))
}

fn pairs_to_header(pairs: &[(String, String)]) -> String {
    let mut map = std::collections::HashMap::<String, String>::new();
    for (k, v) in pairs {
        map.insert(k.clone(), v.clone());
    }
    map.entry("remixaudio_show_alert_today".into())
        .or_insert_with(|| "0".into());
    map.entry("remixmdevice".into())
        .or_insert_with(|| "1920/1080/2/!!-!!!!".into());
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn write_netscape_cookies(path: &Path, pairs: &[(String, String)]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create cookie dir: {e}"))?;
    }
    let mut out = String::from("# Netscape HTTP Cookie File\n# Muzeeka VK session\n");
    for (name, value) in pairs {
        if name.is_empty() || value.is_empty() {
            continue;
        }
        // domain, include_subdomains, path, secure, expiry, name, value
        out.push_str(&format!(
            ".vk.ru\tTRUE\t/\tTRUE\t0\t{name}\t{value}\n"
        ));
        out.push_str(&format!(
            ".vk.com\tTRUE\t/\tTRUE\t0\t{name}\t{value}\n"
        ));
    }
    fs::write(path, out).map_err(|e| format!("Failed to write VK cookies: {e}"))
}

fn logged_out_status() -> VkAuthStatus {
    VkAuthStatus {
        logged_in: false,
        user_id: None,
        user_name: None,
    }
}

/// Current VK login status (saved session).
pub fn auth_status(app: &AppHandle) -> VkAuthStatus {
    let path = cookie_file_path(app);
    let has_file = path
        .as_ref()
        .map(|p| p.is_file())
        .unwrap_or(false);

    if !has_file {
        return logged_out_status();
    }

    let pairs = path
        .as_ref()
        .map(|p| load_cookies_from_file(p))
        .unwrap_or_default();

    if !has_session_cookie_pairs(&pairs) {
        // Stale guest cookie dump — clean up.
        if let Some(p) = path {
            let _ = fs::remove_file(p);
        }
        clear_session_meta(app);
        return logged_out_status();
    }

    // Valid remixsid on disk = logged in. Do not HTTP-verify (antibot flaky).
    if let Some(meta) = load_session_meta(app) {
        if meta.user_id > 0 {
            return VkAuthStatus {
                logged_in: true,
                user_id: Some(meta.user_id),
                user_name: meta.user_name,
            };
        }
    }

    VkAuthStatus {
        logged_in: true,
        user_id: None,
        user_name: None,
    }
}

fn collect_vk_cookies_from_window(
    window: &tauri::WebviewWindow,
) -> Result<Vec<(String, String)>, String> {
    let mut map = std::collections::HashMap::<String, String>::new();

    let ingest_one =
        |map: &mut std::collections::HashMap<String, String>, c: tauri::webview::Cookie<'static>| {
            let domain = c.domain().unwrap_or("").to_ascii_lowercase();
            let name = c.name().to_string();
            let value = c.value().to_string();
            if name.is_empty() || value.is_empty() {
                return;
            }
            if domain.contains("vk.com")
                || domain.contains("vk.ru")
                || domain.contains("vk.me")
                || domain.is_empty()
                || name.starts_with("remix")
                || name.contains("sid")
            {
                map.insert(name, value);
            }
        };

    // Call cookie APIs directly (Tauri dispatches to the webview thread).
    // Do NOT wrap in spawn_blocking — WebView2 cookie store can return empty there.
    match window.cookies() {
        Ok(cookies) => {
            eprintln!("[vk_auth] cookies() count={}", cookies.len());
            for c in cookies {
                ingest_one(&mut map, c);
            }
        }
        Err(e) => eprintln!("[vk_auth] cookies() error: {e}"),
    }

    for raw in [
        "https://vk.com/",
        "https://vk.ru/",
        "https://m.vk.com/",
        "https://m.vk.ru/",
        "https://id.vk.com/",
        "https://id.vk.ru/",
        "https://login.vk.com/",
        "https://login.vk.ru/",
    ] {
        if let Ok(url) = raw.parse() {
            if let Ok(cookies) = window.cookies_for_url(url) {
                for c in cookies {
                    ingest_one(&mut map, c);
                }
            }
        }
    }

    eprintln!(
        "[vk_auth] collected cookie names: {:?}",
        map.keys().collect::<Vec<_>>()
    );

    Ok(map.into_iter().collect())
}

#[derive(Debug, Clone, Deserialize)]
struct JsLoginProbe {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    href: Option<String>,
    #[serde(default)]
    logged_in_ui: bool,
}

/// Continuous scanner injected into every page in the login webview.
const VK_INIT_SCRIPT: &str = r#"
(function () {
  if (window.__muzeekaVkScanInstalled) return;
  window.__muzeekaVkScanInstalled = true;
  window.__muzeekaVk = { id: 0, name: null, href: location.href, logged_in_ui: false };

  function pickId(v) {
    var n = Number(v);
    return (n && n > 0 && isFinite(n)) ? n : 0;
  }

  function scan() {
    try {
      var id = 0;
      var name = null;
      var href = String(location.href || '');

      if (window.vk) {
        id = pickId(window.vk.id) || pickId(window.vk.viewer_id) || pickId(window.vk.user_id);
        if (id && (window.vk.first_name || window.vk.last_name)) {
          name = [window.vk.first_name || '', window.vk.last_name || ''].join(' ').trim() || null;
        }
      }
      if (!id && window.cur) {
        id = pickId(window.cur.id) || pickId(window.cur.viewer_id) || pickId(window.cur.oid);
      }

      var html = '';
      try { html = document.documentElement ? document.documentElement.innerHTML : ''; } catch (e) {}

      if (!id && html) {
        var patterns = [
          /vk\.id\s*=\s*(\d{3,})/i,
          /"viewer_id"\s*:\s*(\d{3,})/,
          /"user_id"\s*:\s*(\d{3,})/,
          /"owner_id"\s*:\s*(\d{5,})/,
          /"id"\s*:\s*(\d{5,})\s*,\s*"first_name"/,
          /"id"\s*:\s*(\d{5,})\s*,\s*"domain"/,
          /href="\/id(\d{3,})"/
        ];
        for (var i = 0; i < patterns.length; i++) {
          var m = html.match(patterns[i]);
          if (m) { id = pickId(m[1]); if (id) break; }
        }
      }

      var loggedUi = !!(
        document.querySelector('#top_profile_link') ||
        document.querySelector('[data-testid="profilebutton"]') ||
        document.querySelector('.TopNavBtn__profileImg') ||
        document.querySelector('#l_pr') ||
        document.querySelector('a[href^="/id"]') ||
        document.body && document.body.className && document.body.className.indexOf('is_auth') >= 0
      );

      // Feed/im/audio without login form almost always means authenticated session
      var pathOk = /\/(feed|audio|im|music|settings|id\d+)/i.test(href)
        && href.indexOf('login') < 0
        && href.indexOf('id.vk') < 0
        && href.indexOf('oauth') < 0;

      window.__muzeekaVk = {
        id: id || 0,
        name: name,
        href: href,
        logged_in_ui: !!(loggedUi || (pathOk && id > 0) || (pathOk && loggedUi))
      };
    } catch (e) {
      window.__muzeekaVk = window.__muzeekaVk || { id: 0, name: null, href: '', logged_in_ui: false };
    }
  }

  scan();
  setInterval(scan, 700);
  document.addEventListener('DOMContentLoaded', scan);
  window.addEventListener('load', scan);
})();
"#;

const JS_LOGIN_PROBE: &str = r#"
(function () {
  try {
    if (window.__muzeekaVk) return JSON.stringify(window.__muzeekaVk);
    // Fallback one-shot if init script missed this document
    var id = 0;
    if (window.vk) id = Number(window.vk.id) || 0;
    return JSON.stringify({
      id: id,
      name: null,
      href: String(location.href || ''),
      logged_in_ui: false
    });
  } catch (e) {
    return JSON.stringify({ id: 0, name: null, href: '', logged_in_ui: false });
  }
})()
"#;

async fn eval_js_string(window: &tauri::WebviewWindow, js: &str) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = std::sync::Mutex::new(Some(tx));

    if window
        .eval_with_callback(js.to_string(), move |result| {
            if let Ok(mut guard) = tx.lock() {
                if let Some(sender) = guard.take() {
                    let _ = sender.send(result);
                }
            }
        })
        .is_err()
    {
        return None;
    }

    tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .ok()?
        .ok()
}

fn decode_eval_json(raw: &str) -> Option<Value> {
    let as_value: Value = serde_json::from_str(raw).ok()?;
    match as_value {
        Value::String(s) => serde_json::from_str(&s).ok().or(Some(Value::String(s))),
        other => Some(other),
    }
}

async fn probe_webview_login(window: &tauri::WebviewWindow) -> Option<JsLoginProbe> {
    // Ensure scanner is present even if init script didn't run yet
    let _ = window.eval(VK_INIT_SCRIPT);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let raw = eval_js_string(window, JS_LOGIN_PROBE).await?;
    let value = decode_eval_json(&raw)?;
    let probe: JsLoginProbe = serde_json::from_value(value).ok()?;
    Some(probe)
}

fn url_looks_logged_in(href: &str) -> bool {
    let h = href.to_ascii_lowercase();
    if h.contains("login") || h.contains("oauth") || h.contains("id.vk.com") || h.contains("id.vk.ru")
    {
        return false;
    }
    h.contains("/feed")
        || h.contains("/audio")
        || h.contains("/im")
        || h.contains("/music")
        || h.contains("/settings")
        || h.contains("vk.com/id")
        || h.contains("vk.ru/id")
}

fn finalize_login(
    app: &AppHandle,
    pairs: Vec<(String, String)>,
    user_id: i64,
    user_name: Option<String>,
) -> Result<VkAuthStatus, String> {
    if !has_session_cookie_pairs(&pairs) {
        return Err("session cookie missing".to_string());
    }

    // user_id is preferred; if missing we still save session (decode can recover later).
    let uid = if user_id > 0 { user_id } else { 0 };
    let path = cookie_file_path(app).ok_or_else(|| "No app data dir".to_string())?;
    write_netscape_cookies(&path, &pairs)?;

    if uid > 0 {
        let _ = save_session_meta(
            app,
            &VkSessionMeta {
                user_id: uid,
                user_name: user_name.clone(),
            },
        );
    }

    let status = VkAuthStatus {
        logged_in: true,
        user_id: if uid > 0 { Some(uid) } else { None },
        user_name,
    };
    let _ = app.emit("vk:auth-changed", &status);
    Ok(status)
}

async fn open_login_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(existing) = app.get_webview_window(VK_LOGIN_WINDOW_LABEL) {
        let _ = existing.set_focus();
        return Ok(existing);
    }

    let url = "https://vk.com/login"
        .parse()
        .map_err(|e| format!("Invalid login URL: {e}"))?;

    let window = WebviewWindowBuilder::new(app, VK_LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title("VK Login — Muzeeka")
        .inner_size(520.0, 760.0)
        .resizable(true)
        .center()
        .initialization_script(VK_INIT_SCRIPT)
        .build()
        .map_err(|e| format!("Failed to open VK login window: {e}"))?;

    Ok(window)
}

/// Open VK login webview and wait until a real session is verified (or window closes).
pub async fn login(app: AppHandle) -> Result<VkAuthStatus, String> {
    let current = auth_status(&app);
    if current.logged_in && current.user_id.is_some_and(|id| id > 0) {
        return Ok(current);
    }

    let _window = open_login_window(&app).await?;
    let mut last_err = String::from("Waiting for login…");

    // Poll webview: JS login signal + remixsid cookie. Never verify via HTTP.
    for tick in 0..360 {
        tokio::time::sleep(Duration::from_millis(700)).await;

        let Some(window) = app.get_webview_window(VK_LOGIN_WINDOW_LABEL) else {
            return Err("VK login window was closed".to_string());
        };

        // Re-inject scanner periodically (SPA navigations)
        if tick % 3 == 0 {
            let _ = window.eval(VK_INIT_SCRIPT);
        }

        let probe = probe_webview_login(&window).await.unwrap_or(JsLoginProbe {
            id: 0,
            name: None,
            href: None,
            logged_in_ui: false,
        });

        let href = probe.href.clone().unwrap_or_default();
        let looks_in = probe.id > 0
            || probe.logged_in_ui
            || (!href.is_empty() && url_looks_logged_in(&href));

        if !looks_in {
            continue;
        }

        // After login UI, jump to feed so window.vk.id is populated.
        let mut probe = probe;
        if probe.id <= 0 {
            let _ = window.eval(
                "try{if(!/\\/(feed|audio|im|music)/i.test(location.pathname)){location.replace('https://vk.com/feed');}}catch(e){}",
            );
            tokio::time::sleep(Duration::from_millis(1200)).await;
            if let Some(p2) = probe_webview_login(&window).await {
                probe = p2;
            }
        }

        // Also pull cookies for the exact current URL
        if let Some(h) = probe.href.as_deref() {
            if let Ok(url) = h.parse() {
                let _ = window.cookies_for_url(url);
            }
        }

        let pairs = match collect_vk_cookies_from_window(&window) {
            Ok(p) => p,
            Err(e) => {
                last_err = e;
                continue;
            }
        };

        if !has_session_cookie_pairs(&pairs) {
            last_err = format!(
                "Logged in UI detected (id={}, href={}), waiting for remixsid cookie…",
                probe.id, href
            );
            eprintln!("[vk_auth] {last_err}");
            continue;
        }

        // If still no id, try once with saved cookies via soft HTTP (may fail antibot).
        let mut user_id = probe.id;
        let user_name = probe.name.clone();
        if user_id <= 0 {
            let header = pairs_to_header(&pairs);
            if let Ok(id) = resolve_user_id(&header) {
                user_id = id;
            }
        }

        match finalize_login(&app, pairs, user_id, user_name) {
            Ok(status) => {
                let _ = window.close();
                return Ok(status);
            }
            Err(e) => {
                last_err = e;
                eprintln!("[vk_auth] finalize: {last_err}");
            }
        }
    }

    let _ = app
        .get_webview_window(VK_LOGIN_WINDOW_LABEL)
        .map(|w| w.close());
    Err(format!(
        "VK login timed out. Last status: {last_err}. Stay on the feed a few seconds after sign-in."
    ))
}

/// Clear saved VK session (logout).
pub async fn logout(app: AppHandle) -> Result<VkAuthStatus, String> {
    if let Some(path) = cookie_file_path(&app) {
        let _ = fs::remove_file(path);
    }
    clear_session_meta(&app);

    // Clear webview cookie store so next login is clean.
    if let Some(window) = app.get_webview_window(VK_LOGIN_WINDOW_LABEL) {
        let _ = window.clear_all_browsing_data();
        let _ = window.close();
    } else {
        // Open briefly to clear persisted WebView2 cookies, then close.
        let url = "https://m.vk.ru/"
            .parse()
            .map_err(|e| format!("Invalid URL: {e}"))?;
        if let Ok(window) =
            WebviewWindowBuilder::new(&app, VK_LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
                .title("VK Logout")
                .inner_size(1.0, 1.0)
                .visible(false)
                .build()
        {
            let w = window.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || w.clear_all_browsing_data()).await;
            let _ = window.close();
        }
    }

    let status = VkAuthStatus {
        logged_in: false,
        user_id: None,
        user_name: None,
    };
    let _ = app.emit("vk:auth-changed", &status);
    Ok(status)
}

fn parse_netscape_cookies(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 7 {
            continue;
        }
        let domain = parts[0].to_ascii_lowercase();
        if !(domain.contains("vk.com") || domain.contains("vk.ru")) {
            continue;
        }
        let name = parts[5].trim();
        let value = parts[6].trim();
        if !name.is_empty() && !value.is_empty() {
            out.push((name.to_string(), value.to_string()));
        }
    }
    out
}

fn load_cookies_from_file(path: &Path) -> Vec<(String, String)> {
    fs::read_to_string(path)
        .map(|raw| parse_netscape_cookies(&raw))
        .unwrap_or_default()
}

fn load_cookies_from_browser() -> Vec<(String, String)> {
    let domains: Vec<String> = vec![
        "vk.com".into(),
        "vk.ru".into(),
        ".vk.com".into(),
        ".vk.ru".into(),
        "m.vk.com".into(),
        "m.vk.ru".into(),
    ];
    let mut map = std::collections::HashMap::<String, String>::new();

    let mut collect = |cookies: Vec<rookie::common::enums::Cookie>| {
        for c in cookies {
            if !c.name.is_empty() && !c.value.is_empty() {
                map.insert(c.name, c.value);
            }
        }
    };

    if let Ok(cookies) = rookie::chrome(Some(domains.clone())) {
        collect(cookies);
    }
    if let Ok(cookies) = rookie::edge(Some(domains.clone())) {
        collect(cookies);
    }
    if let Ok(cookies) = rookie::chromium(Some(domains.clone())) {
        collect(cookies);
    }
    if let Ok(cookies) = rookie::firefox(Some(domains.clone())) {
        collect(cookies);
    }
    if let Ok(cookies) = rookie::brave(Some(domains)) {
        collect(cookies);
    }

    map.into_iter().collect()
}

fn merge_cookies(primary: Vec<(String, String)>, secondary: Vec<(String, String)>) -> String {
    let mut map = std::collections::HashMap::<String, String>::new();
    for (k, v) in secondary.into_iter().chain(primary) {
        map.insert(k, v);
    }
    // Sensible defaults used by vk_api
    map.entry("remixaudio_show_alert_today".into())
        .or_insert_with(|| "0".into());
    map.entry("remixmdevice".into())
        .or_insert_with(|| "1920/1080/2/!!-!!!!".into());

    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn load_cookie_header(app: &AppHandle) -> Result<String, String> {
    let mut from_file = Vec::new();
    if let Some(path) = cookie_file_path(app) {
        if path.is_file() {
            from_file = load_cookies_from_file(&path);
        }
    }

    let from_browser = load_cookies_from_browser();
    let header = merge_cookies(from_file, from_browser);

    // Only remixsid / remixnsid count as a real session (not remixstid tracking cookies).
    let has_session = header.split(';').any(|part| {
        let part = part.trim();
        if let Some((name, value)) = part.split_once('=') {
            is_auth_cookie_name(name.trim()) && is_plausible_session_value(value)
        } else {
            false
        }
    });

    if !has_session {
        return Err(
            "VK login required. Open Settings → General → VK Music and log in."
                .to_string(),
        );
    }

    Ok(header)
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

fn http_get(url: &str, cookie: &str, mobile: bool) -> Result<String, String> {
    let ua = if mobile { USER_AGENT } else { DESKTOP_UA };
    let mut response = http()
        .get(url)
        .header("User-Agent", ua)
        .header("Cookie", cookie)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7")
        .header("Cache-Control", "no-cache")
        .header(
            "Sec-Ch-Ua",
            r#""Chromium";v="120", "Not_A Brand";v="24", "Google Chrome";v="120""#,
        )
        .header("Sec-Ch-Ua-Mobile", if mobile { "?1" } else { "?0" })
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Upgrade-Insecure-Requests", "1")
        .call()
        .map_err(|e| format!("VK request failed: {e}"))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read VK response: {e}"))
}

fn http_post_form(url: &str, cookie: &str, body: &str, mobile: bool) -> Result<String, String> {
    let ua = if mobile { USER_AGENT } else { DESKTOP_UA };
    let mut response = http()
        .post(url)
        .header("User-Agent", ua)
        .header("Cookie", cookie)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.8")
        .header("Origin", if mobile { "https://m.vk.ru" } else { "https://vk.ru" })
        .header("Referer", if mobile { "https://m.vk.ru/audio" } else { "https://vk.ru/audio" })
        .send(body.as_bytes())
        .map_err(|e| format!("VK POST failed: {e}"))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read VK response: {e}"))
}

/// Prefer user id saved during WebView login; fall back to HTTP scrape.
/// Returns 0 only if completely unknown (decode may still work for some tracks).
fn resolve_user_id_for_app(app: &AppHandle, cookie: &str) -> Result<i64, String> {
    if let Some(meta) = load_session_meta(app) {
        if meta.user_id > 0 {
            return Ok(meta.user_id);
        }
    }
    match resolve_user_id(cookie) {
        Ok(id) if id > 0 => {
            let _ = save_session_meta(
                app,
                &VkSessionMeta {
                    user_id: id,
                    user_name: None,
                },
            );
            Ok(id)
        }
        Ok(id) => Ok(id),
        // Don't hard-fail downloads — user_id=0 still works when URL isn't encrypted with `i` op.
        Err(err) => {
            eprintln!("[vk_auth] resolve_user_id soft-fail: {err}");
            Ok(0)
        }
    }
}

fn resolve_user_id(cookie: &str) -> Result<i64, String> {
    // Pages that only work meaningfully when logged in.
    for (url, mobile) in [
        ("https://m.vk.ru/audio", true),
        ("https://vk.ru/feed", false),
        ("https://m.vk.ru/settings", true),
        ("https://vk.com/feed", false),
        ("https://vk.com/", false),
    ] {
        let html = match http_get(url, cookie, mobile) {
            Ok(h) => h,
            Err(_) => continue,
        };

        if looks_like_login_page(&html) {
            continue;
        }

        if let Some(id) = extract_user_id_from_html(&html) {
            return Ok(id);
        }

        // Broader patterns for modern SPA bootstrap
        for re in [
            r#""id"\s*:\s*(\d{3,})\s*,\s*"first_name""#,
            r#""id"\s*:\s*(\d{3,})\s*,\s*"domain""#,
            r#"viewer_id["']?\s*[:=]\s*(\d{3,})"#,
        ] {
            if let Ok(rx) = Regex::new(re) {
                if let Some(c) = rx.captures(&html) {
                    if let Ok(id) = c[1].parse::<i64>() {
                        if id > 0 {
                            return Ok(id);
                        }
                    }
                }
            }
        }
    }

    Err(
        "VK session cookies are present, but user id is unknown. Log out and log in again via Settings → VK Music."
            .to_string(),
    )
}

// ── URL decode (audio_api_unavailable) ───────────────────────────────────────

fn vk_o(string: &str) -> String {
    let mut result = String::new();
    let mut index2: i32 = 0;
    let mut i: i32 = 0;

    for s in string.chars() {
        let sym_index = match VK_STR.find(s) {
            Some(idx) => idx as i32,
            None => continue,
        };

        if index2 % 4 != 0 {
            index2 += 1;
            i = (i << 6) + sym_index;
            let shift = (-2 * index2) & 6;
            result.push(char::from_u32((0xFF & (i >> shift)) as u32).unwrap_or('\0'));
        } else {
            i = sym_index;
            index2 += 1;
        }
    }

    result
}

fn vk_r(string: &str, i: i32) -> String {
    let vk_str2 = format!("{VK_STR}{VK_STR}");
    let vk_str2_len = vk_str2.len() as i32;
    let mut result = String::new();

    for s in string.chars() {
        if let Some(index) = vk_str2.find(s) {
            let mut offset = index as i32 - i;
            if offset < 0 {
                offset += vk_str2_len;
            }
            result.push(vk_str2.chars().nth(offset as usize).unwrap_or(s));
        } else {
            result.push(s);
        }
    }
    result
}

fn vk_xor(string: &str, i: &str) -> String {
    let xor_val = i.chars().next().map(|c| c as u32).unwrap_or(0);
    string
        .chars()
        .map(|s| char::from_u32((s as u32) ^ xor_val).unwrap_or(s))
        .collect()
}

fn vk_s_child(t_len: usize, e: i32) -> Vec<usize> {
    if t_len == 0 {
        return Vec::new();
    }
    let mut o = Vec::with_capacity(t_len);
    let mut e = e;
    for a in (0..t_len).rev() {
        e = ((t_len as i32) * (a as i32 + 1) ^ e + a as i32).rem_euclid(t_len as i32);
        o.push(e as usize);
    }
    o.reverse();
    o
}

fn vk_s(t: &str, e: i32) -> String {
    let i = t.len();
    if i == 0 {
        return t.to_string();
    }
    let o = vk_s_child(i, e);
    let mut t: Vec<char> = t.chars().collect();
    for a in 1..i {
        let idx = o[i - 1 - a];
        let y = t[idx];
        t[idx] = t[a];
        t[a] = y;
    }
    t.into_iter().collect()
}

fn decode_audio_url(string: &str, user_id: i64) -> Result<String, String> {
    if !string.contains("audio_api_unavailable") {
        return Ok(string.to_string());
    }

    let extra = string
        .split_once("?extra=")
        .map(|(_, rest)| rest)
        .ok_or_else(|| "Invalid encrypted VK audio URL".to_string())?;

    let (vals0, vals1) = match extra.split_once('#') {
        Some((a, b)) => (a, b),
        None => (extra, ""),
    };

    let mut tstr = vk_o(vals0);
    if tstr.is_empty() {
        return Ok(string.to_string());
    }

    let ops = if vals1.is_empty() {
        String::new()
    } else {
        vk_o(vals1)
    };

    let mut ops_list: Vec<&str> = if ops.is_empty() {
        Vec::new()
    } else {
        ops.split('\u{0009}').collect()
    };
    ops_list.reverse();

    for op_data in ops_list {
        let mut parts = op_data.split('\u{000b}');
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next();
        match cmd {
            "v" => tstr = tstr.chars().rev().collect(),
            "r" => {
                let i: i32 = arg
                    .unwrap_or("0")
                    .parse()
                    .map_err(|_| "VK URL decode r arg".to_string())?;
                tstr = vk_r(&tstr, i);
            }
            "x" => {
                tstr = vk_xor(&tstr, arg.unwrap_or(""));
            }
            "s" => {
                let i: i32 = arg
                    .unwrap_or("0")
                    .parse()
                    .map_err(|_| "VK URL decode s arg".to_string())?;
                tstr = vk_s(&tstr, i);
            }
            "i" => {
                let i: i32 = arg
                    .unwrap_or("0")
                    .parse()
                    .map_err(|_| "VK URL decode i arg".to_string())?;
                tstr = vk_s(&tstr, i ^ (user_id as i32));
            }
            _ => return Err(format!("Unknown VK audio URL decode op: {cmd}")),
        }
    }

    if tstr.starts_with("http") {
        Ok(tstr)
    } else {
        Ok(string.to_string())
    }
}

fn convert_m3u8_to_mp3(url: &str) -> String {
    re_m3u8_to_mp3()
        .replace(url, "$1/$2.mp3")
        .into_owned()
}

/// Prefer original-quality direct MP3 over HLS re-encode.
/// VK often serves the full-bitrate file at a rewritten `.mp3` URL.
fn stream_url_candidates(url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let push = |list: &mut Vec<String>, u: String| {
        if !u.is_empty() && !list.iter().any(|x| x == &u) {
            list.push(u);
        }
    };

    if url.contains("m3u8") {
        // Classic vk_api rewrite: .../hex(/audios)?/hex/index.m3u8 → .../hex(/audios)?/hex.mp3
        push(&mut out, convert_m3u8_to_mp3(url));
        // Simpler rewrites some CDNs accept
        push(&mut out, url.replace("/index.m3u8", ".mp3"));
        push(&mut out, url.replace("index.m3u8", "index.mp3"));
        // Strip query string then rewrite
        if let Some((base, _)) = url.split_once('?') {
            if base.contains("m3u8") {
                push(&mut out, convert_m3u8_to_mp3(base));
                push(&mut out, base.replace("/index.m3u8", ".mp3"));
            }
        }
        // HLS last — only if direct mp3 fails (forces re-encode)
        push(&mut out, url.to_string());
    } else {
        push(&mut out, url.to_string());
        // If API already gave mp3, still nothing else to try
    }

    out
}

// ── Track parsing ────────────────────────────────────────────────────────────

fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // basic entities
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

fn parse_audio_array(arr: &[Value], user_id: i64) -> Option<VkTrack> {
    if arr.len() < 6 {
        return None;
    }

    let id = arr[0].as_u64().or_else(|| arr[0].as_i64().map(|v| v as u64))?;
    let owner_id = arr[1].as_i64()?;
    let mut url = arr[2].as_str().unwrap_or("").to_string();
    let title = strip_html(arr[3].as_str().unwrap_or("Unknown"));
    let artist = strip_html(arr[4].as_str().unwrap_or("Unknown"));
    let duration = arr[5].as_u64().unwrap_or(0) as u32;

    // Field 14 is often "small,large" — prefer larger first for embedding.
    let mut covers = arr
        .get(14)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("http"))
        .collect::<Vec<_>>();
    covers.reverse();

    if url.contains("audio_api_unavailable") {
        if let Ok(decoded) = decode_audio_url(&url, user_id) {
            url = decoded;
        }
    }
    if url.contains("m3u8") {
        url = convert_m3u8_to_mp3(&url);
    }

    Some(VkTrack {
        owner_id,
        id,
        title,
        artist,
        duration,
        url,
        covers,
    })
}

fn parse_audio_object(obj: &serde_json::Map<String, Value>, user_id: i64) -> Option<VkTrack> {
    let id = obj
        .get("id")
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|x| x as u64)))?;
    let owner_id = obj
        .get("owner_id")
        .or_else(|| obj.get("ownerId"))
        .and_then(|v| v.as_i64())?;
    let mut url = obj
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = strip_html(obj.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown"));
    let artist = strip_html(
        obj.get("artist")
            .or_else(|| obj.get("performer"))
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown"),
    );
    let duration = obj
        .get("duration")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let covers = collect_cover_urls_from_object(obj);

    if url.contains("audio_api_unavailable") {
        if let Ok(decoded) = decode_audio_url(&url, user_id) {
            url = decoded;
        }
    }
    if url.contains("m3u8") {
        url = convert_m3u8_to_mp3(&url);
    }

    Some(VkTrack {
        owner_id,
        id,
        title,
        artist,
        duration,
        url,
        covers,
    })
}

/// Collect cover URLs from VK audio object (API / al_audio).
/// Prefers larger album.thumb photo_* sizes when present.
fn collect_cover_urls_from_object(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut covers = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if s.starts_with("http") && !covers.iter().any(|c| c == s) {
            covers.push(s.to_string());
        }
    };

    for key in [
        "coverUrl_l",
        "coverUrl_p",
        "coverUrl_s",
        "cover_url",
        "thumb",
        "photo",
    ] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            push(s);
        }
    }

    // album may be object with thumb.photo_600 etc., or a nested structure
    if let Some(album) = obj.get("album") {
        if let Some(map) = album.as_object() {
            if let Some(thumb) = map.get("thumb").or_else(|| map.get("photo")) {
                if let Some(s) = thumb.as_str() {
                    push(s);
                } else if let Some(tm) = thumb.as_object() {
                    // Prefer larger sizes first
                    let mut sized: Vec<(u32, String)> = Vec::new();
                    for (k, v) in tm {
                        if let Some(s) = v.as_str() {
                            if !s.starts_with("http") {
                                continue;
                            }
                            let size = k
                                .rsplit('_')
                                .next()
                                .and_then(|p| p.parse::<u32>().ok())
                                .unwrap_or(0);
                            sized.push((size, s.to_string()));
                        }
                    }
                    sized.sort_by(|a, b| b.0.cmp(&a.0));
                    for (_, s) in sized {
                        push(&s);
                    }
                    for key in ["photo_1200", "photo_600", "photo_300", "photo_270", "src"] {
                        if let Some(s) = tm.get(key).and_then(|v| v.as_str()) {
                            push(s);
                        }
                    }
                }
            }
            for key in ["cover", "thumb", "photo"] {
                if let Some(s) = map.get(key).and_then(|v| v.as_str()) {
                    push(s);
                }
            }
        }
    }

    // Some payloads put covers as comma-separated string
    if let Some(s) = obj.get("covers").and_then(|v| v.as_str()) {
        for part in s.split(',') {
            push(part);
        }
    }

    covers
}

fn parse_track_value(value: &Value, user_id: i64) -> Option<VkTrack> {
    if let Some(arr) = value.as_array() {
        parse_audio_array(arr, user_id)
    } else if let Some(obj) = value.as_object() {
        parse_audio_object(obj, user_id)
    } else {
        None
    }
}

fn scrap_ids_from_html(html: &str) -> Vec<(i64, u64, String, String)> {
    let mut ids = Vec::new();
    let mut search = html;

    while let Some(pos) = search.find("data-audio") {
        let slice = &search[pos..];
        let Some(eq) = slice.find('=') else {
            search = &search[pos + 10..];
            continue;
        };
        let after = slice[eq + 1..].trim_start();
        let quote = after.chars().next();
        if quote != Some('\'') && quote != Some('"') {
            search = &search[pos + 10..];
            continue;
        }
        let q = quote.unwrap();
        let rest = &after[1..];
        let Some(end) = rest.find(q) else {
            search = &search[pos + 10..];
            continue;
        };
        let raw = &rest[..end];
        let decoded = raw
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&");

        if let Ok(value) = serde_json::from_str::<Value>(&decoded) {
            if let Some(arr) = value.as_array() {
                if arr.len() > 1 {
                    if let (Some(id), Some(owner)) = (
                        arr[0].as_u64().or_else(|| arr[0].as_i64().map(|v| v as u64)),
                        arr[1].as_i64(),
                    ) {
                        let hashes = arr
                            .get(13)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .split('/')
                            .collect::<Vec<_>>();
                        let action = hashes.get(2).unwrap_or(&"").to_string();
                        let url_hash = hashes.get(5).unwrap_or(&"").to_string();
                        // Prefer full hashes; otherwise still keep id for access_key path.
                        ids.push((owner, id, action, url_hash));
                    }
                }
            } else if let Some(obj) = value.as_object() {
                if let (Some(id), Some(owner)) = (
                    obj.get("id")
                        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|x| x as u64))),
                    obj.get("owner_id")
                        .or_else(|| obj.get("ownerId"))
                        .and_then(|v| v.as_i64()),
                ) {
                    let action = obj
                        .get("actionHash")
                        .or_else(|| obj.get("action_hash"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let url_hash = obj
                        .get("urlHash")
                        .or_else(|| obj.get("url_hash"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let access = obj
                        .get("access_key")
                        .or_else(|| obj.get("accessKey"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if action.is_empty() && !access.is_empty() {
                        ids.push((owner, id, access.to_string(), String::new()));
                    } else {
                        ids.push((owner, id, action, url_hash));
                    }
                }
            }
        }

        search = &rest[end + 1..];
    }

    // Also scrape raw audio arrays embedded in page JS
    if ids.is_empty() {
        if let Some(re) = Regex::new(r#"\[(-?\d+),(-?\d+),(\d+),[^]]*?"([^"]*?/[^"]*?)"#).ok() {
            for caps in re.captures_iter(html) {
                // too loose — skip
                let _ = caps;
            }
        }
    }

    ids
}

fn strip_vk_ajax_wrapper(raw: &str) -> &str {
    let t = raw.trim();
    t.strip_prefix("<!--")
        .and_then(|s| s.strip_suffix("-->"))
        .map(str::trim)
        .unwrap_or(t)
}

fn tracks_from_reload_json(raw: &str, user_id: i64) -> Vec<VkTrack> {
    let cleaned = strip_vk_ajax_wrapper(raw);
    let mut tracks = Vec::new();

    // Mobile: {"data":[[audio,...]]}
    if let Ok(parsed) = serde_json::from_str::<ReloadResponse>(cleaned) {
        if let Some(data) = parsed.data {
            if let Some(first) = data.first() {
                if let Some(list) = first.as_array() {
                    for item in list {
                        if let Some(track) = parse_track_value(item, user_id) {
                            tracks.push(track);
                        }
                    }
                } else if let Some(track) = parse_track_value(first, user_id) {
                    tracks.push(track);
                }
            }
        }
    }

    // Desktop al_audio.php: {"payload":[0,[[audio arrays], ...]]} or similar
    if tracks.is_empty() {
        if let Ok(value) = serde_json::from_str::<Value>(cleaned) {
            // payload[1][0] is often the list
            if let Some(list) = value
                .pointer("/payload/1/0")
                .and_then(|v| v.as_array())
                .or_else(|| value.pointer("/payload/1").and_then(|v| v.as_array()))
            {
                for item in list {
                    if let Some(track) = parse_track_value(item, user_id) {
                        tracks.push(track);
                    } else if let Some(inner) = item.as_array() {
                        // sometimes nested
                        for sub in inner {
                            if let Some(track) = parse_track_value(sub, user_id) {
                                tracks.push(track);
                            }
                        }
                    }
                }
            }
            // direct list
            if tracks.is_empty() {
                if let Some(list) = value.as_array() {
                    for item in list {
                        if let Some(track) = parse_track_value(item, user_id) {
                            tracks.push(track);
                        }
                    }
                }
            }
        }
    }

    tracks
}

fn reload_audio_ids(
    cookie: &str,
    user_id: i64,
    id_tokens: &[String],
) -> Result<Vec<VkTrack>, String> {
    if id_tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut tracks = Vec::new();
    for chunk in id_tokens.chunks(10) {
        check_cancel()?;
        let joined = chunk.join(",");
        let mut chunk_tracks = Vec::new();

        // 1) mobile
        let body = format!("act=reload_audio&ids={}", urlencoding::encode(&joined));
        if let Ok(raw) = http_post_form("https://m.vk.ru/audio", cookie, &body, true) {
            chunk_tracks.extend(tracks_from_reload_json(&raw, user_id));
        }

        // 2) desktop al_audio.php
        if chunk_tracks.is_empty() {
            let body = format!(
                "act=reload_audio&al=1&ids={}",
                urlencoding::encode(&joined)
            );
            if let Ok(raw) = http_post_form("https://vk.ru/al_audio.php", cookie, &body, false) {
                chunk_tracks.extend(tracks_from_reload_json(&raw, user_id));
            }
            if chunk_tracks.is_empty() {
                if let Ok(raw) =
                    http_post_form("https://vk.com/al_audio.php", cookie, &body, false)
                {
                    chunk_tracks.extend(tracks_from_reload_json(&raw, user_id));
                }
            }
        }

        tracks.extend(
            chunk_tracks
                .into_iter()
                .filter(|t| has_playable_stream(t)),
        );
        std::thread::sleep(Duration::from_millis(250));
    }

    Ok(tracks)
}

/// VK antibot often returns short "listen in the official app" placeholder audio
/// when the request is not from a real browser session.
fn is_restriction_stub(track: &VkTrack) -> bool {
    let title = track.title.to_lowercase();
    let artist = track.artist.to_lowercase();
    let hay = format!("{title} {artist}");

    const PATTERNS: &[&str] = &[
        "недоступн",
        "официальн",
        "приложении",
        "в приложении",
        "слушайте в",
        "не в том",
        "only available",
        "official app",
        "official vk",
        "audio is unavailable",
        "track is unavailable",
        "content is not available",
        "listen in the",
        "another app",
        "other app",
        "unavailable in your",
        "not available in this",
        "open the official",
    ];
    if PATTERNS.iter().any(|p| hay.contains(p)) {
        return true;
    }

    // Typical stub stream: very short clip with empty/placeholder meta
    if track.duration > 0
        && track.duration <= 20
        && (title.contains("music")
            || title.contains("audio")
            || title.contains("трек")
            || title.contains("аудио")
            || artist == "vk"
            || artist == "vk music"
            || artist.is_empty()
            || artist == "unknown")
        && (title.contains("доступ")
            || title.contains("available")
            || title.contains("app")
            || title.contains("прилож"))
    {
        return true;
    }

    false
}

fn has_playable_stream(track: &VkTrack) -> bool {
    track.url.starts_with("http") && !is_restriction_stub(track)
}

fn fetch_single_track(
    cookie: &str,
    user_id: i64,
    owner_id: i64,
    audio_id: u64,
    access_key: Option<&str>,
) -> Result<VkTrack, String> {
    let ak = access_key.unwrap_or("").trim();

    // Build candidate id tokens (order matters — best first).
    let mut tokens: Vec<String> = Vec::new();
    if !ak.is_empty() {
        // VK Music Saver / audio.getById style
        tokens.push(format!("{owner_id}_{audio_id}_{ak}"));
    }

    // Try scrape hashes from track page
    let mut page_urls = vec![
        format!("https://m.vk.ru/audio{owner_id}_{audio_id}"),
        format!("https://vk.ru/audio{owner_id}_{audio_id}"),
        format!("https://vk.com/audio{owner_id}_{audio_id}"),
    ];
    if !ak.is_empty() {
        page_urls.insert(0, format!("https://m.vk.ru/audio{owner_id}_{audio_id}_{ak}"));
        page_urls.insert(1, format!("https://vk.ru/audio{owner_id}_{audio_id}_{ak}"));
        page_urls.insert(2, format!("https://vk.com/audio{owner_id}_{audio_id}_{ak}"));
    }

    let mut scraped: Vec<(i64, u64, String, String)> = Vec::new();
    for page_url in &page_urls {
        if let Ok(html) = http_get(page_url, cookie, page_url.contains("m.vk")) {
            // Direct URL embedded in HTML (rare but free win)
            if let Some(re) = Regex::new(r#"https://[a-zA-Z0-9._/-]+\.(?:mp3|m3u8)[^"'\s]*"#).ok()
            {
                if let Some(m) = re.find(&html) {
                    let mut url = m.as_str().to_string();
                    if url.contains("audio_api_unavailable") {
                        if let Ok(decoded) = decode_audio_url(&url, user_id) {
                            url = decoded;
                        }
                    }
                    if url.contains("m3u8") {
                        url = convert_m3u8_to_mp3(&url);
                    }
                    if url.starts_with("http") {
                        return Ok(VkTrack {
                            owner_id,
                            id: audio_id,
                            title: format!("Track {audio_id}"),
                            artist: "Unknown".into(),
                            duration: 0,
                            url,
                            covers: Vec::new(),
                        });
                    }
                }
            }

            let mut ids = scrap_ids_from_html(&html);
            ids.retain(|(o, a, _, _)| *o == owner_id && *a == audio_id);
            if !ids.is_empty() {
                scraped = ids;
                break;
            }
        }
    }

    for (owner, id, action, url_hash) in &scraped {
        if !action.is_empty() && !url_hash.is_empty() {
            tokens.push(format!("{owner}_{id}_{action}_{url_hash}"));
        } else if !action.is_empty() {
            tokens.push(format!("{owner}_{id}_{action}"));
        }
    }
    tokens.push(format!("{owner_id}_{audio_id}"));

    // Dedup while preserving order
    let mut seen = std::collections::HashSet::new();
    tokens.retain(|t| seen.insert(t.clone()));

    let tracks = reload_audio_ids(cookie, user_id, &tokens)?;
    if let Some(track) = tracks
        .into_iter()
        .find(|t| t.owner_id == owner_id && t.id == audio_id && t.url.starts_with("http"))
    {
        return Ok(track);
    }

    // Last attempt: any track with a real stream URL from the best tokens
    let tracks = reload_audio_ids(
        cookie,
        user_id,
        &tokens.iter().take(3).cloned().collect::<Vec<_>>(),
    )?;
    tracks
        .into_iter()
        .find(|t| t.url.starts_with("http"))
        .ok_or_else(|| {
            format!(
                "Could not resolve VK track stream URL for audio{owner_id}_{audio_id}. Track may be blocked or session incomplete — try Log out / Log in again."
            )
        })
}

/// Playlist entry from load_section — IDs + metadata. Stream URLs usually empty
/// or restriction stubs when fetched over plain HTTP; resolve via WebView later.
#[derive(Debug, Clone)]
struct PlaylistEntry {
    owner_id: i64,
    id: u64,
    /// action_hash / access_key for getById & reload_audio
    access_key: String,
    url_hash: String,
    title: String,
    artist: String,
    duration: u32,
    covers: Vec<String>,
}

fn playlist_entry_from_value(item: &Value, user_id: i64) -> Option<PlaylistEntry> {
    // Prefer array form used by load_section
    if let Some(arr) = item.as_array() {
        if arr.len() < 2 {
            return None;
        }
        let id = arr[0].as_u64().or_else(|| arr[0].as_i64().map(|v| v as u64))?;
        let owner_id = arr[1].as_i64()?;
        let title = strip_html(arr.get(3).and_then(|v| v.as_str()).unwrap_or("Unknown"));
        let artist = strip_html(arr.get(4).and_then(|v| v.as_str()).unwrap_or("Unknown"));
        let duration = arr.get(5).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let mut covers = arr
            .get(14)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("http"))
            .collect::<Vec<_>>();
        covers.reverse();

        let hashes = arr
            .get(13)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .split('/')
            .collect::<Vec<_>>();
        // actionHash ~ [2], urlHash ~ [5]; access_key sometimes sits alone
        let action = hashes.get(2).copied().unwrap_or("").to_string();
        let url_hash = hashes.get(5).copied().unwrap_or("").to_string();
        let access_key = if !action.is_empty() {
            action
        } else {
            hashes
                .iter()
                .find(|h| !h.is_empty())
                .copied()
                .unwrap_or("")
                .to_string()
        };

        // If list already embeds a real stream, keep hashes from parse too
        let _ = user_id;
        return Some(PlaylistEntry {
            owner_id,
            id,
            access_key,
            url_hash,
            title,
            artist,
            duration,
            covers,
        });
    }

    if let Some(obj) = item.as_object() {
        let id = obj
            .get("id")
            .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|x| x as u64)))?;
        let owner_id = obj
            .get("owner_id")
            .or_else(|| obj.get("ownerId"))
            .and_then(|v| v.as_i64())?;
        let title = strip_html(obj.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown"));
        let artist = strip_html(
            obj.get("artist")
                .or_else(|| obj.get("performer"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown"),
        );
        let duration = obj
            .get("duration")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let covers = collect_cover_urls_from_object(obj);
        let access_key = obj
            .get("access_key")
            .or_else(|| obj.get("accessKey"))
            .or_else(|| obj.get("actionHash"))
            .or_else(|| obj.get("action_hash"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let url_hash = obj
            .get("urlHash")
            .or_else(|| obj.get("url_hash"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return Some(PlaylistEntry {
            owner_id,
            id,
            access_key,
            url_hash,
            title,
            artist,
            duration,
            covers,
        });
    }

    None
}

fn entry_id_token(entry: &PlaylistEntry) -> String {
    if !entry.access_key.is_empty() && !entry.url_hash.is_empty() {
        format!(
            "{}_{}_{}_{}",
            entry.owner_id, entry.id, entry.access_key, entry.url_hash
        )
    } else if !entry.access_key.is_empty() {
        format!("{}_{}_{}", entry.owner_id, entry.id, entry.access_key)
    } else {
        format!("{}_{}", entry.owner_id, entry.id)
    }
}

fn entry_getbyid_tokens(entry: &PlaylistEntry) -> Vec<String> {
    let mut tokens = Vec::new();
    if !entry.access_key.is_empty() {
        tokens.push(format!(
            "{}_{}_{}",
            entry.owner_id, entry.id, entry.access_key
        ));
    }
    tokens.push(format!("{}_{}", entry.owner_id, entry.id));
    tokens
}

/// Load playlist title/cover + track IDs via m.vk load_section (no stream resolve).
fn fetch_playlist_catalog(
    cookie: &str,
    owner_id: i64,
    playlist_id: u64,
    access_hash: Option<&str>,
    user_id: i64,
) -> Result<(String, Option<String>, Vec<PlaylistEntry>), String> {
    let mut entries = Vec::new();
    let mut title = format!("Playlist {owner_id}_{playlist_id}");
    let mut thumb: Option<String> = None;
    let mut offset = 0u32;

    loop {
        check_cancel()?;
        let access = access_hash.unwrap_or("");
        let body = format!(
            "act=load_section&owner_id={owner_id}&playlist_id={playlist_id}&offset={offset}&type=playlist&access_hash={}&is_loading_all=1",
            urlencoding::encode(access)
        );
        let raw = http_post_form("https://m.vk.ru/audio", cookie, &body, true)?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("Failed to parse playlist response: {e}"))?;

        let section = value
            .pointer("/data/0")
            .cloned()
            .ok_or_else(|| "VK playlist not found or access denied".to_string())?;

        if section.is_null() {
            return Err("VK playlist not found or access denied".to_string());
        }

        if let Some(t) = section.get("title").and_then(|v| v.as_str()) {
            if !t.is_empty() {
                title = strip_html(t);
            }
        }
        if thumb.is_none() {
            thumb = extract_playlist_thumb(&section);
        }

        let list = section
            .get("list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if list.is_empty() {
            break;
        }

        for item in &list {
            if let Some(entry) = playlist_entry_from_value(item, user_id) {
                // Skip obvious restriction placeholders from the catalog itself
                let probe = VkTrack {
                    owner_id: entry.owner_id,
                    id: entry.id,
                    title: entry.title.clone(),
                    artist: entry.artist.clone(),
                    duration: entry.duration,
                    url: String::new(),
                    covers: entry.covers.clone(),
                };
                if is_restriction_stub(&probe) {
                    continue;
                }
                entries.push(entry);
            }
        }

        let has_more = section
            .get("hasMore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has_more {
            break;
        }
        offset += list.len() as u32;
        if offset > 5000 {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if entries.is_empty() {
        return Err("VK playlist is empty or unavailable".to_string());
    }

    let thumb = thumb.or_else(|| entries.first().and_then(|e| e.covers.first().cloned()));
    Ok((title, thumb, entries))
}

/// Extract the best playlist/album cover URL from a VK load_section payload.
fn extract_playlist_thumb(section: &Value) -> Option<String> {
    let push_http = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.starts_with("http") {
            Some(s.to_string())
        } else {
            None
        }
    };

    // Direct string fields
    for key in [
        "thumb",
        "coverUrl_l",
        "coverUrl_p",
        "coverUrl_s",
        "photo",
        "cover",
        "img",
    ] {
        if let Some(s) = section.get(key).and_then(|v| v.as_str()) {
            if let Some(u) = push_http(s) {
                return Some(u);
            }
        }
    }

    // Nested photo / thumb objects with size keys (photo_600, etc.)
    for key in ["thumb", "photo", "cover", "image"] {
        let Some(obj) = section.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        // Prefer larger sizes
        let mut sized: Vec<(u32, String)> = Vec::new();
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                if !s.starts_with("http") {
                    continue;
                }
                let size = k
                    .rsplit('_')
                    .next()
                    .and_then(|p| p.parse::<u32>().ok())
                    .unwrap_or(0);
                sized.push((size, s.to_string()));
            }
        }
        sized.sort_by(|a, b| b.0.cmp(&a.0));
        if let Some((_, u)) = sized.first() {
            return Some(u.clone());
        }
        for k in ["photo_1200", "photo_600", "photo_300", "src", "url"] {
            if let Some(s) = obj.get(k).and_then(|v| v.as_str()).and_then(push_http) {
                return Some(s);
            }
        }
    }

    // Sometimes covers live under album
    if let Some(album) = section.get("album") {
        if let Some(u) = extract_playlist_thumb(album) {
            return Some(u);
        }
    }

    None
}

// ── Download helpers ─────────────────────────────────────────────────────────

fn sanitize_filename(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    out = out.trim().trim_matches('.').to_string();
    if out.is_empty() {
        out = "track".to_string();
    }
    if out.len() > 180 {
        out.truncate(180);
    }
    out
}

fn unique_path(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut path = dir.join(format!("{base}.{ext}"));
    if !path.exists() {
        return path;
    }
    for i in 1..1000 {
        path = dir.join(format!("{base} ({i}).{ext}"));
        if !path.exists() {
            return path;
        }
    }
    dir.join(format!("{base}.{ext}"))
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

fn download_bytes(
    url: &str,
    cookie: &str,
    dest: &Path,
    app: &AppHandle,
    page_url: &str,
    progress_lo: f32,
    progress_hi: f32,
) -> Result<(), String> {
    check_cancel()?;
    emit_progress(app, page_url, "Downloading…", Some(progress_lo));

    let mut response = http()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Cookie", cookie)
        .header("Referer", "https://m.vk.ru/")
        .call()
        .map_err(|e| format!("Failed to download audio: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("Audio download HTTP {}", status.as_u16()));
    }

    let len = response
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let mut reader = response.body_mut().as_reader();
    let mut file =
        File::create(dest).map_err(|e| format!("Failed to create file {}: {e}", dest.display()))?;

    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    let mut last_emit = progress_lo - 1.0;
    let span = (progress_hi - progress_lo).max(1.0);
    loop {
        check_cancel()?;
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Download read error: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("Download write error: {e}"))?;
        written += n as u64;
        let pct = if let Some(total) = len.filter(|t| *t > 0) {
            progress_lo + (written as f32 / total as f32) * span
        } else {
            // Unknown size: ease toward hi without jumping to 100
            let approx = progress_lo + span * (1.0 - (-(written as f32) / 2_000_000.0).exp());
            approx.min(progress_hi - 0.5)
        };
        if (pct - last_emit).abs() >= 0.4 {
            last_emit = pct;
            emit_progress(app, page_url, "Downloading…", Some(pct.clamp(0.0, 99.0)));
        }
    }

    emit_progress(app, page_url, "Downloading…", Some(progress_hi.min(99.0)));
    Ok(())
}

fn download_with_ffmpeg(
    app: &AppHandle,
    url: &str,
    dest: &Path,
    page_url: &str,
    progress_lo: f32,
) -> Result<(), String> {
    let ffmpeg_dir = ytdlp::resolve_ffmpeg_location(app)
        .ok_or_else(|| "ffmpeg not found (required for HLS streams)".to_string())?;
    let ffmpeg = ffmpeg_dir.join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    if !ffmpeg.is_file() {
        return Err(format!("ffmpeg not found at {}", ffmpeg.display()));
    }

    // Pulse a few intermediate ticks so UI doesn't sit frozen at one value.
    let start = progress_lo.clamp(5.0, 70.0);
    emit_progress(app, page_url, "Converting to 320 kbps MP3…", Some(start));

    // CBR 320 — not VBR q=0 (which often lands ~220–260 kbps from AAC HLS).
    let status = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            url,
            "-vn",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "320k",
            "-ar",
            "44100",
            "-ac",
            "2",
            dest.to_string_lossy().as_ref(),
        ])
        .status()
        .map_err(|e| format!("Failed to run ffmpeg: {e}"))?;

    if !status.success() {
        return Err("ffmpeg HLS conversion failed".to_string());
    }
    if !dest.is_file() {
        return Err("ffmpeg finished but output file is missing".to_string());
    }
    emit_progress(app, page_url, "Converting to 320 kbps MP3…", Some(96.0));
    Ok(())
}

fn looks_like_m3u8_file(path: &Path) -> bool {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() < 8192 {
            if let Ok(head) = fs::read_to_string(path) {
                return head.contains("#EXTM3U") || head.contains("#EXTINF");
            }
        }
    }
    false
}

fn download_track_file(
    app: &AppHandle,
    cookie: &str,
    track: &VkTrack,
    dir: &Path,
    page_url: &str,
    index: usize,
    total: usize,
) -> Result<String, String> {
    check_cancel()?;

    if track.url.is_empty() || (!track.url.starts_with("http")) {
        return Err(format!(
            "No stream URL for {} — {}",
            track.artist, track.title
        ));
    }

    let base = sanitize_filename(&format!("{} - {}", track.artist, track.title));
    let dest = unique_path(dir, &base, "mp3");

    // Map multi-track downloads into a global 5%…95% window.
    let track_lo = if total > 1 {
        5.0 + (index as f32 / total as f32) * 90.0
    } else {
        5.0
    };
    let track_hi = if total > 1 {
        5.0 + ((index as f32 + 1.0) / total as f32) * 90.0
    } else {
        92.0
    };
    let label = if total > 1 {
        format!("Downloading {}/{}…", index + 1, total)
    } else {
        "Downloading…".to_string()
    };
    emit_progress(app, page_url, &label, Some(track_lo));

    // Prefer direct MP3 (keeps original VK bitrate, often 320 CBR).
    // Only fall back to ffmpeg re-encode for real HLS.
    let candidates = stream_url_candidates(&track.url);
    let mut last_err = String::from("no candidates");
    let mut downloaded = false;

    for cand in &candidates {
        check_cancel()?;
        if cand.contains(".m3u8") {
            continue;
        }
        match download_bytes(cand, cookie, &dest, app, page_url, track_lo, track_hi) {
            Ok(()) => {
                if looks_like_m3u8_file(&dest) {
                    let _ = fs::remove_file(&dest);
                    last_err = format!("got m3u8 body from {cand}");
                    continue;
                }
                // Reject tiny non-audio junk
                if let Ok(meta) = fs::metadata(&dest) {
                    if meta.len() < 16 * 1024 {
                        let _ = fs::remove_file(&dest);
                        last_err = format!("file too small from {cand}");
                        continue;
                    }
                }
                downloaded = true;
                break;
            }
            Err(err) => {
                let _ = fs::remove_file(&dest);
                last_err = err;
            }
        }
    }

    if !downloaded {
        // HLS re-encode at max 320 kbps CBR
        let hls = candidates
            .iter()
            .find(|u| u.contains(".m3u8"))
            .map(|s| s.as_str())
            .unwrap_or(track.url.as_str());
        if let Err(e2) = download_with_ffmpeg(app, hls, &dest, page_url, track_lo + 10.0) {
            return Err(format!(
                "Direct MP3 failed ({last_err}); ffmpeg fallback: {e2}"
            ));
        }
    }

    emit_progress(app, page_url, "Writing tags…", Some(track_hi.min(97.0)));
    let _ = metadata::write_track_tags(
        &dest,
        Some(track.title.as_str()),
        Some(track.artist.as_str()),
    );

    if let Err(err) = embed_track_cover(&dest, track, cookie) {
        eprintln!(
            "[vk_audio] cover not embedded for {} — {}: {err}",
            track.artist, track.title
        );
    }
    emit_progress(app, page_url, &label, Some(track_hi.min(99.0)));

    Ok(dest.to_string_lossy().to_string())
}

fn embed_track_cover(audio_path: &Path, track: &VkTrack, cookie: &str) -> Result<(), String> {
    let urls: Vec<&str> = track
        .covers
        .iter()
        .map(|s| s.as_str())
        .filter(|s| s.starts_with("http"))
        .collect();

    if urls.is_empty() {
        return Err("no cover URLs on track".to_string());
    }

    // Prefer larger images first (coverUrl_l / photo_600 usually earlier after sort)
    for url in urls {
        match download_cover_bytes(url, cookie) {
            Ok((bytes, mime)) if !bytes.is_empty() => {
                metadata::write_track_cover(audio_path, &bytes, mime.as_deref())?;
                return Ok(());
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("[vk_audio] cover download failed ({url}): {e}");
                continue;
            }
        }
    }

    Err("all cover URLs failed".to_string())
}

fn download_cover_bytes(url: &str, cookie: &str) -> Result<(Vec<u8>, Option<String>), String> {
    let mut response = http()
        .get(url)
        .header("User-Agent", DESKTOP_UA)
        .header("Cookie", cookie)
        .header("Referer", "https://vk.com/")
        .header("Accept", "image/avif,image/webp,image/apng,image/*,*/*;q=0.8")
        .call()
        .map_err(|e| format!("cover request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("cover HTTP {}", response.status().as_u16()));
    }

    let mime = response
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());

    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cover read failed: {e}"))?;

    if bytes.len() < 32 {
        return Err("cover too small".to_string());
    }

    Ok((bytes, mime))
}

// ── WebView session resolve (same context as VK Music Saver) ─────────────────

pub const VK_SESSION_WINDOW_LABEL: &str = "vk-session";

async fn ensure_session_webview(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(w) = app.get_webview_window(VK_SESSION_WINDOW_LABEL) {
        return Ok(w);
    }
    // Reuse open login window if user still has it.
    if let Some(w) = app.get_webview_window(VK_LOGIN_WINDOW_LABEL) {
        return Ok(w);
    }

    let url = "https://vk.com/feed"
        .parse()
        .map_err(|e| format!("Invalid session URL: {e}"))?;

    let window =
        WebviewWindowBuilder::new(app, VK_SESSION_WINDOW_LABEL, WebviewUrl::External(url))
            .title("VK Session")
            .inner_size(420.0, 320.0)
            .visible(false)
            .initialization_script(VK_INIT_SCRIPT)
            .build()
            .map_err(|e| format!("Failed to open VK session webview: {e}"))?;

    // Wait until the page has a chance to restore cookies / bootstrap.
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = window.eval(VK_INIT_SCRIPT);
        if let Some(probe) = probe_webview_login(&window).await {
            if probe.id > 0 || probe.logged_in_ui || probe.href.as_deref().is_some_and(url_looks_logged_in)
            {
                break;
            }
        }
    }

    // Refresh exported cookie jar from the live WebView session.
    if let Ok(pairs) = collect_vk_cookies_from_window(&window) {
        if has_session_cookie_pairs(&pairs) {
            if let Some(path) = cookie_file_path(app) {
                let _ = write_netscape_cookies(&path, &pairs);
            }
            if let Some(probe) = probe_webview_login(&window).await {
                if probe.id > 0 {
                    let _ = save_session_meta(
                        app,
                        &VkSessionMeta {
                            user_id: probe.id,
                            user_name: probe.name,
                        },
                    );
                }
            }
        }
    }

    Ok(window)
}

fn track_from_api_object(obj: &Value, user_id: i64) -> Option<VkTrack> {
    parse_track_value(obj, user_id).or_else(|| {
        let map = obj.as_object()?;
        parse_audio_object(map, user_id)
    })
}

/// Resolve many audio IDs inside the authenticated session WebView (getById +
/// al_audio reload with browser cookies). Plain HTTP reload returns antibot stubs.
async fn webview_resolve_audios_batch(
    app: &AppHandle,
    id_tokens: &[String],
    user_id: i64,
) -> Result<Vec<VkTrack>, String> {
    if id_tokens.is_empty() {
        return Ok(Vec::new());
    }
    let window = ensure_session_webview(app).await?;
    let ids_json = serde_json::to_string(id_tokens).unwrap_or_else(|_| "[]".into());

    let start_js = format!(
        r#"
(async function () {{
  window.__muzeekaAudioBatch = null;
  try {{
    const ids = {ids_json};
    let tracks = [];
    let lastText = '';

    if (window.vkApi && typeof window.vkApi.api === 'function') {{
      try {{
        const joined = ids.join(',');
        const res = await window.vkApi.api('audio.getById', {{ audios: joined, v: '5.204' }});
        if (Array.isArray(res)) {{
          tracks = res.filter(t => t && t.url);
        }} else if (res && Array.isArray(res.response)) {{
          tracks = res.response.filter(t => t && t.url);
        }}
      }} catch (e) {{}}
    }}

    if (!tracks.length) {{
      const body = new URLSearchParams();
      body.set('act', 'reload_audio');
      body.set('al', '1');
      body.set('ids', ids.join(','));
      const endpoints = [
        (location.origin || 'https://vk.com') + '/al_audio.php',
        'https://vk.com/al_audio.php',
        'https://vk.ru/al_audio.php',
        'https://m.vk.ru/audio'
      ];
      for (const ep of endpoints) {{
        try {{
          const r = await fetch(ep, {{
            method: 'POST',
            headers: {{
              'Content-Type': 'application/x-www-form-urlencoded',
              'X-Requested-With': 'XMLHttpRequest'
            }},
            body: body.toString(),
            credentials: 'include'
          }});
          const text = await r.text();
          if (text && text.length > 2) {{ lastText = text; break; }}
        }} catch (e) {{}}
      }}
      window.__muzeekaAudioBatch = JSON.stringify({{
        ok: true,
        source: 'al_audio',
        text: (lastText || '').slice(0, 400000)
      }});
      return;
    }}

    window.__muzeekaAudioBatch = JSON.stringify({{
      ok: true,
      source: 'vkApi',
      tracks
    }});
  }} catch (e) {{
    window.__muzeekaAudioBatch = JSON.stringify({{ ok: false, error: String(e) }});
  }}
}})();
true
"#
    );

    window
        .eval(&start_js)
        .map_err(|e| format!("Failed to run VK batch resolve script: {e}"))?;

    for _ in 0..60 {
        check_cancel()?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let Some(raw) = eval_js_string(&window, "window.__muzeekaAudioBatch").await else {
            continue;
        };
        let Some(value) = decode_eval_json(&raw) else {
            continue;
        };
        if value.is_null() {
            continue;
        }

        let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let err = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown webview error");
            return Err(format!("VK webview batch resolve failed: {err}"));
        }

        let mut out = Vec::new();
        if let Some(list) = value.get("tracks").and_then(|v| v.as_array()) {
            for item in list {
                if let Some(track) = track_from_api_object(item, user_id) {
                    if has_playable_stream(&track) {
                        out.push(track);
                    }
                }
            }
        }
        if out.is_empty() {
            if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
                out.extend(
                    tracks_from_reload_json(text, user_id)
                        .into_iter()
                        .filter(|t| has_playable_stream(t)),
                );
            }
        }
        return Ok(out);
    }

    Err("Timed out waiting for VK webview batch resolve".to_string())
}

async fn webview_resolve_audio(
    app: &AppHandle,
    owner_id: i64,
    audio_id: u64,
    access_key: Option<&str>,
    user_id: i64,
) -> Result<VkTrack, String> {
    let ak = access_key.unwrap_or("").trim();
    let mut id_candidates = Vec::new();
    if !ak.is_empty() {
        id_candidates.push(format!("{owner_id}_{audio_id}_{ak}"));
    }
    id_candidates.push(format!("{owner_id}_{audio_id}"));

    let tracks = webview_resolve_audios_batch(app, &id_candidates[..1], user_id).await?;
    if let Some(mut track) = tracks.into_iter().find(|t| {
        has_playable_stream(t) && (t.id == audio_id || (t.owner_id == owner_id && t.id != 0))
    }) {
        if track.owner_id == 0 {
            track.owner_id = owner_id;
        }
        if track.id == 0 {
            track.id = audio_id;
        }
        if has_playable_stream(&track) {
            return Ok(track);
        }
    }

    // Second try: alternate id form without access_key
    if id_candidates.len() > 1 {
        let tracks = webview_resolve_audios_batch(app, &id_candidates[1..], user_id).await?;
        if let Some(mut track) = tracks.into_iter().find(has_playable_stream) {
            if track.owner_id == 0 {
                track.owner_id = owner_id;
            }
            if track.id == 0 {
                track.id = audio_id;
            }
            return Ok(track);
        }
    }

    Err(format!(
        "VK returned no stream URL for audio{owner_id}_{audio_id} (webview)"
    ))
}

fn merge_entry_meta(mut track: VkTrack, entry: &PlaylistEntry) -> VkTrack {
    if (track.title.is_empty() || track.title == "Unknown" || track.title.starts_with("Track "))
        && !entry.title.is_empty()
        && entry.title != "Unknown"
    {
        track.title = entry.title.clone();
    }
    if (track.artist.is_empty() || track.artist == "Unknown")
        && !entry.artist.is_empty()
        && entry.artist != "Unknown"
    {
        track.artist = entry.artist.clone();
    }
    if track.duration == 0 && entry.duration > 0 {
        track.duration = entry.duration;
    }
    if track.covers.is_empty() && !entry.covers.is_empty() {
        track.covers = entry.covers.clone();
    }
    if track.owner_id == 0 {
        track.owner_id = entry.owner_id;
    }
    if track.id == 0 {
        track.id = entry.id;
    }
    track
}

/// Resolve playlist streams via session WebView (same path as single-track downloads).
async fn fetch_playlist_tracks_async(
    app: &AppHandle,
    cookie: &str,
    user_id: i64,
    owner_id: i64,
    playlist_id: u64,
    access_hash: Option<&str>,
) -> Result<(String, Option<String>, Vec<VkTrack>), String> {
    let cookie = cookie.to_string();
    let access_hash_owned = access_hash.map(|s| s.to_string());
    let (title, thumb, entries) = tauri::async_runtime::spawn_blocking({
        let cookie = cookie.clone();
        move || {
            fetch_playlist_catalog(
                &cookie,
                owner_id,
                playlist_id,
                access_hash_owned.as_deref(),
                user_id,
            )
        }
    })
    .await
    .map_err(|e| format!("Playlist catalog task failed: {e}"))??;

    if entries.is_empty() {
        return Err("VK playlist is empty or unavailable".to_string());
    }

    emit_progress(
        app,
        &format!("playlist:{owner_id}_{playlist_id}"),
        &format!("Resolving {} tracks…", entries.len()),
        Some(5.0),
    );

    let mut resolved: Vec<VkTrack> = Vec::new();
    let mut unresolved: Vec<PlaylistEntry> = Vec::new();

    // Prefer getById access_key form in batches of 8 (API is picky about long lists).
    for (chunk_idx, chunk) in entries.chunks(8).enumerate() {
        check_cancel()?;
        let tokens: Vec<String> = chunk
            .iter()
            .flat_map(|e| {
                // Prefer access_key form first for each track
                let mut t = entry_getbyid_tokens(e);
                // Also offer full reload token as last resort in same batch? No —
                // mixed formats confuse getById. Use access tokens only here.
                if t.is_empty() {
                    t.push(format!("{}_{}", e.owner_id, e.id));
                }
                // Only first (best) token per track for batch
                t.into_iter().take(1)
            })
            .collect();

        let pct = 5.0 + (chunk_idx as f32 / (entries.len() as f32 / 8.0).max(1.0)) * 25.0;
        emit_progress(
            app,
            &format!("playlist:{owner_id}_{playlist_id}"),
            &format!(
                "Resolving tracks {}–{}…",
                chunk_idx * 8 + 1,
                (chunk_idx * 8 + chunk.len()).min(entries.len())
            ),
            Some(pct.min(30.0)),
        );

        match webview_resolve_audios_batch(app, &tokens, user_id).await {
            Ok(tracks) => {
                let mut by_key: std::collections::HashMap<(i64, u64), VkTrack> =
                    std::collections::HashMap::new();
                for t in tracks {
                    by_key.insert((t.owner_id, t.id), t);
                }
                for entry in chunk {
                    if let Some(track) = by_key.remove(&(entry.owner_id, entry.id)) {
                        if has_playable_stream(&track) {
                            resolved.push(merge_entry_meta(track, entry));
                            continue;
                        }
                    }
                    // Match loosely if owner_id differs (rare)
                    if let Some((_, track)) = by_key.iter().find(|(_, t)| t.id == entry.id) {
                        let track = track.clone();
                        if has_playable_stream(&track) {
                            resolved.push(merge_entry_meta(track, entry));
                            by_key.retain(|_, t| t.id != entry.id);
                            continue;
                        }
                    }
                    unresolved.push(entry.clone());
                }
            }
            Err(err) => {
                eprintln!("[vk_audio] batch resolve failed: {err}");
                unresolved.extend(chunk.iter().cloned());
            }
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Per-track webview resolve for leftovers
    let need_retry = std::mem::take(&mut unresolved);
    let mut still_missing = Vec::new();
    for entry in need_retry {
        check_cancel()?;
        match webview_resolve_audio(
            app,
            entry.owner_id,
            entry.id,
            if entry.access_key.is_empty() {
                None
            } else {
                Some(entry.access_key.as_str())
            },
            user_id,
        )
        .await
        {
            Ok(track) if has_playable_stream(&track) => {
                resolved.push(merge_entry_meta(track, &entry));
            }
            Ok(_) | Err(_) => {
                still_missing.push(entry);
            }
        }
    }

    // Last resort: HTTP reload (often stubs — filter aggressively)
    if !still_missing.is_empty() {
        eprintln!(
            "[vk_audio] {} playlist tracks left for HTTP fallback",
            still_missing.len()
        );
        let tokens: Vec<String> = still_missing.iter().map(entry_id_token).collect();
        let cookie2 = cookie.clone();
        let http_tracks = tauri::async_runtime::spawn_blocking(move || {
            reload_audio_ids(&cookie2, user_id, &tokens)
        })
        .await
        .map_err(|e| format!("HTTP fallback task failed: {e}"))??;

        let mut by_key: std::collections::HashMap<(i64, u64), VkTrack> =
            std::collections::HashMap::new();
        for t in http_tracks {
            if has_playable_stream(&t) {
                by_key.insert((t.owner_id, t.id), t);
            }
        }
        for entry in &still_missing {
            if let Some(track) = by_key.remove(&(entry.owner_id, entry.id)) {
                resolved.push(merge_entry_meta(track, entry));
            }
        }
    }

    // Dedup by (owner, id), preserve order
    let mut seen = std::collections::HashSet::new();
    resolved.retain(|t| seen.insert((t.owner_id, t.id)));
    resolved.retain(|t| has_playable_stream(t));

    if resolved.is_empty() {
        return Err(
            "Could not resolve any playable streams for this VK playlist. \
             Tracks look like restriction stubs — log out/in VK in Settings and try again."
                .to_string(),
        );
    }

    let thumb = thumb.or_else(|| resolved.first().and_then(|t| t.covers.first().cloned()));
    Ok((title, thumb, resolved))
}

async fn fetch_single_track_async(
    app: &AppHandle,
    cookie: &str,
    user_id: i64,
    owner_id: i64,
    audio_id: u64,
    access_key: Option<&str>,
) -> Result<VkTrack, String> {
    // Primary: resolve inside authenticated WebView (same as VK Music Saver).
    match webview_resolve_audio(app, owner_id, audio_id, access_key, user_id).await {
        Ok(track) if has_playable_stream(&track) => return Ok(track),
        Ok(track) => {
            eprintln!(
                "[vk_audio] webview returned stub/unplayable for {}_{}: {} — {}",
                owner_id, audio_id, track.artist, track.title
            );
        }
        Err(err) => eprintln!("[vk_audio] webview resolve failed: {err}"),
    }

    // Fallback: pure HTTP (often blocked by antibot / returns stubs)
    let track = fetch_single_track(cookie, user_id, owner_id, audio_id, access_key)?;
    if has_playable_stream(&track) {
        Ok(track)
    } else {
        Err(format!(
            "VK returned a restriction stub for audio{owner_id}_{audio_id} (\"{}\" — {}). \
             Log out/in VK in Settings and try again.",
            track.artist, track.title
        ))
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

pub async fn probe_async(app: AppHandle, url: String) -> Result<YtdlpProbeResult, String> {
    let target = parse_target(&url)?;
    let cookie = load_cookie_header(&app).unwrap_or_default();
    let user_id = resolve_user_id_for_app(&app, &cookie).unwrap_or(0);

    // Ensure live session + refresh cookies/user id when possible.
    let _ = ensure_session_webview(&app).await;
    let cookie = load_cookie_header(&app).unwrap_or(cookie);
    let user_id = resolve_user_id_for_app(&app, &cookie).unwrap_or(user_id);

    match target {
        VkTarget::Track {
            owner_id,
            audio_id,
            access_key,
        } => {
            let track = fetch_single_track_async(
                &app,
                &cookie,
                user_id,
                owner_id,
                audio_id,
                access_key.as_deref(),
            )
            .await?;
            Ok(YtdlpProbeResult {
                title: track.title.clone(),
                uploader: Some(track.artist),
                duration_secs: if track.duration > 0 {
                    Some(track.duration as f64)
                } else {
                    None
                },
                thumbnail: track.covers.first().cloned(),
                is_playlist: false,
                entry_count: None,
            })
        }
        VkTarget::Playlist {
            owner_id,
            playlist_id,
            access_hash,
        } => {
            let cookie = load_cookie_header(&app).unwrap_or(cookie);
            let user_id = resolve_user_id_for_app(&app, &cookie).unwrap_or(user_id);
            // Catalog only — no stream resolve (HTTP reload returns stubs).
            let (title, thumb, entries) = tauri::async_runtime::spawn_blocking({
                let cookie = cookie.clone();
                let access_hash = access_hash.clone();
                move || {
                    fetch_playlist_catalog(
                        &cookie,
                        owner_id,
                        playlist_id,
                        access_hash.as_deref(),
                        user_id,
                    )
                }
            })
            .await
            .map_err(|e| format!("Playlist task failed: {e}"))??;

            Ok(YtdlpProbeResult {
                title,
                uploader: None,
                duration_secs: None,
                thumbnail: thumb.or_else(|| entries.first().and_then(|e| e.covers.first().cloned())),
                is_playlist: true,
                entry_count: Some(entries.len() as u32),
            })
        }
    }
}

pub async fn download_async(
    app: AppHandle,
    url: String,
    output_dir: Option<String>,
    allow_playlist: bool,
) -> Result<YtdlpDownloadResult, String> {
    CANCELLED.store(false, Ordering::SeqCst);

    let target = parse_target(&url)?;
    let _ = ensure_session_webview(&app).await;
    let cookie = load_cookie_header(&app).unwrap_or_default();
    let user_id = resolve_user_id_for_app(&app, &cookie).unwrap_or(0);
    let dir = ytdlp::resolve_download_dir(&app, output_dir.as_deref())?;

    emit_progress(&app, &url, "Resolving VK track…", Some(2.0));

    let tracks = match target {
        VkTarget::Track {
            owner_id,
            audio_id,
            access_key,
        } => {
            vec![
                fetch_single_track_async(
                    &app,
                    &cookie,
                    user_id,
                    owner_id,
                    audio_id,
                    access_key.as_deref(),
                )
                .await?,
            ]
        }
        VkTarget::Playlist {
            owner_id,
            playlist_id,
            access_hash,
        } => {
            if !allow_playlist {
                return Err(
                    "This is a VK playlist. Confirm download to fetch all tracks.".to_string(),
                );
            }
            emit_progress(
                &app,
                &url,
                "Loading VK playlist…",
                Some(3.0),
            );
            let (_title, _thumb, tracks) = fetch_playlist_tracks_async(
                &app,
                &cookie,
                user_id,
                owner_id,
                playlist_id,
                access_hash.as_deref(),
            )
            .await?;
            tracks
        }
    };

    // Drop restriction stubs ("listen in the official app") if any slipped through.
    let tracks: Vec<VkTrack> = tracks
        .into_iter()
        .filter(|t| {
            if has_playable_stream(t) {
                true
            } else {
                eprintln!(
                    "[vk_audio] skip stub {}_{}: {} — {}",
                    t.owner_id, t.id, t.artist, t.title
                );
                false
            }
        })
        .collect();

    if tracks.is_empty() {
        return Err(
            "No playable VK tracks to download (all looked like restriction stubs). \
             Re-login to VK in Settings and try again."
                .to_string(),
        );
    }

    let total = tracks.len();
    let mut paths = Vec::new();
    let cookie = load_cookie_header(&app).unwrap_or(cookie);

    for (i, track) in tracks.iter().enumerate() {
        check_cancel()?;
        match download_track_file(&app, &cookie, track, &dir, &url, i, total) {
            Ok(path) => paths.push(path),
            Err(err) => {
                eprintln!("[vk_audio] skip track {}_{}: {err}", track.owner_id, track.id);
                if total == 1 {
                    return Err(err);
                }
            }
        }
    }

    if paths.is_empty() {
        return Err("VK download finished but no audio files were saved".to_string());
    }

    emit_progress(&app, &url, "Processing files…", Some(100.0));
    let mut files = library::fetch_metadata(&paths)?;

    for (file, track) in files.iter_mut().zip(tracks.iter()) {
        if file.title.is_none() {
            file.title = Some(track.title.clone());
        }
        if file.artist.is_none() {
            file.artist = Some(track.artist.clone());
        }
        if file.duration_secs.is_none() && track.duration > 0 {
            file.duration_secs = Some(track.duration as f64);
        }
    }

    emit_progress(&app, &url, "Done", Some(100.0));
    Ok(YtdlpDownloadResult { files })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_audio_urls() {
        assert!(is_vk_audio_url("https://vk.com/audio-2000123456_123456789"));
        assert!(is_vk_audio_url("https://vk.ru/music/playlist/1_2"));
        assert!(is_vk_audio_url("https://m.vk.com/audio123_456"));
        assert!(!is_vk_audio_url("https://vk.com/video-1_2"));
        assert!(!is_vk_audio_url("https://youtube.com/watch?v=x"));
    }

    #[test]
    fn parses_track_and_playlist() {
        match parse_target("https://vk.com/audio-2000123_456").unwrap() {
            VkTarget::Track {
                owner_id, audio_id, ..
            } => {
                assert_eq!(owner_id, -2000123);
                assert_eq!(audio_id, 456);
            }
            _ => panic!("expected track"),
        }

        match parse_target("https://vk.ru/music/album/-2000_55_abcdef").unwrap() {
            VkTarget::Playlist {
                owner_id,
                playlist_id,
                access_hash,
            } => {
                assert_eq!(owner_id, -2000);
                assert_eq!(playlist_id, 55);
                assert_eq!(access_hash.as_deref(), Some("abcdef"));
            }
            _ => panic!("expected playlist"),
        }
    }

    #[test]
    fn m3u8_rewrite() {
        let url = "https://psv4.userapi.com/s/v1/ab12/audios/cd34/index.m3u8";
        let out = convert_m3u8_to_mp3(url);
        assert!(out.ends_with("/audios/cd34.mp3"), "{out}");
    }

    #[test]
    fn prefers_direct_mp3_before_hls() {
        let url = "https://psv4.userapi.com/s/v1/ab12/audios/cd34/index.m3u8";
        let c = stream_url_candidates(url);
        assert!(c.len() >= 2);
        assert!(!c[0].contains("m3u8"), "first candidate should be mp3: {}", c[0]);
        assert!(c.last().unwrap().contains("m3u8"));
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_filename("a/b:c*"), "a_b_c_");
    }

    #[test]
    fn guest_tracking_cookies_are_not_session() {
        let guest = vec![
            ("remixlang".into(), "0".into()),
            ("remixstlid".into(), "9060780302042340642_FHxYXzorWNjYVoYCZe0Si0aFAwdX23H35Ifuezh4180".into()),
            ("remixstid".into(), "1234567890abcdef".into()),
        ];
        assert!(!has_session_cookie_pairs(&guest));

        let real = vec![
            ("remixlang".into(), "0".into()),
            ("remixsid".into(), "1_deadbeef_cafebabe_session_token_xx".into()),
        ];
        assert!(has_session_cookie_pairs(&real));

        let deleted = vec![("remixsid".into(), "DELETED".into())];
        assert!(!has_session_cookie_pairs(&deleted));
    }
}
