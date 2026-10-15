use crate::{
    error::AppError,
    services::{
        hls::{supports_direct_play, valid_segment_name},
        mpv::{self, MpvSession},
        parser::{parse_filename, ParsedEpisode},
        probe::{generate_thumbnail, probe},
        subtitles::to_webvtt,
    },
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::{
    collections::HashSet,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

pub fn router(state: Arc<AppState>) -> Router {
    let middleware_state = Arc::clone(&state);
    Router::new()
        .route("/health", get(health))
        .route("/login", get(login_page))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/", get(home))
        .route("/anime/:id", get(anime_page))
        .route("/watch/:id", get(watch_page))
        .route("/settings", get(settings_page))
        .route("/stream/:id", get(stream_episode))
        .route("/hls/:id/master.m3u8", get(hls_playlist))
        .route("/hls/:id/:segment", get(hls_segment))
        .route("/thumb/:id.jpg", get(episode_thumbnail))
        .route("/subs/:id.vtt", get(subtitle_vtt))
        .route("/api/episode/:id/subtitles", get(list_subtitles))
        .route("/api/anime", get(list_anime).post(add_anime))
        .route("/api/anime/:id/refresh-metadata", post(refresh_metadata))
        .route(
            "/api/anime/:id/metadata-lock",
            axum::routing::patch(set_metadata_lock),
        )
        .route("/api/anime/:id/episodes", post(add_episode))
        .route("/api/library/scan", post(scan_library))
        .route("/api/library/status", get(scan_status))
        .route("/api/library/events", get(scan_events))
        .route("/api/library/add-folder", post(add_folder))
        .route("/api/library/remove-folder", post(remove_folder))
        .route("/api/settings", get(get_settings).post(save_settings))
        .route("/api/mpv/play", post(play_with_mpv))
        .route("/api/progress/:id", get(get_progress).post(save_progress))
        .nest_service(
            "/static",
            tower_http::services::ServeDir::new(static_dir()).with_mime_types(|path| {
                mime_guess::from_path(path).first().map(|m| m.to_string())
            }),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            middleware_state,
            require_auth,
        ))
}

/// Static assets live next to the executable in the packaged app and in
/// `./static` during development.
fn static_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("static");
            if candidate.join("app.css").is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("static")
}

async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: axum::http::Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !state.settings.auth.enabled
        || path == "/health"
        || path == "/login"
        || path == "/api/auth/login"
        || path.starts_with("/static/")
    {
        return next.run(request).await;
    }
    let token = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .find_map(|cookie| cookie.trim().strip_prefix("anime_session="))
        })
        .map(str::to_owned);
    if let Some(token) = token {
        let expiry = state.sessions.read().await.get(&token).copied();
        if expiry.is_some_and(|expiry| expiry > std::time::Instant::now()) {
            return next.run(request).await;
        }
        if expiry.is_some() {
            state.sessions.write().await.remove(&token);
        }
    }
    if path.starts_with("/api/")
        || path.starts_with("/stream/")
        || path.starts_with("/hls/")
        || path.starts_with("/subs/")
        || path.starts_with("/thumb/")
    {
        return (StatusCode::UNAUTHORIZED, "authentication required").into_response();
    }
    Redirect::to("/login").into_response()
}

async fn login_page() -> Html<String> {
    page("Login", "<div class=\"card\" style=\"max-width:24rem;margin:6rem auto\"><h1>Masuk</h1><form id=\"login\"><label>Password<input type=\"password\" name=\"password\" required autofocus></label><button>Masuk</button></form><p id=\"message\"></p></div><script>document.querySelector('#login').addEventListener('submit',async e=>{e.preventDefault();const password=new FormData(e.target).get('password');const r=await fetch('/api/auth/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({password})});if(r.ok)location.href='/';else document.querySelector('#message').textContent='Password salah atau login gagal';});</script>")
}

#[derive(Deserialize)]
struct LoginInput {
    password: String,
}
async fn login(
    State(state): State<Arc<AppState>>,
    Json(input): Json<LoginInput>,
) -> Result<Response, AppError> {
    if !state.settings.auth.enabled
        || input.password.len() != state.settings.auth.password.len()
        || !constant_time_eq(
            input.password.as_bytes(),
            state.settings.auth.password.as_bytes(),
        )
    {
        return Err(AppError::BadRequest("invalid credentials".into()));
    }
    let token = uuid::Uuid::new_v4().to_string();
    state.sessions.write().await.insert(
        token.clone(),
        std::time::Instant::now() + std::time::Duration::from_secs(7 * 24 * 60 * 60),
    );
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "anime_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800"
        ))
        .map_err(|e| AppError::BadRequest(e.to_string()))?,
    );
    Ok(response)
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> StatusCode {
    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .find_map(|cookie| cookie.trim().strip_prefix("anime_session="))
        })
    {
        state.sessions.write().await.remove(token);
    }
    StatusCode::NO_CONTENT
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

