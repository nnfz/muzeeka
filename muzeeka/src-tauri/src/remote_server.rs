// Local HTTP server for phone/browser remote control.
// Can be enabled/disabled and rebound to a different port from settings.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use if_addrs::IfAddr;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::remote_control::RemoteController;

pub const DEFAULT_REMOTE_PORT: u16 = 8765;
const REMOTE_UI: &str = include_str!("remote/index.html");

#[derive(Debug, Clone, Serialize)]
pub struct RemoteStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    /// Best-guess LAN address for phone/remote on the same network.
    pub local_ip: Option<String>,
    /// Other usable IPv4 addresses (VPN/virtual/etc), ranked after `local_ip`.
    pub local_ips: Vec<String>,
    pub urls: Vec<String>,
    pub last_error: Option<String>,
}

struct Inner {
    enabled: bool,
    port: u16,
    running: bool,
    last_error: Option<String>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

/// Controllable remote HTTP server (managed Tauri state).
pub struct RemoteServer {
    controller: Arc<RemoteController>,
    inner: Mutex<Inner>,
}

impl RemoteServer {
    pub fn new(controller: Arc<RemoteController>, enabled: bool, port: u16) -> Arc<Self> {
        let port = sanitize_port(port);
        let server = Arc::new(Self {
            controller,
            inner: Mutex::new(Inner {
                enabled,
                port,
                running: false,
                last_error: None,
                shutdown_tx: None,
            }),
        });
        if enabled {
            server.spawn(port);
        }
        server
    }

    pub fn status(&self) -> RemoteStatus {
        let g = self.inner.lock();
        build_status(&g)
    }

    /// Apply config from settings. Restarts the server when needed.
    pub fn apply(self: &Arc<Self>, enabled: bool, port: u16) -> RemoteStatus {
        let port = sanitize_port(port);

        let (should_stop, should_start) = {
            let mut g = self.inner.lock();
            let same = g.enabled == enabled && g.port == port;
            let failed = enabled && !g.running && g.last_error.is_some();

            if same && !failed {
                return build_status(&g);
            }

            let was_active = g.running || g.shutdown_tx.is_some();
            g.enabled = enabled;
            g.port = port;
            g.last_error = None;
            (was_active, enabled)
        };

        if should_stop {
            self.stop();
            // Give the OS a moment to release the socket on Windows.
            std::thread::sleep(Duration::from_millis(80));
        }

        if should_start {
            self.spawn(port);
            // Wait briefly for bind result so UI shows accurate running/error state.
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(25));
                let g = self.inner.lock();
                if g.running || g.last_error.is_some() {
                    break;
                }
            }
        }

        self.status()
    }

    fn stop(&self) {
        let tx = {
            let mut g = self.inner.lock();
            g.running = false;
            g.shutdown_tx.take()
        };
        if let Some(tx) = tx {
            let _ = tx.send(());
        }
    }

    fn spawn(self: &Arc<Self>, port: u16) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        {
            let mut g = self.inner.lock();
            g.port = port;
            g.shutdown_tx = Some(shutdown_tx);
            g.running = false;
            g.last_error = None;
        }

        let server = Arc::clone(self);
        let controller = Arc::clone(&self.controller);

        std::thread::Builder::new()
            .name("muzeeka-remote".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let mut g = server.inner.lock();
                        g.running = false;
                        g.shutdown_tx = None;
                        g.last_error = Some(format!("Failed to start remote runtime: {e}"));
                        return;
                    }
                };

                rt.block_on(async {
                    match run_server(controller, port, shutdown_rx, &server).await {
                        Ok(()) => {
                            let mut g = server.inner.lock();
                            g.running = false;
                            g.shutdown_tx = None;
                        }
                        Err(error) => {
                            let mut g = server.inner.lock();
                            g.running = false;
                            g.shutdown_tx = None;
                            g.last_error = Some(error);
                        }
                    }
                });
            })
            .expect("spawn remote server thread");
    }
}

fn build_status(g: &Inner) -> RemoteStatus {
    let port = g.port;
    let ips = lan_ipv4_candidates();
    let local_ip = ips.first().cloned();
    let local_ips = if ips.len() > 1 {
        ips[1..].to_vec()
    } else {
        Vec::new()
    };
    let urls = if g.running {
        public_urls(port, &ips)
    } else {
        Vec::new()
    };
    RemoteStatus {
        enabled: g.enabled,
        running: g.running,
        port,
        local_ip,
        local_ips,
        urls,
        last_error: g.last_error.clone(),
    }
}

