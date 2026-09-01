//! Sidecar ICY reader for internet radio.
//!
//! BASS_StreamCreateURL with a mixer DECODE source strips in-band ICY metadata
//! (DOWNLOADPROC sees audio only, BASS_SYNC_META fires with a null pointer).
//! A second HTTP GET with `Icy-MetaData: 1` parses `icy-metaint` blocks so
//! `StreamTitle` can reach the UI.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use encoding_rs::WINDOWS_1251;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_SLEEP: Duration = Duration::from_secs(2);
const USER_AGENT: &str = "WinampMPEG/5.09";
const SCAN_TAIL: usize = 256;

#[derive(Debug, Default, Clone)]
pub(crate) struct LiveMetaInbox {
    /// Bumped on teardown so a dying tap cannot write into the next stream.
    pub gen: u64,
    pub meta: Option<String>,
    pub icy: Option<String>,
    pub http: Option<String>,
}

impl LiveMetaInbox {
    fn put_meta(&mut self, gen: u64, raw: String) {
        if self.gen == gen {
            self.meta = Some(raw);
        }
    }

    fn put_headers(&mut self, gen: u64, icy: Option<String>, http: Option<String>) {
        if self.gen != gen {
            return;
        }
        if icy.is_some() {
            self.icy = icy;
        }
        if http.is_some() {
            self.http = http;
        }
    }
}

#[derive(Default)]
struct IcyParse {
    metaint: u32,
    audio_left: u32,
    meta_left: u32,
    meta: Vec<u8>,
}

/// Spawn a background ICY tap. The thread exits after `stop` is set and the
/// current read unblocks (live audio arrives continuously). Do not join it on
/// the BASS thread — detach and let it notice `stop`.
pub(crate) fn spawn(
    url: String,
    inbox: Arc<StdMutex<LiveMetaInbox>>,
    stop: Arc<AtomicBool>,
    gen: u64,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("muzeeka-icy-tap".into())
        .spawn(move || run_loop(&url, &inbox, &stop, gen))
        .expect("spawn icy tap")
}

fn run_loop(url: &str, inbox: &StdMutex<LiveMetaInbox>, stop: &AtomicBool, gen: u64) {
    let url = url.split("\r\n").next().unwrap_or(url).trim();
    crate::stream_debug::log(format!("icy tap start {url}"));
    while !stop.load(Ordering::Relaxed) {
        match tap_once(url, inbox, stop, gen) {
            Ok(()) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                crate::stream_debug::log("icy tap connection ended — retry");
            }
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                crate::stream_debug::log(format!("icy tap error: {e}"));
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(RETRY_SLEEP);
    }
    crate::stream_debug::log("icy tap stop");
}

fn tap_once(
    url: &str,
    inbox: &StdMutex<LiveMetaInbox>,
    stop: &AtomicBool,
    gen: u64,
) -> Result<(), String> {
    let config = ureq::config::Config::builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(HEADER_TIMEOUT))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut response = agent
        .get(url)
        .header("Icy-MetaData", "1")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "*/*")
        .header("Accept-Encoding", "identity")
        .call()
        .map_err(|e| format!("connect: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }

    let mut http_raw = format!("HTTP {} {}\0", status.as_u16(), status.canonical_reason().unwrap_or(""));
    let mut metaint = 0u32;
    let mut station: Option<String> = None;
    for (name, value) in response.headers().iter() {
        let key = name.as_str();
        let val = header_text(value);
        http_raw.push_str(key);
        http_raw.push(':');
        http_raw.push_str(&val);
        http_raw.push('\0');
        if key.eq_ignore_ascii_case("icy-metaint") || key.eq_ignore_ascii_case("ice-metaint") {
            if let Ok(n) = val.trim().parse::<u32>() {
                if n > 0 && n < 1_000_000 {
                    metaint = n;
                }
            }
        }
        if key.eq_ignore_ascii_case("icy-name") || key.eq_ignore_ascii_case("ice-name") {
            let name = val.trim();
            if !name.is_empty() {
                station = Some(name.to_string());
            }
        }
    }

    crate::stream_debug::log(format!(
        "icy tap headers: {}",
        http_raw.replace('\0', " | ").chars().take(400).collect::<String>()
    ));
    if metaint > 0 {
        crate::stream_debug::log(format!("icy tap metaint={metaint}"));
    } else {
        crate::stream_debug::log("icy tap: no icy-metaint — scanning body for StreamTitle");
    }
    if let Some(name) = station.as_deref() {
        crate::stream_debug::log(format!("icy tap icy-name={name}"));
    }

    if let Ok(mut g) = inbox.lock() {
        g.put_headers(
            gen,
            station.map(|n| format!("icy-name:{n}")),
            Some(http_raw),
        );
    }

    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut parse = IcyParse {
        metaint,
        audio_left: metaint,
        meta_left: 0,
        meta: Vec::new(),
    };
    let mut last_title: Option<String> = None;
    let mut scan_tail = Vec::new();
    let mut buf = [0u8; 8192];

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read: {e}")),
        };
        let chunk = &buf[..n];

        if metaint > 0 {
            feed_icy_bytes(&mut parse, chunk, |raw| {
                publish_title(raw, inbox, gen, &mut last_title);
            });
        }

        // Alignment-independent fallback: `StreamTitle='…'` is unique in AAC/MP3.
        scan_tail.extend_from_slice(chunk);
        if let Some(title) = scan_stream_title(&scan_tail) {
            publish_title(&format!("StreamTitle='{title}';"), inbox, gen, &mut last_title);
        }
        if scan_tail.len() > SCAN_TAIL {
            let keep = SCAN_TAIL;
            let drop_n = scan_tail.len() - keep;
            scan_tail.drain(..drop_n);
        }
    }
}