async fn health() -> &'static str {
    "ok"
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><link rel=\"stylesheet\" href=\"/static/app.css\"></head><body>{}</body></html>",
        escape(title),
        layout(body, title)
    ))
}

const ICON_HOME: &str = "<svg width=\"18\" height=\"18\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M3 10.5 12 3l9 7.5\"/><path d=\"M5 9.5V21h14V9.5\"/></svg>";
const ICON_RESUME: &str = "<svg width=\"18\" height=\"18\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\"><circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M10 8.5v7l6-3.5z\"/></svg>";
const ICON_SETTINGS: &str = "<svg width=\"18\" height=\"18\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\"><circle cx=\"12\" cy=\"12\" r=\"3.2\"/><path d=\"M19 12a7 7 0 0 0-.15-1.4l2-1.55-2-3.46-2.4.95A7 7 0 0 0 14.05 5l-.3-2.6h-3.5L9.95 5a7 7 0 0 0-2.4 1.54l-2.4-.95-2 3.46 2 1.55A7 7 0 0 0 5 12c0 .48.05.94.15 1.4l-2 1.55 2 3.46 2.4-.95a7 7 0 0 0 2.4 1.54l.3 2.6h3.5l.3-2.6a7 7 0 0 0 2.4-1.54l2.4.95 2-3.46-2-1.55c.1-.46.15-.92.15-1.4z\"/></svg>";

fn layout(body: &str, active: &str) -> String {
    let nav = |href: &str, key: &str, icon: &str, label: &str| {
        format!(
            "<a href=\"{href}\" class=\"{}\">{icon}<span class=\"nav-label\">{label}</span></a>",
            if active == key { "active" } else { "" }
        )
    };
    format!(
        "<aside class=\"sidebar\"><div class=\"brand\">Fets<span>Anime</span></div><nav class=\"side-nav\">{}{}{}</nav><div class=\"sidebar-footer\">Lokal · hanya file milikmu</div></aside><main class=\"content\">{}</main>",
        nav("/", "Library anime", ICON_HOME, "Perpustakaan"),
        nav("/?resume=1", "Lanjutkan menonton", ICON_RESUME, "Lanjutkan"),
        nav("/settings", "Pengaturan", ICON_SETTINGS, "Pengaturan"),
        body
    )
}

#[derive(Serialize)]
struct ScanStatus {
    running: bool,
    total: i64,
    done: i64,
    current: String,
    last_result: Option<String>,
}

#[derive(Serialize)]
struct ScanEvent {
    state: &'static str,
    total: i64,
    done: i64,
    current: String,
    message: Option<String>,
}

fn current_scan_status(state: &Arc<AppState>) -> ScanStatus {
    let guard = state.scan.lock();
    ScanStatus {
        running: guard.is_some(),
        total: 0,
        done: 0,
        current: String::new(),
        last_result: None,
    }
}

async fn scan_status(State(state): State<Arc<AppState>>) -> Json<ScanStatus> {
    let guard = state.scan.lock();
    let status = match guard.as_ref() {
        Some(job) => ScanStatus {
            running: true,
            total: job.total.load(std::sync::atomic::Ordering::Relaxed),
            done: job.done.load(std::sync::atomic::Ordering::Relaxed),
            current: job.current.read().clone(),
            last_result: None,
        },
        None => {
            let last = state.last_scan.lock().clone();
            ScanStatus {
                running: false,
                total: 0,
                done: 0,
                current: String::new(),
                last_result: last,
            }
        }
    };
    Json(status)
}