async fn run_server(
    controller: Arc<RemoteController>,
    port: u16,
    shutdown_rx: oneshot::Receiver<()>,
    server: &RemoteServer,
) -> Result<(), String> {
    let state = AppState { controller, port };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/info", get(api_info))
        .route("/api/state", get(api_state))
        .route("/api/playlists", get(api_playlists))
        .route("/api/playlist", get(api_playlist))
        .route("/api/play", post(api_play))
        .route("/api/toggle", post(api_toggle))
        .route("/api/pause", post(api_pause))
        .route("/api/resume", post(api_resume))
        .route("/api/next", post(api_next))
        .route("/api/prev", post(api_prev))
        .route("/api/seek", post(api_seek))
        .route("/api/volume", post(api_volume))
        .route("/api/playlist/select", post(api_select_playlist))
        .route("/api/shuffle/toggle", post(api_toggle_shuffle))
        .route("/api/repeat/toggle", post(api_toggle_repeat))
        .route("/api/cover", get(api_cover))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind remote server on port {port}: {e}"))?;

    {
        let mut g = server.inner.lock();
        g.running = true;
        g.last_error = None;
    }

    eprintln!("Remote control: http://localhost:{port} (LAN: http://<your-ip>:{port})");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|e| format!("Remote server error: {e}"))
}

#[derive(Clone)]
struct AppState {
    controller: Arc<RemoteController>,
    port: u16,
}