fn publish_title(
    raw: &str,
    inbox: &StdMutex<LiveMetaInbox>,
    gen: u64,
    last: &mut Option<String>,
) {
    let Some(title) = parse_icy_stream_title(raw) else {
        return;
    };
    if last.as_deref() == Some(title.as_str()) {
        return;
    }
    *last = Some(title.clone());
    crate::stream_debug::log(format!("icy tap StreamTitle='{title}'"));
    if let Ok(mut g) = inbox.lock() {
        g.put_meta(gen, format!("StreamTitle='{title}';"));
    }
}

fn feed_icy_bytes(parse: &mut IcyParse, data: &[u8], mut on_meta: impl FnMut(&str)) {
    let metaint = parse.metaint;
    if metaint == 0 {
        return;
    }
    let mut i = 0usize;
    while i < data.len() {
        if parse.meta_left > 0 {
            let n = (parse.meta_left as usize).min(data.len() - i);
            parse.meta.extend_from_slice(&data[i..i + n]);
            parse.meta_left -= n as u32;
            i += n;
            if parse.meta_left == 0 {
                let raw = decode_icy_bytes(&parse.meta);
                parse.meta.clear();
                parse.audio_left = metaint;
                on_meta(&raw);
            }
            continue;
        }
        if parse.audio_left == 0 {
            let len = u32::from(data[i]) * 16;
            i += 1;
            if len == 0 {
                parse.audio_left = metaint;
            } else {
                parse.meta_left = len;
                parse.meta.clear();
                parse.meta.reserve(len as usize);
            }
            continue;
        }
        let n = (parse.audio_left as usize).min(data.len() - i);
        parse.audio_left -= n as u32;
        i += n;
    }
}

fn header_text(value: &ureq::http::HeaderValue) -> String {
    if let Ok(s) = value.to_str() {
        return s.trim().to_string();
    }
    decode_icy_bytes(value.as_bytes())
}

fn decode_icy_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.trim().to_string();
    }
    WINDOWS_1251.decode(bytes).0.trim().to_string()
}

/// Extract the current track from an ICY metadata block.
/// `StreamTitle='Artist - Song';StreamUrl='...';`
pub(crate) fn parse_icy_stream_title(raw: &str) -> Option<String> {
    let key = "StreamTitle=";
    let start = raw.find(key)? + key.len();
    let rest = &raw[start..];
    let inner = if let Some(stripped) = rest.strip_prefix('\'') {
        let end = stripped.find("';").unwrap_or(stripped.len());
        &stripped[..end]
    } else if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"').unwrap_or(stripped.len());
        &stripped[..end]
    } else {
        let end = rest.find(';').unwrap_or(rest.len());
        rest[..end].trim()
    };
    let title = inner.trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

fn scan_stream_title(data: &[u8]) -> Option<String> {
    for key in [b"StreamTitle='".as_slice(), b"StreamTitle=\"".as_slice()] {
        let Some(pos) = data.windows(key.len()).position(|w| w == key) else {
            continue;
        };
        let quote = *key.last()?;
        let rest = &data[pos + key.len()..];
        let end = rest
            .iter()
            .position(|&b| b == quote)
            .unwrap_or(rest.len().min(200));
        if end == 0 {
            continue;
        }
        let title = decode_icy_bytes(&rest[..end]);
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_stream_title() {
        let raw = "StreamTitle='Artist - Song';StreamUrl='http://x';";
        assert_eq!(
            parse_icy_stream_title(raw).as_deref(),
            Some("Artist - Song")
        );
    }

    #[test]
    fn skips_empty_stream_title() {
        assert!(parse_icy_stream_title("StreamTitle='';").is_none());
    }

    #[test]
    fn scans_in_band_bytes() {
        let mut buf = vec![0u8; 40];
        buf.extend_from_slice(b"StreamTitle='Jungle Track';");
        buf.extend_from_slice(&[1, 2, 3]);
        assert_eq!(scan_stream_title(&buf).as_deref(), Some("Jungle Track"));
    }
}
