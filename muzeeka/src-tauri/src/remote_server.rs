// Local HTTP server for phone/browser remote control.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::discord_rpc::DiscordPresence;
use crate::player::Player;
use crate::remote_control::RemoteController;

const REMOTE_PORT: u16 = 8765;
const REMOTE_UI: &str = include_str!("remote/index.html");

#[derive(Clone)]
struct AppState {
    controller: Arc<RemoteController>,
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

pub fn start(player: Player, discord: DiscordPresence, app: AppHandle) {
    let controller = Arc::new(RemoteController::new(player, discord, app));

    std::thread::Builder::new()
        .name("muzeeka-remote".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("remote server tokio runtime");

            rt.block_on(async {
                if let Err(error) = run_server(controller).await {
                    eprintln!("Remote control server failed: {error}");
                }
            });
        })
        .expect("spawn remote server thread");
}

async fn run_server(controller: Arc<RemoteController>) -> Result<(), String> {
    let state = AppState { controller };

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

    let addr = SocketAddr::from(([0, 0, 0, 0], REMOTE_PORT));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind remote server on port {REMOTE_PORT}: {e}"))?;

    eprintln!(
        "Remote control: http://localhost:{REMOTE_PORT} (LAN: http://<your-ip>:{REMOTE_PORT})"
    );

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Remote server error: {e}"))
}

async fn index() -> Html<&'static str> {
    Html(REMOTE_UI)
}

async fn api_info() -> Json<InfoResponse> {
    Json(InfoResponse {
        port: REMOTE_PORT,
        urls: local_urls(REMOTE_PORT),
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

async fn api_play(State(state): State<AppState>, Json(body): Json<PlayBody>) -> Result<Json<OkResponse>, AppError> {
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

async fn api_seek(State(state): State<AppState>, Json(body): Json<SeekBody>) -> Result<Json<OkResponse>, AppError> {
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

fn local_urls(port: u16) -> Vec<String> {
    vec![format!("http://localhost:{port}")]
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