#[derive(Debug, Deserialize)]
struct PlayBody {
    path: String,
    #[serde(default)]
    playlist_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SeekBody {
    position: f64,
}

#[derive(Debug, Deserialize)]
struct VolumeBody {
    volume: f32,
}

#[derive(Debug, Deserialize)]
struct PlaylistBody {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CoverQuery {
    path: String,
}

#[derive(Debug, Serialize)]
struct InfoResponse {
    port: u16,
    urls: Vec<String>,
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct ToggleShuffleResponse {
    shuffle_enabled: bool,
}

#[derive(Debug, Serialize)]
struct ToggleRepeatResponse {
    repeat_mode: String,
}

async fn index() -> Html<&'static str> {
    Html(REMOTE_UI)
}

async fn api_info(State(state): State<AppState>) -> Json<InfoResponse> {
    Json(InfoResponse {
        port: state.port,
        urls: public_urls(state.port, &lan_ipv4_candidates()),
    })
}

fn json_value<T: Serialize>(value: T) -> Result<Json<serde_json::Value>, AppError> {
    serde_json::to_value(value)
        .map(Json)
        .map_err(|error| AppError(format!("Failed to serialize response: {error}")))
}

async fn api_state(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    json_value(state.controller.get_state()?)
}

async fn api_playlists(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    json_value(state.controller.get_playlists()?)
}

#[derive(Debug, Deserialize)]
struct PlaylistQuery {
    id: String,
}

async fn api_playlist(
    State(state): State<AppState>,
    Query(query): Query<PlaylistQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    json_value(state.controller.get_playlist_view(&query.id)?)
}

async fn api_play(
    State(state): State<AppState>,
    Json(body): Json<PlayBody>,
) -> Result<Json<OkResponse>, AppError> {
    state
        .controller
        .play(&body.path, body.playlist_id.as_deref())?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_toggle(State(state): State<AppState>) -> Result<Json<OkResponse>, AppError> {
    state.controller.toggle()?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_pause(State(state): State<AppState>) -> Result<Json<OkResponse>, AppError> {
    state.controller.pause()?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_resume(State(state): State<AppState>) -> Result<Json<OkResponse>, AppError> {
    state.controller.resume()?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_next(State(state): State<AppState>) -> Result<Json<OkResponse>, AppError> {
    state.controller.next()?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_prev(State(state): State<AppState>) -> Result<Json<OkResponse>, AppError> {
    state.controller.prev()?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_seek(
    State(state): State<AppState>,
    Json(body): Json<SeekBody>,
) -> Result<Json<OkResponse>, AppError> {
    state.controller.seek(body.position)?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_volume(
    State(state): State<AppState>,
    Json(body): Json<VolumeBody>,
) -> Result<Json<OkResponse>, AppError> {
    state.controller.set_volume(body.volume)?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_select_playlist(
    State(state): State<AppState>,
    Json(body): Json<PlaylistBody>,
) -> Result<Json<OkResponse>, AppError> {
    state.controller.select_playlist(&body.id)?;
    Ok(Json(OkResponse { ok: true }))
}

async fn api_toggle_shuffle(
    State(state): State<AppState>,
) -> Result<Json<ToggleShuffleResponse>, AppError> {
    let enabled = state.controller.toggle_shuffle()?;
    Ok(Json(ToggleShuffleResponse {
        shuffle_enabled: enabled,
    }))
}

async fn api_toggle_repeat(
    State(state): State<AppState>,
) -> Result<Json<ToggleRepeatResponse>, AppError> {
    let mode = state.controller.toggle_repeat()?;
    Ok(Json(ToggleRepeatResponse { repeat_mode: mode }))
}

async fn api_cover(
    State(state): State<AppState>,
    Query(query): Query<CoverQuery>,
) -> Result<Response, AppError> {
    match state.controller.cover_bytes(&query.path)? {
        Some((bytes, mime)) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("image/jpeg")),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            );
            Ok((StatusCode::OK, headers, bytes).into_response())
        }
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

pub fn sanitize_port(port: u16) -> u16 {
    if port < 1024 {
        DEFAULT_REMOTE_PORT
    } else {
        port
    }
}

/// Ranked IPv4 candidates for LAN remote control (best first).
///
/// Avoids the classic "UDP to 8.8.8.8" trap that picks Clash/Mihomo fake-IP
/// (198.18.x), VPN tunnels, or VMware host-only adapters instead of Ethernet/Wi‑Fi.
fn lan_ipv4_candidates() -> Vec<String> {
    let mut scored: Vec<(i32, String)> = Vec::new();

    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            let IfAddr::V4(v4) = iface.addr else {
                continue;
            };
            let ip = v4.ip;
            if !is_usable_lan_v4(ip) {
                continue;
            }
            let score = score_lan_v4(ip, &iface.name);
            if score <= -100 {
                continue;
            }
            let s = ip.to_string();
            if scored.iter().any(|(_, existing)| existing == &s) {
                continue;
            }
            scored.push((score, s));
        }
    }

    // Mild hint from default-route interface, but never trust it alone if it's fake-IP/VPN.
    if let Some(ip) = udp_outbound_v4() {
        if is_usable_lan_v4(ip) {
            let s = ip.to_string();
            if let Some((score, _)) = scored.iter_mut().find(|(_, existing)| existing == &s) {
                *score += 15;
            } else {
                let score = score_lan_v4(ip, "") + 10;
                if score > -100 {
                    scored.push((score, s));
                }
            }
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, ip)| ip).collect()
}

fn is_usable_lan_v4(ip: Ipv4Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() {
        return false;
    }
    // 169.254.0.0/16 link-local — useless for phone remote.
    if ip.is_link_local() {
        return false;
    }
    true
}

fn score_lan_v4(ip: Ipv4Addr, iface_name: &str) -> i32 {
    let o = ip.octets();
    let mut score: i32 = 0;
    let name = iface_name.to_ascii_lowercase();

    // Clash/Mihomo fake-IP / benchmark range — never prefer.
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return -250;
    }

    // RFC1918 private networks (what phones use on home Wi‑Fi).
    if o[0] == 10 {
        score += 120;
    } else if o[0] == 172 && (16..=31).contains(&o[1]) {
        score += 110;
    } else if o[0] == 192 && o[1] == 168 {
        score += 130;
    } else if o[0] == 100 && (64..=127).contains(&o[1]) {
        // CGNAT / Tailscale-ish — ok as fallback.
        score += 40;
    } else {
        // Public or exotic — rarely what you want for "open on phone".
        score -= 40;
    }

    // Radmin-style 26.x and other odd tunnels.
    if o[0] == 26 {
        score -= 60;
    }

    // Adapter name heuristics (Windows + common virtuals).
    const PENALIZE: &[&str] = &[
        "vmware",
        "vbox",
        "virtualbox",
        "hyper-v",
        "vethernet",
        "wsl",
        "docker",
        "radmin",
        "hamachi",
        "zerotier",
        "outline",
        "tap-windows",
        "tap0",
        "tun",
        "mihomo",
        "clash",
        "meta",
        "loopback",
        "bluetooth",
        "isatap",
        "teredo",
    ];
    for p in PENALIZE {
        if name.contains(p) {
            score -= 90;
            break;
        }
    }

    const PREFER: &[&str] = &[
        "ethernet",
        "wi-fi",
        "wifi",
        "wlan",
        "local area connection",
        "беспровод", // Wireless (RU Windows)
        "ethernet",
        "eth",
        "en0",
        "en1",
    ];
    for p in PREFER {
        if name.contains(p) {
            score += 35;
            break;
        }
    }

    // Host-only adapters often claim *.1 without being the PC's LAN address.
    if o[3] == 1 && (name.contains("vmware") || name.contains("virtual") || name.contains("vbox"))
    {
        score -= 40;
    }

    score
}

fn udp_outbound_v4() -> Option<Ipv4Addr> {
    // Does not send packets; OS selects the interface for that route.
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

fn public_urls(port: u16, ips: &[String]) -> Vec<String> {
    let mut urls = vec![format!("http://localhost:{port}")];
    for ip in ips {
        urls.push(format!("http://{ip}:{port}"));
    }
    urls
}

struct AppError(String);

impl From<String> for AppError {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.0).into_response()
    }
}
