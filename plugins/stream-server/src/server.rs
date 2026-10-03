use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::encode::Format;
use crate::meta::{icy_block, mux_icy};
use crate::{Config, Shared, STOP};

const METAINT: usize = 8192;

pub fn serve(listener: TcpListener, cfg: Arc<Config>, shared: Arc<Shared>) {
    let mut clients: Vec<JoinHandle<()>> = Vec::new();
    while !STOP.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let cfg = cfg.clone();
                let shared = shared.clone();
                if let Ok(handle) = thread::Builder::new()
                    .name("stream-client".into())
                    .spawn(move || handle_client(stream, cfg, shared))
                {
                    clients.push(handle);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
        let (done, alive): (Vec<_>, Vec<_>) = clients.drain(..).partition(|h| h.is_finished());
        clients = alive;
        for h in done {
            let _ = h.join();
        }
    }
    for h in clients {
        let _ = h.join();
    }
}

struct Request {
    path: String,
    query: String,
    icy: bool,
    /// Browser address-bar navigation. An `<audio>` or `fetch` of the same URL
    /// must still receive the raw stream.
    page: bool,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while buf.len() < 8192 {
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let icy = wants_icy(&text);
    let page = wants_page(&text);
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" {
        return None;
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    Some(Request {
        path,
        query,
        icy,
        page,
    })
}

fn header_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
}

/// Chrome and Edge play a naked `audio/mpeg` response only after their own
/// pipeline has stored many seconds. A document navigation gets a page that
/// keeps the live edge itself. Media fetches do not send `text/html`.
fn wants_page(text: &str) -> bool {
    let dest = header_value(text, "sec-fetch-dest").unwrap_or("");
    if dest.eq_ignore_ascii_case("audio") || dest.eq_ignore_ascii_case("empty") {
        return false;
    }
    if dest.eq_ignore_ascii_case("document") {
        return true;
    }
    header_value(text, "accept").is_some_and(|v| v.to_ascii_lowercase().contains("text/html"))
}

fn wants_icy(text: &str) -> bool {
    text.lines().skip(1).any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.trim().eq_ignore_ascii_case("icy-metadata") && value.trim() == "1"
    })
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn tokens_match(expected: &str, given: &str) -> bool {
    let a = expected.as_bytes();
    let b = given.as_bytes();
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= (*a.get(i).unwrap_or(&0) ^ *b.get(i).unwrap_or(&0)) as usize;
    }
    diff == 0
}