/// Server-Sent Events stream of scan progress; the UI subscribes to it and
/// also polls `/api/library/status` as a fallback.
async fn scan_events(State(state): State<Arc<AppState>>) -> Response {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    state.scan_listeners.lock().push(tx);
    let body = Body::from_stream(async_stream::stream! {
        while let Some(message) = rx.recv().await {
            yield Ok::<String, std::convert::Infallible>(format!("data: {message}\n\n"));
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap()
        .into_response()
}

pub(crate) fn publish_scan_event(state: &Arc<AppState>, event: ScanEvent) {
    if let Ok(text) = serde_json::to_string(&event) {
        let mut listeners = state.scan_listeners.lock();
        listeners.retain(|tx| tx.send(text.clone()).is_ok());
    }
}

#[derive(Deserialize)]
struct FolderInput {
    path: String,
}

/// Register a library folder chosen through the native Windows dialog (or
/// drag & drop). The path is canonicalized server-side before being stored.
async fn add_folder(
    State(state): State<Arc<AppState>>,
    Json(input): Json<FolderInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let candidate = FsPath::new(input.path.trim());
    let canonical = candidate.canonicalize().map_err(|_| {
        AppError::BadRequest("folder tidak ditemukan atau sedang tidak dapat diakses".into())
    })?;
    if !canonical.is_dir() {
        return Err(AppError::BadRequest("yang dipilih bukan folder".into()));
    }
    let text = canonical.to_string_lossy().to_string();
    let mut settings = state.settings.write();
    if !settings.library.paths.iter().any(|p| p == &text) {
        settings.library.paths.push(text);
        settings.save().map_err(|e| AppError::Internal(e.to_string()))?;
    }
    drop(settings);
    let paths = state.settings.read().library.paths.clone();
    spawn_scan(Arc::clone(&state));
    Ok(Json(serde_json::json!({ "paths": paths })))
}

#[derive(Deserialize)]
struct RemoveFolderInput {
    path: String,
}

async fn remove_folder(
    State(state): State<Arc<AppState>>,
    Json(input): Json<RemoveFolderInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut settings = state.settings.write();
    settings.library.paths.retain(|p| p != &input.path);
    settings.save().map_err(|e| AppError::Internal(e.to_string()))?;
    let paths = settings.library.paths.clone();
    drop(settings);
    spawn_scan(Arc::clone(&state));
    Ok(Json(serde_json::json!({ "paths": paths })))
}

#[derive(Serialize)]
struct PublicSettings {
    player: String,
    mpv_path: String,
    mpv_found: Option<String>,
    theme: String,
    watch: bool,
    anilist_enabled: bool,
    ffmpeg_path: String,
    ffprobe_path: String,
    library_paths: Vec<String>,
    data_dir: String,
}

#[derive(Deserialize)]
struct SettingsPatch {
    player: Option<String>,
    mpv_path: Option<String>,
    theme: Option<String>,
    watch: Option<bool>,
    anilist_enabled: Option<bool>,
    ffmpeg_path: Option<String>,
    ffprobe_path: Option<String>,
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<PublicSettings> {
    let settings = state.settings.read();
    let mpv_found = mpv::find_mpv(Some(&settings.desktop.mpv_path))
        .map(|p| crate::desktop::display_path(&p));
    Json(PublicSettings {
        player: settings.desktop.player.clone(),
        mpv_path: settings.desktop.mpv_path.clone(),
        mpv_found,
        theme: settings.desktop.theme.clone(),
        watch: settings.library.watch,
        anilist_enabled: settings.metadata.anilist_enabled,
        ffmpeg_path: settings.ffmpeg.ffmpeg_path.clone(),
        ffprobe_path: settings.ffmpeg.ffprobe_path.clone(),
        library_paths: settings.library.paths.clone(),
        data_dir: settings.data.dir.clone(),
    })
}

async fn save_settings(
    State(state): State<Arc<AppState>>,
    Json(patch): Json<SettingsPatch>,
) -> Result<StatusCode, AppError> {
    let mut settings = state.settings.write();
    if let Some(player) = patch.player {
        if !matches!(player.as_str(), "auto" | "html5" | "mpv") {
            return Err(AppError::BadRequest("pemutar tidak valid".into()));
        }
        settings.desktop.player = player;
    }
    if let Some(mpv_path) = patch.mpv_path {
        settings.desktop.mpv_path = mpv_path;
    }
    if let Some(theme) = patch.theme {
        if !matches!(theme.as_str(), "dark" | "light" | "system") {
            return Err(AppError::BadRequest("tema tidak valid".into()));
        }
        settings.desktop.theme = theme;
    }
    if let Some(watch) = patch.watch {
        settings.library.watch = watch;
    }
    if let Some(anilist) = patch.anilist_enabled {
        settings.metadata.anilist_enabled = anilist;
    }
    if let Some(path) = patch.ffmpeg_path {
        settings.ffmpeg.ffmpeg_path = path;
    }
    if let Some(path) = patch.ffprobe_path {
        settings.ffmpeg.ffprobe_path = path;
    }
    settings.save().map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MpvPlayInput {
    episode_id: i64,
    position_sec: f64,
    reason: Option<String>,
}

/// Launch mpv for one episode. The file path always comes from the database
/// (server-validated), never from the client. Progress flows back over the
/// named pipe so "lanjutkan menonton" keeps working.
async fn play_with_mpv(
    State(state): State<Arc<AppState>>,
    Json(input): Json<MpvPlayInput>,
) -> Result<StatusCode, AppError> {
    tracing::info!(episode = input.episode_id, reason = ?input.reason, "switching to mpv");
    let mpv_path = mpv::find_mpv(Some(&state.settings.read().desktop.mpv_path)).ok_or_else(|| {
        AppError::BadRequest(
            "mpv.exe tidak ditemukan. Pasang mpv atau tentukan jalurnya di Pengaturan."
                .to_string(),
        )
    })?;
    let row = sqlx::query(
        "SELECT e.file_path FROM episode e WHERE e.id=? AND e.removed_at IS NULL",
    )
    .bind(input.episode_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("episode tidak ditemukan".into()))?;
    let stored: String = row.get("file_path");
    let validated = validate_media_path(&state, &stored)?;
    let id = uuid::Uuid::new_v4().to_string();
    let pipe = mpv::ipc_pipe(&id);
    let args = mpv::build_args(&pipe, input.position_sec, &validated);
    let session = MpvSession::spawn(&mpv_path, &id, &args)
        .map_err(AppError::BadRequest)?;
    let child = session.into_child();
    // Keep the session id alive in state so shutdown can reap children.
    let watcher_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _guard = SessionGuard {
            id: id.clone(),
            state: Arc::clone(&watcher_state),
        };
        #[cfg(windows)]
        {
            use windows_named_pipe_watcher::*;
            let _ = Unreachable;
        }
        watch_mpv_progress(watcher_state, input.episode_id, pipe, child).await;
    });
    Ok(StatusCode::ACCEPTED)
}

struct SessionGuard {
    id: String,
    state: Arc<AppState>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.state.mpv_sessions.lock().remove(&self.id);
    }
}

/// Connect to the mpv IPC pipe, subscribe to time-pos/eof-reached, mirror
/// progress into the database, and make sure the process is reaped when mpv
/// exits or the app shuts down.
async fn watch_mpv_progress(
    state: Arc<AppState>,
    episode_id: i64,
    pipe: String,
    child: tokio::process::Child,
) {
    #[cfg(windows)]
    {
        let pipe_clone = pipe.clone();
        let handle = tokio::task::spawn_blocking(move || {
            crate::services::mpv::pipe_io::PipeHandle::open(&pipe_clone, 50)
        })
        .await;
        match handle {
            Ok(Ok(handle)) => {
                let handle = Arc::new(handle);
                state.mpv_sessions.lock().insert(
                    pipe.rsplit('-').next().unwrap_or_default().to_string(),
                    Arc::clone(&handle),
                );
                // Subscribe to the properties we need.
                let _ = handle.write_line(r#"{"command":["observe_property",1,"time-pos"]}"#);
                let _ = handle.write_line(r#"{"command":["observe_property",2,"eof-reached"]}"#);
                let pool = state.pool.clone();
                loop {
                    let handle_clone = Arc::clone(&handle);
                    let line = tokio::task::spawn_blocking(move || handle_clone.read_line())
                        .await;
                    let Ok(Ok(line)) = line else { break };
                    if line.is_empty() {
                        break; // mpv closed the window => save final state below
                    }
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                        if let Some((position, watched)) = mpv::progress_from_event(&event) {
                            let _ = update_mpv_progress(&pool, episode_id, position, watched).await;
                        }
                    }
                }
                // Final flush: ask mpv for its last position before reaping.
                let _ = handle.write_line(r#"{"command":["get_property","time-pos"]}"#);
            }
            Ok(Err(error)) => tracing::warn!(%error, "could not attach to mpv IPC pipe"),
            Err(_) => {}
        }
    }
    let _ = pipe;
    let _ = child.wait().await;
}

async fn update_mpv_progress(
    pool: &sqlx::SqlitePool,
    episode_id: i64,
    position: f64,
    watched: bool,
) -> Result<(), AppError> {
    let duration: Option<f64> = sqlx::query_scalar(
        "SELECT COALESCE(json_extract(streams_json,'$[0].duration'), NULL) FROM episode WHERE id=?",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await?;
    let fraction = duration.filter(|d| *d > 0.0).map(|d| (position / d).min(1.0));
    if watched {
        sqlx::query(
            "INSERT INTO playback(episode_id,position_sec,duration_sec,fraction,watched,updated_at) VALUES(?,?,?,?,1,unixepoch()) \
             ON CONFLICT(episode_id) DO UPDATE SET watched=1, updated_at=unixepoch()",
        )
        .bind(episode_id)
        .bind(position)
        .bind(duration)
        .bind(fraction)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO playback(episode_id,position_sec,duration_sec,fraction,watched,updated_at) VALUES(?,?,?,?,0,unixepoch()) \
             ON CONFLICT(episode_id) DO UPDATE SET position_sec=excluded.position_sec, duration_sec=excluded.duration_sec, fraction=excluded.fraction, watched=MAX(playback.watched, excluded.watched), updated_at=unixepoch()",
        )
        .bind(episode_id)
        .bind(position)
        .bind(duration)
        .bind(fraction)
        .execute(pool)
        .await?;
    }
    Ok(())
}

mod windows_named_pipe_watcher {
    #[allow(dead_code)]
    pub struct Unreachable;
}