fn player_html(format: Format) -> String {
    let (path, mime) = match format {
        Format::Mp3 => ("/stream.mp3", "audio/mpeg"),
        Format::Opus => ("/stream.opus", "audio/ogg"),
    };
    format!(
        r#"<!doctype html>
<html lang="ru">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Muzeeka</title>
<style>
  html, body {{ margin: 0; height: 100%; background: #141414; color: #eee; font: 16px/1.4 sans-serif; }}
  main {{ min-height: 100%; display: grid; place-items: center; }}
  div {{ width: min(28rem, calc(100% - 2rem)); }}
  audio {{ width: 100%; }}
  button {{ margin-top: 0.8rem; font: inherit; padding: 0.4rem 0.9rem; }}
  p {{ color: #aaa; }}
</style>
<main>
  <div>
    <audio id="a" controls autoplay></audio>
    <button id="play" hidden type="button">Слушать</button>
    <p id="err" hidden>Поток не открылся.</p>
  </div>
</main>
<script>
const STREAM = "{path}" + location.search;
const MIME = "{mime}";
const audio = document.getElementById("a");
const play = document.getElementById("play");
const err = document.getElementById("err");
function showPlay() {{ play.hidden = false; }}
function fail() {{ err.hidden = false; }}
play.onclick = () => {{ play.hidden = true; audio.play().catch(showPlay); }};
function live() {{
  if (audio.paused || audio.seeking || !audio.buffered.length) return;
  if (!audio.seekable.length) return;
  const end = audio.buffered.end(audio.buffered.length - 1);
  if (end - audio.currentTime > 0.8) {{
    try {{ audio.currentTime = Math.max(0, end - 0.3); }} catch (e) {{}}
  }}
}}
function plain() {{
  audio.src = STREAM;
  audio.play().catch(showPlay);
  setInterval(live, 250);
}}
function startMse() {{
  const ms = new MediaSource();
  audio.src = URL.createObjectURL(ms);
  ms.addEventListener("sourceopen", async () => {{
    let sb;
    try {{ sb = ms.addSourceBuffer(MIME); }} catch (e) {{ plain(); return; }}
    sb.mode = "sequence";
    const queue = [];
    let busy = false;
    function pump() {{
      if (busy || sb.updating || queue.length === 0) return;
      busy = true;
      try {{ sb.appendBuffer(queue.shift()); }}
      catch (e) {{ busy = false; fail(); }}
    }}
    sb.addEventListener("updateend", () => {{
      busy = false;
      if (audio.buffered.length && !sb.updating) {{
        const start = audio.buffered.start(0);
        const end = audio.buffered.end(audio.buffered.length - 1);
        if (end - start > 8) {{
          try {{ sb.remove(start, Math.max(start, audio.currentTime - 1)); return; }} catch (e) {{}}
        }}
      }}
      live();
      pump();
    }});
    sb.addEventListener("error", fail);
    audio.play().catch(showPlay);
    try {{
      const res = await fetch(STREAM);
      if (!res.ok || !res.body) {{ fail(); return; }}
      const reader = res.body.getReader();
      while (true) {{
        const chunk = await reader.read();
        if (chunk.done) break;
        queue.push(chunk.value);
        pump();
      }}
    }} catch (e) {{ fail(); }}
  }});
}}
if (window.MediaSource && MediaSource.isTypeSupported(MIME) && MIME === "audio/mpeg") startMse();
else plain();
</script>
"#
    )
}

fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn handle_client(mut stream: TcpStream, cfg: Arc<Config>, shared: Arc<Shared>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_nodelay(true);

    let Some(req) = read_request(&mut stream) else {
        respond(&mut stream, "400 Bad Request", "text/plain", "bad request");
        return;
    };

    if !cfg.token.is_empty() {
        let given = query_value(&req.query, "token").map(percent_decode).unwrap_or_default();
        if !tokens_match(&cfg.token, &given) {
            respond(&mut stream, "401 Unauthorized", "text/plain", "token required");
            return;
        }
    }

    match stream_route(&req.path, cfg.format) {
        Route::Audio if req.page => {
            respond(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                &player_html(cfg.format),
            );
        }
        Route::Audio => stream_audio(stream, &cfg, &shared, req.icy),
        Route::Wrong => {
            let hint = format!("use /stream.{}", cfg.format.as_str());
            respond(&mut stream, "404 Not Found", "text/plain", &hint);
        }
        Route::Missing => respond(&mut stream, "404 Not Found", "text/plain", "not found"),
    }
}

enum Route {
    Audio,
    Wrong,
    Missing,
}

fn stream_route(path: &str, format: Format) -> Route {
    if path == "/" || path == "/stream" {
        return Route::Audio;
    }
    let opus = path == "/stream.opus" || path == "/stream.ogg";
    let mp3 = path == "/stream.mp3";
    if !(opus || mp3 || path == "/stream.wav") {
        return Route::Missing;
    }
    let matches_format = match format {
        Format::Opus => opus,
        Format::Mp3 => mp3,
    };
    if matches_format {
        Route::Audio
    } else {
        Route::Wrong
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_follows_the_selected_format() {
        assert!(matches!(stream_route("/", Format::Opus), Route::Audio));
        assert!(matches!(stream_route("/stream", Format::Mp3), Route::Audio));
        assert!(matches!(stream_route("/stream.opus", Format::Opus), Route::Audio));
        assert!(matches!(stream_route("/stream.ogg", Format::Opus), Route::Audio));
        assert!(matches!(stream_route("/stream.mp3", Format::Opus), Route::Wrong));
        assert!(matches!(stream_route("/stream.opus", Format::Mp3), Route::Wrong));
        assert!(matches!(stream_route("/stream.mp3", Format::Mp3), Route::Audio));
        assert!(matches!(stream_route("/stream.wav", Format::Mp3), Route::Wrong));
        assert!(matches!(stream_route("/nope", Format::Opus), Route::Missing));
    }

    #[test]
    fn mp3_with_an_empty_header_is_ready_to_send() {
        assert!(super::encoder_ready(true, 0));
        assert!(super::encoder_ready(true, 64));
        assert!(!super::encoder_ready(false, 0));
    }

    #[test]
    fn browser_navigation_is_a_page_and_players_get_audio() {
        let nav = "GET / HTTP/1.1\r\nHost: localhost\r\nAccept: text/html,application/xhtml+xml\r\nSec-Fetch-Dest: document\r\n\r\n";
        assert!(super::wants_page(nav));
        let audio = "GET /stream.mp3 HTTP/1.1\r\nAccept: */*\r\nSec-Fetch-Dest: audio\r\n\r\n";
        assert!(!super::wants_page(audio));
        let fetch = "GET /stream.mp3 HTTP/1.1\r\nAccept: */*\r\nSec-Fetch-Dest: empty\r\n\r\n";
        assert!(!super::wants_page(fetch));
        let vlc = "GET /stream.mp3 HTTP/1.1\r\nUser-Agent: VLC/3.0.20\r\nIcy-MetaData: 1\r\n\r\n";
        assert!(!super::wants_page(vlc));
    }

    #[test]
    fn player_page_points_at_the_selected_stream() {
        let mp3 = super::player_html(Format::Mp3);
        assert!(mp3.contains("\"/stream.mp3\""));
        assert!(mp3.contains("audio/mpeg"));
        let opus = super::player_html(Format::Opus);
        assert!(opus.contains("\"/stream.opus\""));
        assert!(opus.contains("audio/ogg"));
    }
}

fn stream_audio(mut stream: TcpStream, cfg: &Config, shared: &Shared, icy: bool) {
    if shared.hub.count() >= cfg.max_listeners {
        respond(&mut stream, "503 Service Unavailable", "text/plain", "too many listeners");
        return;
    }
    // Answer before the encoder is up. MP3 publishes an empty container header,
    // so waiting for non-empty bytes never returns and the browser spins.
    let metaint = if icy {
        format!("icy-metaint: {METAINT}\r\n")
    } else {
        String::new()
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nCache-Control: no-store, no-cache\r\nicy-name: Muzeeka\r\nicy-description: Muzeeka application audio\r\nicy-br: {}\r\nicy-pub: 0\r\n{metaint}Connection: close\r\n\r\n",
        cfg.format.mime(),
        cfg.bitrate
    );
    let mut out = Body {
        stream,
        metaint: if icy { METAINT } else { 0 },
        left: if icy { METAINT } else { 0 },
        sent_gen: u64::MAX,
        now: shared.now.clone(),
        buf: Vec::new(),
    };
    if out.write_raw(head.as_bytes()).is_err() || !wait_ready(shared) {
        return;
    }
    let headers = shared.hub.headers();
    // Subscribe only once a session exists, so chunks queued during the wait
    // cannot fill this listener and drop it.
    let rx = shared.hub.subscribe();
    if out.write(&headers).is_err() {
        return;
    }
    while !STOP.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                if out.write(&chunk).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

struct Body {
    stream: TcpStream,
    metaint: usize,
    left: usize,
    sent_gen: u64,
    now: Arc<crate::meta::NowPlaying>,
    buf: Vec<u8>,
}

impl Body {
    fn write_raw(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(data)
    }

    fn write(&mut self, audio: &[u8]) -> std::io::Result<()> {
        if self.metaint == 0 || audio.is_empty() {
            return self.stream.write_all(audio);
        }
        self.buf.clear();
        let now = Arc::clone(&self.now);
        let metaint = self.metaint;
        let mut left = self.left;
        let sent = &mut self.sent_gen;
        let buf = &mut self.buf;
        mux_icy(
            audio,
            &mut left,
            metaint,
            |out| {
                let (gen, title) = now.icy();
                if gen == *sent {
                    out.push(0);
                } else {
                    *sent = gen;
                    out.extend_from_slice(&icy_block(&title));
                }
            },
            buf,
        );
        self.left = left;
        self.stream.write_all(&self.buf)
    }
}

/// MP3 is ready with an empty header. Opus is ready with Ogg pages. Either one
/// means the body can start; an empty header is not "still waiting".
fn encoder_ready(ready: bool, _header_len: usize) -> bool {
    ready
}

fn wait_ready(shared: &Shared) -> bool {
    loop {
        if encoder_ready(shared.hub.ready(), shared.hub.headers().len()) {
            return true;
        }
        if STOP.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
