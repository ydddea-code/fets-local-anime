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
    extract::{DefaultBodyLimit, Multipart, Path, State},
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
        .route("/add", get(add_page))
        .route("/anime/:id", get(anime_page))
        .route("/watch/:id", get(watch_page))
        .route("/settings", get(settings_page))
        .route("/stream/:id", get(stream_episode))
        .route("/hls/:id/master.m3u8", get(hls_playlist))
        .route("/hls/:id/:segment", get(hls_segment))
        .route("/thumb/:id.jpg", get(episode_thumbnail))
        .route("/cover/:id", get(anime_cover))
        .route("/subs/:id.vtt", get(subtitle_vtt))
        .route("/api/episode/:id/subtitles", get(list_subtitles))
        .route("/api/anime", get(list_anime).post(add_anime))
        .route("/api/anime/:id/refresh-metadata", post(refresh_metadata))
        .route(
            "/api/anime/:id/metadata-lock",
            axum::routing::patch(set_metadata_lock),
        )
        .route("/api/anime/:id/episodes", post(add_episode))
        .route(
            "/api/anime/:id/cover",
            post(upload_anime_cover).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/api/library/scan", post(scan_library))
        .route("/api/library/status", get(scan_status))
        .route("/api/library/events", get(scan_events))
        .route("/api/library/add-folder", post(add_library_folder))
        .route("/api/library/remove-folder", post(remove_library_folder))
        .route("/api/settings", get(get_settings).post(save_settings))
        .route("/api/mpv/play", post(play_with_mpv))
        .route("/api/progress/:id", get(get_progress).post(save_progress))
        .nest_service("/static", tower_http::services::ServeDir::new(static_dir()))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            middleware_state,
            require_auth,
        ))
}

fn static_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("FETS_STATIC_DIR").map(PathBuf::from) {
        if path.join("app.css").is_file() {
            return path;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join("static"), dir.join("resources").join("static")] {
                if candidate.join("app.css").is_file() {
                    return candidate;
                }
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
    if !state.settings_snapshot().auth.enabled
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
    page("Login", "<h1>Masuk</h1><form id=\"login\"><label>Password<input type=\"password\" name=\"password\" required autofocus></label><button>Masuk</button></form><p id=\"message\"></p><script>document.querySelector('#login').addEventListener('submit',async e=>{e.preventDefault();const password=new FormData(e.target).get('password');const r=await fetch('/api/auth/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({password})});if(r.ok)location.href='/';else document.querySelector('#message').textContent='Password salah atau login gagal';});</script>")
}

#[derive(Deserialize)]
struct LoginInput {
    password: String,
}
async fn login(
    State(state): State<Arc<AppState>>,
    Json(input): Json<LoginInput>,
) -> Result<Response, AppError> {
    if !state.settings_snapshot().auth.enabled
        || input.password.len() != state.settings_snapshot().auth.password.len()
        || !constant_time_eq(
            input.password.as_bytes(),
            state.settings_snapshot().auth.password.as_bytes(),
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
    Html(format!("<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"theme-color\" content=\"#101014\"><title>{}</title><link rel=\"stylesheet\" href=\"/static/app.css\"></head><body><aside class=\"sidebar\"><a class=\"brand\" href=\"/\">Fets <span>Anime</span></a><nav class=\"side-nav\"><a href=\"/\">▦ <span class=\"nav-label\">Perpustakaan</span></a><a href=\"/#lanjutkan\">▶ <span class=\"nav-label\">Lanjutkan</span></a><a href=\"/settings\">⚙ <span class=\"nav-label\">Pengaturan</span></a></nav><div class=\"sidebar-footer\">Koleksi lokalmu</div></aside><main class=\"content\">{}</main><script src=\"/static/settings.js\" defer></script></body></html>", escape(title), body))
}

async fn home(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query("SELECT a.id, a.title, a.cover_path, (SELECT e.id FROM episode e WHERE e.anime_id=a.id AND e.thumb_path IS NOT NULL ORDER BY e.season,e.number LIMIT 1) AS thumb_id FROM anime a ORDER BY a.title")
        .fetch_all(&state.pool)
        .await?;
    let mut cards = String::new();
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let title: String = row.try_get("title")?;
        let cover_path: Option<String> = row.try_get("cover_path")?;
        let thumb_id: Option<i64> = row.try_get("thumb_id")?;
        let art = if cover_path.is_some() {
            format!(
                "<img class=\"poster-art\" src=\"/cover/{id}\" alt=\"Cover {}\" loading=\"lazy\">",
                escape(&title)
            )
        } else {
            thumb_id.map(|episode_id| format!("<img class=\"poster-art\" src=\"/thumb/{episode_id}.jpg\" alt=\"\" loading=\"lazy\">" )).unwrap_or_else(|| "<div class=\"poster-placeholder\" aria-hidden=\"true\">▦</div>".to_string())
        };
        cards.push_str(&format!(
            "<article><a class=\"anime-tile\" href=\"/anime/{id}\">{art}<span>{}</span></a></article>",
            escape(&title)
        ));
    }
    let resume = sqlx::query("SELECT e.id, a.title, e.season, e.number, p.position_sec FROM progress p JOIN episode e ON e.id=p.episode_id JOIN anime a ON a.id=e.anime_id WHERE p.watched=0 ORDER BY p.updated_at DESC LIMIT 1")
        .fetch_optional(&state.pool)
        .await?;
    let resume_html = if let Some(row) = resume {
        let id: i64 = row.try_get("id")?;
        let title: String = row.try_get("title")?;
        let season: i64 = row.try_get("season")?;
        let number: f64 = row.try_get("number")?;
        let position: f64 = row.try_get("position_sec")?;
        format!("<section id=\"lanjutkan\" class=\"resume-card\"><div><p class=\"eyebrow\">LANJUTKAN MENONTON</p><h2>{}</h2><p class=\"muted\">Musim {season} · Episode {number} · {:.0} detik</p></div><a class=\"btn\" href=\"/watch/{id}\">Lanjutkan <span aria-hidden=\"true\">→</span></a></section>", escape(&title), position)
    } else {
        String::new()
    };
    let empty = if cards.is_empty() {
        "<section class=\"empty-state\"><div class=\"empty-icon\">▦</div><h1>Perpustakaanmu masih kosong</h1><p>Tambahkan folder anime untuk mulai membangun koleksi dan melanjutkan tontonanmu.</p><button id=\"add-folder\" class=\"big\" type=\"button\">Tambah folder anime</button></section>"
    } else {
        ""
    };
    Ok(page("Perpustakaan", &format!("<header class=\"page-heading\"><div><p class=\"eyebrow\">KOLEKSI PRIBADI</p><h1>Perpustakaan</h1></div><a class=\"btn\" href=\"/settings\">＋ Tambah Folder</a></header>{resume_html}<section><h2>Semua anime</h2>{empty}<div class=\"poster-grid\">{cards}</div></section>")))
}

async fn anime_page(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let anime = sqlx::query(
        "SELECT title,synopsis,poster_path,cover_path,metadata_locked FROM anime WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    let title: String = anime.try_get("title")?;
    let synopsis: Option<String> = anime.try_get("synopsis")?;
    let poster_url: Option<String> = anime.try_get("poster_path")?;
    let cover_path: Option<String> = anime.try_get("cover_path")?;
    let metadata_locked: bool = anime.try_get("metadata_locked")?;
    let episodes = sqlx::query(
        "SELECT id, season, number, title, thumb_path FROM episode WHERE anime_id = ? ORDER BY season, number",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let mut list = String::new();
    for ep in episodes {
        let eid: i64 = ep.try_get("id")?;
        let season: i64 = ep.try_get("season")?;
        let number: f64 = ep.try_get("number")?;
        let label: Option<String> = ep.try_get("title")?;
        let thumbnail: Option<String> = ep.try_get("thumb_path")?;
        let image = thumbnail
            .map(|_| format!("<img class=\"episode-thumb\" src=\"/thumb/{eid}.jpg\" alt=\"\"> "))
            .unwrap_or_default();
        list.push_str(&format!(
            "<li>{image}<div class=\"ep-info\"><a href=\"/watch/{eid}\">S{season:02} E{number} {}</a></div><a class=\"episode-play\" href=\"/watch/{eid}\" aria-label=\"Putar episode\">▶</a></li>",
            escape(label.as_deref().unwrap_or(""))
        ));
    }
    let manual = format!("<h2>Tambah episode dari file library</h2><form id=\"episode\"><label>Path file<input name=\"file_path\" required></label><label>Season<input type=\"number\" name=\"season\" min=1></label><label>Nomor episode<input type=\"number\" name=\"number\" step=\"0.1\"></label><button>Tambah episode</button></form><p id=\"message\"></p><script>document.querySelector('#episode').addEventListener('submit',async e=>{{e.preventDefault();const f=new FormData(e.target);const body={{file_path:f.get('file_path'),season:f.get('season')?Number(f.get('season')):null,number:f.get('number')?Number(f.get('number')):null}};const r=await fetch('/api/anime/{id}/episodes',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify(body)}});document.querySelector('#message').textContent=r.ok?'Episode ditambahkan':await r.text();if(r.ok)location.reload();}});</script>");
    let poster = if cover_path.is_some() {
        format!(
            "<img class=\"poster\" src=\"/cover/{id}\" alt=\"Cover {}\">",
            escape(&title)
        )
    } else {
        poster_url
            .filter(|url| url.starts_with("https://"))
            .map(|url| {
                format!(
                    "<img class=\"poster\" src=\"{}\" alt=\"Poster {}\">",
                    escape(&url),
                    escape(&title)
                )
            })
            .unwrap_or_else(|| {
                "<div class=\"poster poster-placeholder\" aria-hidden=\"true\">▦</div>".into()
            })
    };
    let cover_form = format!("<form class=\"cover-upload\" id=\"cover-form\"><label>Cover anime<input name=\"cover\" type=\"file\" accept=\"image/jpeg,image/png,image/webp\" required></label><button class=\"secondary\" type=\"submit\">Unggah cover</button><p id=\"cover-message\" role=\"status\"></p></form><script>document.querySelector('#cover-form').addEventListener('submit',async e=>{{e.preventDefault();const r=await fetch('/api/anime/{id}/cover',{{method:'POST',body:new FormData(e.target)}});document.querySelector('#cover-message').textContent=r.ok?'Cover diperbarui':await r.text();if(r.ok)location.reload();}});</script>");
    let synopsis = synopsis
        .map(|text| format!("<p>{}</p>", escape(&text)))
        .unwrap_or_default();
    let metadata_ui = if state.settings_snapshot().metadata.anilist_enabled {
        format!("<section><label><input id=\"metadata-lock\" type=\"checkbox\" {}> Kunci metadata hasil edit manual</label><button id=\"refresh-metadata\">Perbarui dari AniList</button><p id=\"metadata-message\"></p></section><script>const lock=document.querySelector('#metadata-lock');lock.addEventListener('change',async()=>{{const r=await fetch('/api/anime/{id}/metadata-lock',{{method:'PATCH',headers:{{'content-type':'application/json'}},body:JSON.stringify({{locked:lock.checked}})}});if(!r.ok)document.querySelector('#metadata-message').textContent=await r.text();}});document.querySelector('#refresh-metadata').addEventListener('click',async()=>{{const r=await fetch('/api/anime/{id}/refresh-metadata',{{method:'POST'}});document.querySelector('#metadata-message').textContent=r.ok?'Metadata diperbarui':await r.text();if(r.ok)location.reload();}});</script>", if metadata_locked {"checked"} else {""})
    } else {
        String::new()
    };
    Ok(page(
        &title,
        &format!(
            "<a class=\"back-link\" href=\"/\">← Perpustakaan</a><section class=\"anime-header\">{poster}<div><p class=\"eyebrow\">ANIME</p><h1>{}</h1>{synopsis}{cover_form}{metadata_ui}</div></section><section class=\"episode-section\"><h2>Episode</h2><ul class=\"episodes\">{list}</ul></section><section class=\"card\">{manual}</section>",
            escape(&title)
        ),
    ))
}

const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;

async fn upload_anime_cover(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    mut multipart: Multipart,
) -> Result<StatusCode, AppError> {
    let previous: Option<String> = sqlx::query_scalar("SELECT cover_path FROM anime WHERE id=?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let field = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("file cover tidak valid".into()))?
        .ok_or_else(|| AppError::BadRequest("pilih file cover terlebih dahulu".into()))?;
    if field.name() != Some("cover") {
        return Err(AppError::BadRequest(
            "field upload harus bernama cover".into(),
        ));
    }
    let file_name = field.file_name().unwrap_or_default().to_ascii_lowercase();
    let extension = match FsPath::new(&file_name)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        Some("jpg" | "jpeg") => "jpg",
        Some("png") => "png",
        Some("webp") => "webp",
        _ => {
            return Err(AppError::BadRequest(
                "format cover harus JPG, PNG, atau WebP".into(),
            ))
        }
    };
    let bytes = field
        .bytes()
        .await
        .map_err(|_| AppError::BadRequest("file cover tidak dapat dibaca".into()))?;
    if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES {
        return Err(AppError::BadRequest(
            "ukuran cover harus di bawah 8 MB".into(),
        ));
    }
    let valid_image = match extension {
        "jpg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if !valid_image {
        return Err(AppError::BadRequest(
            "isi file bukan gambar JPG, PNG, atau WebP yang valid".into(),
        ));
    }
    let covers_dir = PathBuf::from(state.settings_snapshot().data.dir).join("covers");
    tokio::fs::create_dir_all(&covers_dir).await?;
    tokio::fs::write(covers_dir.join(format!("{id}.{extension}")), bytes).await?;
    sqlx::query("UPDATE anime SET cover_path=?,updated_at=datetime('now') WHERE id=?")
        .bind(extension)
        .bind(id)
        .execute(&state.pool)
        .await?;
    if let Some(previous) = previous.filter(|ext| matches!(ext.as_str(), "jpg" | "png" | "webp")) {
        if previous != extension {
            let _ = tokio::fs::remove_file(covers_dir.join(format!("{id}.{previous}"))).await;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn anime_cover(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let extension: Option<String> = sqlx::query_scalar("SELECT cover_path FROM anime WHERE id=?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let extension = extension.ok_or(AppError::NotFound)?;
    let content_type = match extension.as_str() {
        "jpg" => HeaderValue::from_static("image/jpeg"),
        "png" => HeaderValue::from_static("image/png"),
        "webp" => HeaderValue::from_static("image/webp"),
        _ => return Err(AppError::NotFound),
    };
    let path = PathBuf::from(state.settings_snapshot().data.dir)
        .join("covers")
        .join(format!("{id}.{extension}"));
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| AppError::NotFound)?;
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    Ok(response)
}

async fn refresh_metadata(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !state.settings_snapshot().metadata.anilist_enabled {
        return Err(AppError::Unavailable("AniList metadata is disabled".into()));
    }
    let row = sqlx::query("SELECT title,metadata_locked FROM anime WHERE id=?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let title: String = row.try_get("title")?;
    let locked: bool = row.try_get("metadata_locked")?;
    if locked {
        return Err(AppError::BadRequest(
            "metadata is locked; unlock it before refreshing".into(),
        ));
    }
    let (entry, cache_hit) = state
        .metadata
        .lookup(
            &state.pool,
            &title,
            state
                .settings_snapshot()
                .metadata
                .cache_ttl_hours
                .try_into()
                .unwrap_or(0),
        )
        .await
        .map_err(AppError::Unavailable)?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE anime SET title_native=?,synopsis=?,poster_path=?,year=?,status=?,anilist_id=?,updated_at=datetime('now') WHERE id=?")
        .bind(&entry.native_title).bind(&entry.description).bind(&entry.cover_url).bind(entry.year).bind(&entry.status).bind(entry.id).bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM anime_genre WHERE anime_id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for genre in &entry.genres {
        sqlx::query("INSERT INTO genre(name) VALUES(?) ON CONFLICT(name) DO NOTHING")
            .bind(genre)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO anime_genre(anime_id,genre_id) SELECT ?,id FROM genre WHERE name=? ON CONFLICT DO NOTHING").bind(id).bind(genre).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(
        serde_json::json!({"anilist_id":entry.id,"native_title":entry.native_title,"year":entry.year,"status":entry.status,"genres":entry.genres,"score":entry.score,"cache_hit":cache_hit}),
    ))
}

#[derive(Deserialize)]
struct MetadataLockInput {
    locked: bool,
}
async fn set_metadata_lock(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(input): Json<MetadataLockInput>,
) -> Result<StatusCode, AppError> {
    let result =
        sqlx::query("UPDATE anime SET metadata_locked=?,updated_at=datetime('now') WHERE id=?")
            .bind(input.locked)
            .bind(id)
            .execute(&state.pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn watch_page(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let row = sqlx::query("SELECT a.title, e.season, e.number, e.anime_id, e.video_codec, e.audio_codec, e.container FROM episode e JOIN anime a ON a.id=e.anime_id WHERE e.id=?").bind(id).fetch_optional(&state.pool).await?.ok_or(AppError::NotFound)?;
    let title: String = row.try_get("title")?;
    let season: i64 = row.try_get("season")?;
    let number: f64 = row.try_get("number")?;
    let anime_id: i64 = row.try_get("anime_id")?;
    let player_mode = state.settings_snapshot().desktop.player;
    let video_codec: Option<String> = row.try_get("video_codec")?;
    let audio_codec: Option<String> = row.try_get("audio_codec")?;
    let container: Option<String> = row.try_get("container")?;
    let probe_missing = video_codec.is_none() && audio_codec.is_none() && container.is_none();
    let use_hls = !probe_missing
        && !supports_direct_play(
            video_codec.as_deref(),
            audio_codec.as_deref(),
            container.as_deref(),
        );
    let player_source = if use_hls {
        format!("/hls/{id}/master.m3u8")
    } else {
        format!("/stream/{id}")
    };
    let next: Option<i64> = sqlx::query_scalar("SELECT id FROM episode WHERE anime_id=? AND (season>? OR (season=? AND number>?)) ORDER BY season,number LIMIT 1")
        .bind(anime_id).bind(season).bind(season).bind(number).fetch_optional(&state.pool).await?;
    let next_link = next
        .map(|next_id| {
            format!("<p><a id=\"next\" href=\"/watch/{next_id}\">Episode berikutnya</a></p>")
        })
        .unwrap_or_default();
    let subs = sqlx::query("SELECT id,lang,label FROM subtitle WHERE episode_id=? ORDER BY id")
        .bind(id)
        .fetch_all(&state.pool)
        .await?;
    let mut tracks = String::new();
    let mut subtitle_options = String::new();
    for (track_index, sub) in subs.into_iter().enumerate() {
        let sub_id: i64 = sub.try_get("id")?;
        let lang: Option<String> = sub.try_get("lang")?;
        let label: Option<String> = sub.try_get("label")?;
        tracks.push_str(&format!(
            "<track kind=\"subtitles\" src=\"/subs/{sub_id}.vtt\" srclang=\"{}\" label=\"{}\">",
            escape(lang.as_deref().unwrap_or("und")),
            escape(label.as_deref().unwrap_or("Subtitle"))
        ));
        subtitle_options.push_str(&format!(
            "<option value=\"{track_index}\">{}</option>",
            escape(label.as_deref().unwrap_or("Subtitle"))
        ));
    }
    let source = if use_hls {
        format!("/hls/{id}/master.m3u8")
    } else {
        player_source
    };
    let hls_script = if use_hls {
        "<script src=\"/static/hls.min.js\"></script>"
    } else {
        ""
    };
    let body = format!("<h1>{} · S{season:02} E{number}</h1><div class=\"player-toolbar\"><label>Subtitle <select id=\"subtitle-select\"><option value=\"\">Nonaktif</option>{subtitle_options}</select></label><button type=\"button\" class=\"secondary\" onclick=\"window.fetsPlayWithMpv()\">Putar di aplikasi dengan mpv</button></div><video id=\"player\" data-episode=\"{id}\" data-mode=\"{}\" data-src=\"{}\" controls preload=\"metadata\">{tracks}</video><p id=\"player-error\" role=\"status\">Jika mpv belum ditemukan, <a href=\"/settings\">pilih mpv.exe di Pengaturan</a> atau <a href=\"https://mpv.io/installation/\" target=\"_blank\" rel=\"noreferrer\">unduh mpv resmi</a>.</p>{next_link}{hls_script}<script src=\"/static/player.js\" defer></script>", escape(&title), escape(&player_mode), escape(&source));
    Ok(page(&title, &body))
}

async fn settings_page(State(state): State<Arc<AppState>>) -> Html<String> {
    let _ = state;
    page(
        "Pengaturan",
        r#"<h1>Pengaturan</h1>
      <section class="card"><h2>Folder anime</h2><p class="muted">Setiap subfolder menjadi satu anime.</p>
      <div class="button-row"><button id="add-folder" type="button">Tambah Folder</button><button id="scan-library" class="secondary" type="button">Pindai ulang</button></div>
      <ul id="library-folders" class="folder-list"></ul><progress id="scan-progress" max="100" value="0" hidden></progress><div id="scan-status" role="status" aria-live="polite"></div></section>
      <section class="card"><h2>Pemutar</h2><label>Pemutar default<select id="player"><option value="auto">Otomatis (HTML5 lalu mpv)</option><option value="html5">HTML5 saja</option><option value="mpv">mpv selalu</option></select></label>
      <label>mpv.exe <div class="input-row"><input id="mpv-path"><button class="secondary" type="button" data-pick="mpv_path">Pilih mpv.exe</button></div></label>
      <label>ffmpeg.exe <div class="input-row"><input id="ffmpeg-path"><button class="secondary" type="button" data-pick="ffmpeg_path">Pilih ffmpeg.exe</button></div></label>
      <label>ffprobe.exe <div class="input-row"><input id="ffprobe-path"><button class="secondary" type="button" data-pick="ffprobe_path">Pilih ffprobe.exe</button></div></label></section>
      <section class="card"><h2>Preferensi</h2><label>Tema<select id="theme"><option value="dark">Gelap</option><option value="light">Terang</option><option value="system">Ikuti Windows</option></select></label>
      <label class="check-row"><input id="watch" type="checkbox"> Pindai otomatis saat folder berubah</label><label class="check-row"><input id="anilist" type="checkbox"> Ambil metadata dari AniList</label>
      <p id="settings-status" role="status" aria-live="polite"></p></section>"#,
    )
}

#[derive(Deserialize)]
struct LibraryFolderInput {
    path: String,
}

async fn add_library_folder(
    State(state): State<Arc<AppState>>,
    Json(input): Json<LibraryFolderInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let folder = FsPath::new(input.path.trim()).canonicalize().map_err(|_| {
        AppError::BadRequest("folder tidak ditemukan atau tidak dapat diakses".into())
    })?;
    if !folder.is_dir() {
        return Err(AppError::BadRequest("pilih sebuah folder".into()));
    }
    let path = crate::desktop::display_path(&folder);
    {
        let mut settings = state.settings.write().expect("settings lock");
        if !settings
            .library
            .paths
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&path))
        {
            settings.library.paths.push(path.clone());
            settings.save()?;
        }
    }
    let job_id = start_scan(Arc::clone(&state))?;
    Ok(Json(serde_json::json!({"path": path, "job_id": job_id})))
}

async fn remove_library_folder(
    State(state): State<Arc<AppState>>,
    Json(input): Json<LibraryFolderInput>,
) -> Result<StatusCode, AppError> {
    // Resolve the target folder to its canonical form so a user-supplied path
    // like `c:\anime` matches stored entries such as `C:\Anime\...`.
    let target = FsPath::new(input.path.trim())
        .canonicalize()
        .map_err(|_| AppError::BadRequest("folder tidak ditemukan".into()))?;
    {
        let mut settings = state.settings.write().expect("settings lock");
        settings.library.paths.retain(|path| {
            let candidate = FsPath::new(path);
            !(crate::desktop::path_eq(candidate, &target)
                || candidate.starts_with(&target)
                || target.starts_with(candidate))
        });
        settings.save()?;
    }
    let rows = sqlx::query("SELECT id,file_path FROM episode")
        .fetch_all(&state.pool)
        .await?;
    for row in rows {
        let path: String = row.try_get("file_path")?;
        if FsPath::new(&path).starts_with(&target) {
            let id: i64 = row.try_get("id")?;
            sqlx::query("DELETE FROM episode WHERE id=?")
                .bind(id)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct SettingsResponse {
    player: String,
    mpv_path: String,
    ffmpeg_path: String,
    ffprobe_path: String,
    theme: String,
    watch: bool,
    anilist_enabled: bool,
    library_paths: Vec<String>,
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsResponse> {
    let settings = state.settings_snapshot();
    Json(SettingsResponse {
        player: settings.desktop.player,
        mpv_path: settings.desktop.mpv_path,
        ffmpeg_path: settings.ffmpeg.ffmpeg_path,
        ffprobe_path: settings.ffmpeg.ffprobe_path,
        theme: settings.desktop.theme,
        watch: settings.library.watch,
        anilist_enabled: settings.metadata.anilist_enabled,
        library_paths: settings.library.paths,
    })
}

#[derive(Deserialize)]
struct SettingsUpdate {
    player: Option<String>,
    mpv_path: Option<String>,
    ffmpeg_path: Option<String>,
    ffprobe_path: Option<String>,
    theme: Option<String>,
    watch: Option<bool>,
    anilist_enabled: Option<bool>,
}

async fn save_settings(
    State(state): State<Arc<AppState>>,
    Json(update): Json<SettingsUpdate>,
) -> Result<StatusCode, AppError> {
    let mut settings = state.settings.write().expect("settings lock");
    if let Some(player) = update.player {
        if !matches!(player.as_str(), "auto" | "html5" | "mpv") {
            return Err(AppError::BadRequest("pilihan pemutar tidak valid".into()));
        }
        settings.desktop.player = player;
    }
    if let Some(theme) = update.theme {
        if !matches!(theme.as_str(), "dark" | "light" | "system") {
            return Err(AppError::BadRequest("pilihan tema tidak valid".into()));
        }
        settings.desktop.theme = theme;
    }
    if let Some(value) = update.mpv_path {
        settings.desktop.mpv_path = value;
    }
    if let Some(value) = update.ffmpeg_path {
        settings.ffmpeg.ffmpeg_path = value;
    }
    if let Some(value) = update.ffprobe_path {
        settings.ffmpeg.ffprobe_path = value;
    }
    if let Some(value) = update.watch {
        settings.library.watch = value;
    }
    if let Some(value) = update.anilist_enabled {
        settings.metadata.anilist_enabled = value;
    }
    settings.save()?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MpvPlayInput {
    episode_id: i64,
    position_sec: f64,
    reason: Option<String>,
}

async fn play_with_mpv(
    State(state): State<Arc<AppState>>,
    Json(input): Json<MpvPlayInput>,
) -> Result<StatusCode, AppError> {
    if !input.position_sec.is_finite() || input.position_sec < 0.0 {
        return Err(AppError::BadRequest("posisi pemutaran tidak valid".into()));
    }
    let settings = state.settings_snapshot();
    let mpv_path = mpv::find_mpv(Some(&settings.desktop.mpv_path)).ok_or_else(|| {
        AppError::Unavailable("mpv.exe tidak ditemukan. Pilih mpv.exe di Pengaturan atau unduh dari https://mpv.io/installation/".into())
    })?;
    let stored_path = sqlx::query_scalar::<_, String>(
        "SELECT file_path FROM episode WHERE id=? AND removed_at IS NULL",
    )
    .bind(input.episode_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    let media_path = validate_library_path(&state, &stored_path)?;
    let id = uuid::Uuid::new_v4().to_string();
    let pipe = mpv::ipc_pipe(&id);
    // NOTE: we deliberately do NOT pass --wid=<webview hwnd> anymore. That
    // made mpv render into a stale webview child window so the video never
    // appeared ("tidak terputar"). mpv now opens its own borderless window
    // and the desktop shell embeds it via the mpv-attached event below.
    let args = mpv::build_args_with_parent(&pipe, input.position_sec, &media_path, None);
    tracing::info!(episode_id = input.episode_id, reason = ?input.reason, "launching mpv playback");
    let (session, os_pid) = MpvSession::spawn(&mpv_path, &id, &args)
        .await
        .map_err(|error| AppError::Unavailable(format!("mpv tidak dapat dijalankan: {error}")))?;
    // Tell the Tauri shell which mpv process to reparent into the app frame.
    state.notify_mpv_launch(&id, os_pid, input.episode_id);
    let child = session.into_child();
    tokio::spawn(watch_mpv_progress(
        Arc::clone(&state),
        input.episode_id,
        pipe,
        child,
    ));
    Ok(StatusCode::ACCEPTED)
}

async fn watch_mpv_progress(
    state: Arc<AppState>,
    episode_id: i64,
    pipe: String,
    mut child: tokio::process::Child,
) {
    #[cfg(windows)]
    {
        let pipe_path = pipe.clone();
        let opened = tokio::task::spawn_blocking(move || {
            crate::services::mpv::pipe_io::PipeHandle::open(&pipe_path, 50)
        })
        .await;
        if let Ok(Ok(handle)) = opened {
            let handle = Arc::new(handle);
            let _ = handle.write_line(r#"{"command":["observe_property",1,"time-pos"]}"#);
            let _ = handle.write_line(r#"{"command":["observe_property",2,"eof-reached"]}"#);
            let _ = handle.write_line(r#"{"command":["observe_property",3,"pause"]}"#);
            let mut position = 0.0;
            let mut watched = false;
            loop {
                let reader = Arc::clone(&handle);
                let line = tokio::task::spawn_blocking(move || reader.read_line()).await;
                let Ok(Ok(line)) = line else { break };
                if line.is_empty() {
                    break;
                }
                let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if event.get("event").and_then(serde_json::Value::as_str) == Some("property-change")
                    && event.get("name").and_then(serde_json::Value::as_str) == Some("pause")
                    && event.get("data").and_then(serde_json::Value::as_bool) == Some(true)
                {
                    if let Err(error) =
                        save_mpv_progress(&state.pool, episode_id, position, watched).await
                    {
                        tracing::warn!(%error, episode_id, "could not save paused mpv progress");
                    }
                    continue;
                }
                if let Some((new_position, new_watched)) = mpv::progress_from_event(&event) {
                    if !new_watched {
                        position = new_position;
                    }
                    watched |= new_watched;
                    if let Err(error) =
                        save_mpv_progress(&state.pool, episode_id, position, watched).await
                    {
                        tracing::warn!(%error, episode_id, "could not save mpv progress");
                    }
                }
            }
            if let Err(error) = save_mpv_progress(&state.pool, episode_id, position, watched).await
            {
                tracing::warn!(%error, episode_id, "could not flush final mpv progress");
            }
        } else if let Ok(Err(error)) = opened {
            tracing::warn!(%error, episode_id, "could not connect to mpv IPC pipe");
        }
    }
    #[cfg(not(windows))]
    {
        match crate::services::mpv::pipe_io::PipeHandle::open(&pipe, 50).await {
            Ok(handle) => {
                let _ = handle
                    .write_line(r#"{"command":["observe_property",1,"time-pos"]}"#)
                    .await;
                let _ = handle
                    .write_line(r#"{"command":["observe_property",2,"eof-reached"]}"#)
                    .await;
                let mut position = 0.0;
                let mut watched = false;
                loop {
                    match handle.read_line().await {
                        Ok(line) if !line.is_empty() => {
                            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                                if let Some((new_position, new_watched)) =
                                    mpv::progress_from_event(&event)
                                {
                                    if !new_watched {
                                        position = new_position;
                                    }
                                    watched |= new_watched;
                                    if let Err(error) = save_mpv_progress(
                                        &state.pool,
                                        episode_id,
                                        position,
                                        watched,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            %error,
                                            episode_id,
                                            "could not save mpv progress"
                                        );
                                    }
                                }
                            }
                        }
                        _ => break,
                    }
                }
                if let Err(error) =
                    save_mpv_progress(&state.pool, episode_id, position, watched).await
                {
                    tracing::warn!(%error, episode_id, "could not flush final mpv progress");
                }
            }
            Err(error) => {
                tracing::warn!(%error, episode_id, "could not connect to mpv IPC socket")
            }
        }
    }
    if let Err(error) = child.wait().await {
        tracing::warn!(%error, episode_id, "error while waiting for mpv to exit");
    }
}

async fn save_mpv_progress(
    pool: &sqlx::SqlitePool,
    episode_id: i64,
    position_sec: f64,
    watched: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO progress(episode_id,position_sec,watched) VALUES(?,?,?) ON CONFLICT(episode_id) DO UPDATE SET position_sec=excluded.position_sec,watched=MAX(progress.watched,excluded.watched),updated_at=datetime('now')")
        .bind(episode_id)
        .bind(position_sec.max(0.0))
        .bind(watched)
        .execute(pool)
        .await?;
    Ok(())
}

async fn add_page() -> Html<String> {
    page("Tambah anime", "<h1>Tambah anime</h1><form id=\"add\"><label>Nama anime <input name=\"title\" required></label><label>Folder (opsional, harus di dalam library)<input name=\"folder\"></label><button>Tambah</button></form><p id=\"message\"></p><script>document.querySelector('#add').addEventListener('submit',async e=>{e.preventDefault();const f=new FormData(e.target);const body={title:f.get('title'),folder:f.get('folder')||null};const r=await fetch('/api/anime',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});document.querySelector('#message').textContent=r.ok?'Anime berhasil ditambahkan':await r.text();if(r.ok)location.href='/';});</script>")
}

#[derive(Serialize)]
struct AnimeDto {
    id: i64,
    title: String,
}
async fn list_anime(State(state): State<Arc<AppState>>) -> Result<Json<Vec<AnimeDto>>, AppError> {
    let rows = sqlx::query("SELECT id,title FROM anime ORDER BY title")
        .fetch_all(&state.pool)
        .await?;
    rows.into_iter()
        .map(|r| {
            Ok(AnimeDto {
                id: r.try_get("id")?,
                title: r.try_get("title")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map(Json)
        .map_err(AppError::from)
}

#[derive(Deserialize)]
struct AddAnime {
    title: String,
    folder: Option<String>,
}
async fn add_anime(
    State(state): State<Arc<AppState>>,
    Json(input): Json<AddAnime>,
) -> Result<(StatusCode, Json<AnimeDto>), AppError> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(AppError::BadRequest("title must not be empty".into()));
    }
    let folder = match input.folder {
        Some(folder) => Some(validate_library_path(&state, &folder)?),
        None => None,
    };
    let result = sqlx::query("INSERT INTO anime(title,folder_path) VALUES(?,?)")
        .bind(title)
        .bind(folder.as_ref().map(|p| p.to_string_lossy().to_string()))
        .execute(&state.pool)
        .await?;
    let dto = AnimeDto {
        id: result.last_insert_rowid(),
        title: title.into(),
    };
    Ok((StatusCode::CREATED, Json(dto)))
}

#[derive(Deserialize)]
struct AddEpisode {
    file_path: String,
    season: Option<i64>,
    number: Option<f64>,
}
async fn add_episode(
    State(state): State<Arc<AppState>>,
    Path(anime_id): Path<i64>,
    Json(input): Json<AddEpisode>,
) -> Result<StatusCode, AppError> {
    let path = validate_library_path(&state, &input.file_path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !state
        .settings_snapshot()
        .library
        .extensions
        .iter()
        .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext))
    {
        return Err(AppError::BadRequest(
            "video extension is not allowed".into(),
        ));
    }
    let meta = tokio::fs::metadata(&path).await?;
    let guessed = parse_filename(&path);
    let season = input
        .season
        .or_else(|| guessed.as_ref().map(|p| p.season as i64))
        .unwrap_or(1);
    let number = input
        .number
        .or_else(|| guessed.as_ref().map(|p| p.number))
        .ok_or_else(|| {
            AppError::BadRequest("episode number is required when it cannot be parsed".into())
        })?;
    let episode_id: i64 = sqlx::query_scalar(
        "INSERT INTO episode(anime_id,season,number,file_path,file_size) VALUES(?,?,?,?,?) RETURNING id",
    )
    .bind(anime_id)
    .bind(season)
    .bind(number)
    .bind(path.to_string_lossy().to_string())
    .bind(meta.len() as i64)
    .fetch_one(&state.pool)
    .await?;
    match probe(&state.settings_snapshot().ffmpeg.ffprobe_path, &path).await {
        Ok(info) => {
            sqlx::query("UPDATE episode SET duration_sec=?,video_codec=?,audio_codec=?,container=?,streams_json=? WHERE id=?")
                .bind(info.duration_sec).bind(info.video_codec).bind(info.audio_codec).bind(info.container).bind(info.streams_json).bind(episode_id).execute(&state.pool).await?;
            extract_embedded_subtitles(&state, episode_id, &path).await;
            let thumb = PathBuf::from(&state.settings_snapshot().data.dir)
                .join("thumbs")
                .join(format!("{episode_id}.jpg"));
            if generate_thumbnail(&state.settings_snapshot().ffmpeg.ffmpeg_path, &path, &thumb)
                .await
                .is_ok()
            {
                sqlx::query("UPDATE episode SET thumb_path=? WHERE id=?")
                    .bind(thumb.to_string_lossy().to_string())
                    .bind(episode_id)
                    .execute(&state.pool)
                    .await?;
            }
        }
        Err(error) => {
            tracing::debug!(%error, path=%path.display(), "ffprobe unavailable for manually added episode")
        }
    }
    sync_external_subtitles(&state.pool, episode_id, &path).await?;
    Ok(StatusCode::CREATED)
}

fn validate_library_path(state: &AppState, input: &str) -> Result<PathBuf, AppError> {
    let candidate = FsPath::new(input)
        .canonicalize()
        .map_err(|_| AppError::BadRequest("file path does not exist".into()))?;
    let allowed = state
        .settings_snapshot()
        .library
        .paths
        .iter()
        .filter_map(|p| FsPath::new(p).canonicalize().ok())
        .any(|root| candidate.starts_with(root));
    if !allowed {
        return Err(AppError::BadRequest(
            "path must be inside a configured library folder".into(),
        ));
    }
    Ok(candidate)
}

async fn scan_library(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let job_id = start_scan(Arc::clone(&state))?;
    Ok(Json(serde_json::json!({"job_id": job_id})))
}

#[derive(Serialize)]
struct ScanStatus {
    job_id: Option<String>,
    running: bool,
    total: u64,
    done: u64,
    current: String,
    last_result: Option<String>,
}

#[derive(Serialize)]
struct ScanEvent {
    job_id: String,
    state: &'static str,
    total: u64,
    done: u64,
    current: String,
    message: Option<String>,
}

async fn scan_status(State(state): State<Arc<AppState>>) -> Json<ScanStatus> {
    let active = state.scan.lock().expect("scan lock").clone();
    let status = if let Some(job) = active {
        ScanStatus {
            job_id: Some(job.id.clone()),
            running: true,
            total: job.total.load(std::sync::atomic::Ordering::Relaxed),
            done: job.done.load(std::sync::atomic::Ordering::Relaxed),
            current: job.current.lock().expect("scan current lock").clone(),
            last_result: None,
        }
    } else {
        ScanStatus {
            job_id: None,
            running: false,
            total: 0,
            done: 0,
            current: String::new(),
            last_result: state.last_scan.lock().expect("last scan lock").clone(),
        }
    };
    Json(status)
}

async fn scan_events(State(state): State<Arc<AppState>>) -> Response {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    state
        .scan_listeners
        .lock()
        .expect("scan listeners lock")
        .push(tx);
    let body = Body::from_stream(async_stream::stream! {
        while let Some(event) = rx.recv().await {
            yield Ok::<String, std::convert::Infallible>(format!("data: {event}\n\n"));
        }
    });
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn publish_scan_event(state: &AppState, event: ScanEvent) {
    if let Ok(message) = serde_json::to_string(&event) {
        state
            .scan_listeners
            .lock()
            .expect("scan listeners lock")
            .retain(|listener| listener.send(message.clone()).is_ok());
    }
}

pub(crate) fn start_scan(state: Arc<AppState>) -> Result<String, AppError> {
    let mut active = state.scan.lock().expect("scan lock");
    if let Some(job) = active.as_ref() {
        return Ok(job.id.clone());
    }
    let job = Arc::new(crate::state::ScanJob {
        id: uuid::Uuid::new_v4().to_string(),
        total: std::sync::atomic::AtomicU64::new(0),
        done: std::sync::atomic::AtomicU64::new(0),
        current: std::sync::Mutex::new(String::new()),
    });
    let job_id = job.id.clone();
    *active = Some(Arc::clone(&job));
    drop(active);
    tokio::spawn(async move {
        let result = scan_folders_with_job(&state, Some(Arc::clone(&job))).await;
        let message = match result {
            Ok(count) => format!("Pemindaian selesai. {count} file video ditemukan."),
            Err(error) => format!("Pemindaian gagal: {error}"),
        };
        *job.current.lock().expect("scan current lock") = message.clone();
        let _ = state
            .last_scan
            .lock()
            .map(|mut previous| *previous = Some(message.clone()));
        let _ = state.scan.lock().map(|mut active| *active = None);
        publish_scan_event(
            &state,
            ScanEvent {
                job_id: job.id.clone(),
                state: "complete",
                total: job.total.load(std::sync::atomic::Ordering::Relaxed),
                done: job.done.load(std::sync::atomic::Ordering::Relaxed),
                current: message.clone(),
                message: Some(message),
            },
        );
    });
    Ok(job_id)
}

pub async fn scan_startup(state: &AppState) -> Result<(), AppError> {
    scan_folders(state).await.map(|_| ())
}

async fn scan_folders(state: &AppState) -> Result<usize, AppError> {
    scan_folders_with_job(state, None).await
}

async fn scan_folders_with_job(
    state: &AppState,
    job: Option<Arc<crate::state::ScanJob>>,
) -> Result<usize, AppError> {
    let mut found = 0usize;
    for configured_root in &state.settings_snapshot().library.paths {
        let root = match FsPath::new(configured_root).canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let root_for_walk = root.clone();
        let mut files = tokio::task::spawn_blocking(move || {
            walkdir::WalkDir::new(root_for_walk)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
                .map(|e| e.into_path())
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
        files.sort();
        if let Some(job) = &job {
            job.total
                .fetch_add(files.len() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        let present: HashSet<PathBuf> = files.iter().cloned().collect();
        for (file_index, file) in files.into_iter().enumerate() {
            if let Some(job) = &job {
                let done = job.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                *job.current.lock().expect("scan current lock") =
                    crate::desktop::display_path(&file);
                if done % 5 == 0 || done == job.total.load(std::sync::atomic::Ordering::Relaxed) {
                    publish_scan_event(
                        state,
                        ScanEvent {
                            job_id: job.id.clone(),
                            state: "running",
                            total: job.total.load(std::sync::atomic::Ordering::Relaxed),
                            done,
                            current: job.current.lock().expect("scan current lock").clone(),
                            message: None,
                        },
                    );
                }
            }
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !state
                .settings_snapshot()
                .library
                .extensions
                .iter()
                .any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(ext))
            {
                continue;
            }
            let folder = file.parent().unwrap_or(&root);
            let anime_folder = folder
                .strip_prefix(&root)
                .ok()
                .and_then(|rel| rel.components().next())
                .map(|part| root.join(part.as_os_str()))
                .unwrap_or_else(|| folder.to_path_buf());
            let folder_name = anime_folder
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_else(|| file.file_stem().and_then(|n| n.to_str()).unwrap_or("Anime"))
                .to_string();
            let parsed = parse_filename(&file).unwrap_or(ParsedEpisode {
                title: folder_name.clone(),
                season: 1,
                number: (file_index + 1) as f64,
            });
            let anime_name = folder_name;
            let anime_id: i64 = sqlx::query_scalar("INSERT INTO anime(title,folder_path) VALUES(?,?) ON CONFLICT(folder_path) DO UPDATE SET title=excluded.title RETURNING id")
                .bind(anime_name).bind(anime_folder.to_string_lossy().to_string()).fetch_one(&state.pool).await?;
            let metadata = tokio::fs::metadata(&file).await?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default();
            let episode_id: i64 = sqlx::query_scalar("INSERT INTO episode(anime_id,season,number,title,file_path,file_size,file_mtime) VALUES(?,?,?,?,?,?,?) ON CONFLICT(file_path) DO UPDATE SET anime_id=excluded.anime_id,season=excluded.season,number=excluded.number,title=excluded.title,file_size=excluded.file_size,file_mtime=excluded.file_mtime RETURNING id")
                .bind(anime_id).bind(parsed.season as i64).bind(parsed.number).bind(Option::<String>::None).bind(file.to_string_lossy().to_string()).bind(metadata.len() as i64).bind(modified).fetch_one(&state.pool).await?;
            if let Ok(info) = probe(&state.settings_snapshot().ffmpeg.ffprobe_path, &file).await {
                sqlx::query("UPDATE episode SET duration_sec=?,video_codec=?,audio_codec=?,container=?,streams_json=? WHERE id=?")
                    .bind(info.duration_sec).bind(info.video_codec).bind(info.audio_codec).bind(info.container).bind(info.streams_json).bind(episode_id).execute(&state.pool).await?;
                sqlx::query("DELETE FROM subtitle WHERE episode_id=?")
                    .bind(episode_id)
                    .execute(&state.pool)
                    .await?;
                sync_external_subtitles(&state.pool, episode_id, &file).await?;
                extract_embedded_subtitles(state, episode_id, &file).await;
                let thumb = PathBuf::from(&state.settings_snapshot().data.dir)
                    .join("thumbs")
                    .join(format!("{episode_id}.jpg"));
                if generate_thumbnail(&state.settings_snapshot().ffmpeg.ffmpeg_path, &file, &thumb)
                    .await
                    .is_ok()
                {
                    sqlx::query("UPDATE episode SET thumb_path=? WHERE id=?")
                        .bind(thumb.to_string_lossy().to_string())
                        .bind(episode_id)
                        .execute(&state.pool)
                        .await?;
                }
            } else {
                tracing::debug!(path = %file.display(), "ffprobe unavailable or could not read media; retaining direct-play entry");
                sync_external_subtitles(&state.pool, episode_id, &file).await?;
            }
            found += 1;
        }
        let known = sqlx::query("SELECT id, file_path FROM episode")
            .fetch_all(&state.pool)
            .await?;
        for row in known {
            let id: i64 = row.try_get("id")?;
            let path: String = row.try_get("file_path")?;
            let path = FsPath::new(&path);
            if path.starts_with(&root) && !present.contains(path) {
                sqlx::query("DELETE FROM episode WHERE id=?")
                    .bind(id)
                    .execute(&state.pool)
                    .await?;
            }
        }
    }
    Ok(found)
}

async fn sync_external_subtitles(
    pool: &sqlx::SqlitePool,
    episode_id: i64,
    video_path: &FsPath,
) -> Result<(), AppError> {
    let Some(parent) = video_path.parent() else {
        return Ok(());
    };
    let Some(video_stem) = video_path.file_stem().and_then(|s| s.to_str()) else {
        return Ok(());
    };
    let mut entries = match tokio::fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["srt", "vtt", "ass", "ssa"].contains(&extension.as_str()) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem != video_stem
            && !stem
                .strip_prefix(video_stem)
                .is_some_and(|suffix| suffix.starts_with(['.', '_', '-']))
        {
            continue;
        }
        let suffix = stem
            .strip_prefix(video_stem)
            .unwrap_or("")
            .trim_start_matches(['.', '_', '-']);
        let lang = suffix
            .rsplit(['.', '_', '-'])
            .next()
            .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()));
        sqlx::query(
            "INSERT INTO subtitle(episode_id,lang,label,format,file_path) VALUES(?,?,?,?,?)",
        )
        .bind(episode_id)
        .bind(lang)
        .bind(path.file_name().and_then(|n| n.to_str()))
        .bind(extension)
        .bind(path.to_string_lossy().to_string())
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn extract_embedded_subtitles(state: &AppState, episode_id: i64, video_path: &FsPath) {
    let Ok(row) =
        sqlx::query_scalar::<_, Option<String>>("SELECT streams_json FROM episode WHERE id=?")
            .bind(episode_id)
            .fetch_one(&state.pool)
            .await
    else {
        return;
    };
    let Some(streams_json) = row else {
        return;
    };
    let Ok(streams) = serde_json::from_str::<Vec<serde_json::Value>>(&streams_json) else {
        return;
    };
    for stream in streams.iter().filter(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("subtitle")
    }) {
        let Some(stream_idx) = stream.get("index").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        let output = PathBuf::from(&state.settings_snapshot().data.dir)
            .join("subtitles")
            .join(format!("{episode_id}-{stream_idx}.vtt"));
        if let Some(parent) = output.parent() {
            if tokio::fs::create_dir_all(parent).await.is_err() {
                continue;
            }
        }
        let mut command =
            tokio::process::Command::new(&state.settings_snapshot().ffmpeg.ffmpeg_path);
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(video_path)
            .arg("-map")
            .arg(format!("0:{stream_idx}"))
            .args(["-f", "webvtt", "-y"])
            .arg(&output);
        crate::desktop::no_window(&mut command);
        let result = command.output().await;
        if !matches!(result, Ok(ref output) if output.status.success()) {
            continue;
        }
        let language = stream
            .get("tags")
            .and_then(|tags| tags.get("language"))
            .and_then(serde_json::Value::as_str);
        let label = stream
            .get("tags")
            .and_then(|tags| tags.get("title"))
            .and_then(serde_json::Value::as_str)
            .or(language);
        if let Err(error) = sqlx::query(
            "INSERT INTO subtitle(episode_id,lang,label,format,stream_idx) VALUES(?,?,?,'vtt',?)",
        )
        .bind(episode_id)
        .bind(language)
        .bind(label)
        .bind(stream_idx)
        .execute(&state.pool)
        .await
        {
            tracing::debug!(%error, episode_id, stream_idx, "could not save embedded subtitle metadata");
        }
    }
}

async fn list_subtitles(
    State(state): State<Arc<AppState>>,
    Path(episode_id): Path<i64>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows =
        sqlx::query("SELECT id,lang,label,format FROM subtitle WHERE episode_id=? ORDER BY id")
            .bind(episode_id)
            .fetch_all(&state.pool)
            .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(serde_json::json!({
            "id": row.try_get::<i64, _>("id")?,
            "lang": row.try_get::<Option<String>, _>("lang")?,
            "label": row.try_get::<Option<String>, _>("label")?,
            "format": row.try_get::<String, _>("format")?,
        }));
    }
    Ok(Json(result))
}

async fn subtitle_vtt(
    State(state): State<Arc<AppState>>,
    Path(subtitle_id): Path<i64>,
) -> Result<Response, AppError> {
    let row = sqlx::query("SELECT s.format,s.file_path,s.stream_idx,s.episode_id,e.file_path AS media_path FROM subtitle s JOIN episode e ON e.id=s.episode_id WHERE s.id=?")
        .bind(subtitle_id).fetch_optional(&state.pool).await?.ok_or(AppError::NotFound)?;
    let format: String = row.try_get("format")?;
    let file_path: Option<String> = row.try_get("file_path")?;
    let contents = if let Some(file_path) = file_path {
        let path = validate_library_path(&state, &file_path)?;
        tokio::fs::read_to_string(path).await?
    } else {
        let stream_idx: i64 = row.try_get("stream_idx")?;
        let episode_id: i64 = row.try_get("episode_id")?;
        let media_path: String = row.try_get("media_path")?;
        let media_path = validate_library_path(&state, &media_path)?;
        let cached = PathBuf::from(&state.settings_snapshot().data.dir)
            .join("subtitles")
            .join(format!("{episode_id}-{stream_idx}.vtt"));
        if !cached.is_file() {
            tokio::fs::create_dir_all(cached.parent().ok_or(AppError::NotFound)?).await?;
            let mut command =
                tokio::process::Command::new(&state.settings_snapshot().ffmpeg.ffmpeg_path);
            command
                .args(["-hide_banner", "-loglevel", "error", "-i"])
                .arg(media_path)
                .arg("-map")
                .arg(format!("0:{stream_idx}"))
                .args(["-f", "webvtt", "-y"])
                .arg(&cached);
            crate::desktop::no_window(&mut command);
            let result = command.output().await.map_err(|e| {
                AppError::BadRequest(format!("ffmpeg could not extract subtitle: {e}"))
            })?;
            if !result.status.success() {
                return Err(AppError::BadRequest(
                    String::from_utf8_lossy(&result.stderr).trim().to_owned(),
                ));
            }
        }
        tokio::fs::read_to_string(cached).await?
    };
    let vtt = to_webvtt(&format, &contents).map_err(AppError::BadRequest)?;
    let mut response = Response::new(Body::from(vtt));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/vtt; charset=utf-8"),
    );
    Ok(response)
}

async fn episode_thumbnail(
    State(state): State<Arc<AppState>>,
    Path(episode_id): Path<i64>,
) -> Result<Response, AppError> {
    let value: Option<String> = sqlx::query_scalar("SELECT thumb_path FROM episode WHERE id=?")
        .bind(episode_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let value = value.ok_or(AppError::NotFound)?;
    let path = FsPath::new(&value).canonicalize()?;
    let thumb_root = FsPath::new(&state.settings_snapshot().data.dir)
        .join("thumbs")
        .canonicalize()?;
    if !path.starts_with(thumb_root) {
        return Err(AppError::NotFound);
    }
    let bytes = tokio::fs::read(path).await?;
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    Ok(response)
}

#[derive(Deserialize)]
struct ProgressInput {
    position_sec: f64,
    watched: Option<bool>,
}
async fn get_progress(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let progress = sqlx::query("SELECT position_sec, watched FROM progress WHERE episode_id=?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    if let Some(row) = progress {
        let position: f64 = row.try_get("position_sec")?;
        let watched: bool = row.try_get("watched")?;
        Ok(Json(
            serde_json::json!({"position_sec": position, "watched": watched}),
        ))
    } else {
        Ok(Json(
            serde_json::json!({"position_sec": 0, "watched": false}),
        ))
    }
}

async fn save_progress(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(input): Json<ProgressInput>,
) -> Result<StatusCode, AppError> {
    if !input.position_sec.is_finite() || input.position_sec < 0.0 {
        return Err(AppError::BadRequest(
            "position_sec must be a non-negative number".into(),
        ));
    }
    let watched = input.watched.unwrap_or(false);
    sqlx::query("INSERT INTO progress(episode_id,position_sec,watched) VALUES(?,?,?) ON CONFLICT(episode_id) DO UPDATE SET position_sec=excluded.position_sec,watched=excluded.watched,updated_at=datetime('now')")
        .bind(id).bind(input.position_sec).bind(watched).execute(&state.pool).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || size == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        Some((size.saturating_sub(suffix), size - 1))
    } else {
        let start: u64 = start.parse().ok()?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>().ok()?.min(size - 1)
        };
        (start < size && start <= end).then_some((start, end))
    }
}

async fn stream_episode(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let value: String = sqlx::query_scalar("SELECT file_path FROM episode WHERE id=?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let path = validate_library_path(&state, &value)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !state
        .settings_snapshot()
        .library
        .extensions
        .iter()
        .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
    {
        return Err(AppError::BadRequest(
            "video extension is not allowed".into(),
        ));
    }
    let file = tokio::fs::File::open(&path).await?;
    let size = file.metadata().await?.len();
    let range_header = headers.get(header::RANGE).and_then(|h| h.to_str().ok());
    let (start, end, status) = match range_header {
        Some(raw) => match parse_range(raw, size) {
            Some((s, e)) => (s, e, StatusCode::PARTIAL_CONTENT),
            None => {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{size}"))],
                )
                    .into_response())
            }
        },
        None => (0, size.saturating_sub(1), StatusCode::OK),
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut file = file;
    file.seek(SeekFrom::Start(start)).await?;
    let body = Body::from_stream(ReaderStream::new(file.take(length)));
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let content_type = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap_or(HeaderValue::from_static("0")),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                .unwrap_or(HeaderValue::from_static("bytes */0")),
        );
    }
    Ok(response)
}

async fn media_path_for_hls(state: &AppState, episode_id: i64) -> Result<PathBuf, AppError> {
    let value: String = sqlx::query_scalar("SELECT file_path FROM episode WHERE id=?")
        .bind(episode_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let path = validate_library_path(state, &value)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !state
        .settings_snapshot()
        .library
        .extensions
        .iter()
        .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
    {
        return Err(AppError::BadRequest(
            "video extension is not allowed".into(),
        ));
    }
    Ok(path)
}

async fn hls_playlist(
    State(state): State<Arc<AppState>>,
    Path(episode_id): Path<i64>,
) -> Result<Response, AppError> {
    let media_path = media_path_for_hls(&state, episode_id).await?;
    let ffmpeg_path = state.settings_snapshot().ffmpeg.ffmpeg_path.clone();
    let directory = state
        .hls
        .ensure_session(episode_id, &media_path, &ffmpeg_path)
        .await
        .map_err(AppError::Unavailable)?;
    let playlist = tokio::fs::read_to_string(directory.join("index.m3u8")).await?;
    let mut response = Response::new(Body::from(playlist));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

async fn hls_segment(
    State(state): State<Arc<AppState>>,
    Path((episode_id, segment)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    if !valid_segment_name(&segment) {
        return Err(AppError::NotFound);
    }
    let media_path = media_path_for_hls(&state, episode_id).await?;
    let ffmpeg_path = state.settings_snapshot().ffmpeg.ffmpeg_path.clone();
    let directory = state
        .hls
        .ensure_session(episode_id, &media_path, &ffmpeg_path)
        .await
        .map_err(AppError::Unavailable)?;
    let directory = directory.canonicalize()?;
    let path = directory
        .join(segment)
        .canonicalize()
        .map_err(|_| AppError::NotFound)?;
    if !path.starts_with(&directory) {
        return Err(AppError::NotFound);
    }
    let file = tokio::fs::File::open(&path).await?;
    let length = file.metadata().await?.len();
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap_or(HeaderValue::from_static("0")),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::parse_range;
    use axum::http::StatusCode;
    #[test]
    fn parses_ranges() {
        assert_eq!(parse_range("bytes=2-5", 10), Some((2, 5)));
        assert_eq!(parse_range("bytes=7-", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=1-2,4-5", 10), None);
    }

    #[tokio::test]
    async fn health_returns_ok() {
        use axum::{body::Body, http::Request};
        use sqlx::sqlite::SqlitePoolOptions;
        use std::path::PathBuf;
        use std::sync::Arc;
        use tower::ServiceExt;

        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let app = super::router(Arc::new(crate::state::AppState::new(
            pool,
            crate::config::Settings::default(),
            Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        )));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn settings_api_persists_desktop_preferences() {
        use axum::{body::Body, http::Request};
        use sqlx::sqlite::SqlitePoolOptions;
        use std::path::PathBuf;
        use std::sync::Arc;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let mut settings = crate::config::Settings::default();
        settings.data.dir = dir.path().to_string_lossy().into_owned();
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let app = super::router(Arc::new(crate::state::AppState::new(
            pool,
            settings,
            Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        )));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"theme":"light","player":"mpv","watch":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let saved = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        let persisted: crate::config::Settings = toml::from_str(&saved).unwrap();
        assert_eq!(persisted.desktop.theme, "light");
        assert_eq!(persisted.desktop.player, "mpv");
        assert!(!persisted.library.watch);
    }

    #[tokio::test]
    async fn scan_endpoint_starts_a_progress_job() {
        use axum::{
            body::{to_bytes, Body},
            http::Request,
        };
        use sqlx::sqlite::SqlitePoolOptions;
        use std::path::PathBuf;
        use std::sync::Arc;
        use tower::ServiceExt;

        let library = tempfile::tempdir().unwrap();
        tokio::fs::write(library.path().join("Example - 01.mkv"), b"video")
            .await
            .unwrap();
        let mut settings = crate::config::Settings::default();
        settings.library.paths = vec![library.path().to_string_lossy().into_owned()];
        settings.ffmpeg.ffprobe_path = "missing-ffprobe-for-scan-job-test".into();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let app = super::router(Arc::new(crate::state::AppState::new(
            pool,
            settings,
            Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        )));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/library/scan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(payload["job_id"].as_str().is_some());
        let status = app
            .oneshot(
                Request::builder()
                    .uri("/api/library/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn optional_auth_redirects_and_login_sets_session_cookie() {
        use axum::{body::Body, http::Request};
        use sqlx::sqlite::SqlitePoolOptions;
        use std::path::PathBuf;
        use std::sync::Arc;
        use tower::ServiceExt;

        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let mut settings = crate::config::Settings::default();
        settings.auth.enabled = true;
        settings.auth.password = "secret-test-password".into();
        let app = super::router(Arc::new(crate::state::AppState::new(
            pool,
            settings,
            Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        )));

        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], "/login");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"password":"secret-test-password"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.headers().get("set-cookie").is_some());
    }

    #[tokio::test]
    async fn library_path_validation_rejects_paths_outside_configured_roots() {
        use std::path::PathBuf;

        let library = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let unicode_folder = library.path().join("Anime 日本語 & Space");
        std::fs::create_dir_all(&unicode_folder).unwrap();
        let inside_file = unicode_folder.join("第01話.mkv");
        let outside_file = outside.path().join("private.txt");
        std::fs::write(&inside_file, b"video").unwrap();
        std::fs::write(&outside_file, b"secret").unwrap();
        let mut settings = crate::config::Settings::default();
        settings.library.paths = vec![library.path().to_string_lossy().to_string()];
        let state = crate::state::AppState::new(
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            settings,
            std::sync::Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        );
        assert!(super::validate_library_path(&state, &inside_file.to_string_lossy()).is_ok());
        assert!(super::validate_library_path(&state, &outside_file.to_string_lossy()).is_err());
        #[cfg(windows)]
        assert!(inside_file.to_string_lossy().contains(":\\"));
    }

    #[tokio::test]
    async fn folder_scan_adds_and_removes_media_entries() {
        use sqlx::sqlite::SqlitePoolOptions;
        use std::path::PathBuf;

        let library = tempfile::tempdir().unwrap();
        let show = library.path().join("Example Show");
        tokio::fs::create_dir_all(&show).await.unwrap();
        let media = show.join("Example Show - 01.mkv");
        tokio::fs::write(&media, b"not a real video").await.unwrap();
        let data = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let mut settings = crate::config::Settings::default();
        settings.library.paths = vec![library.path().to_string_lossy().to_string()];
        settings.ffmpeg.ffprobe_path = "missing-ffprobe-for-test".into();
        settings.data.dir = data.path().to_string_lossy().to_string();
        let state = crate::state::AppState::new(
            pool,
            settings,
            std::sync::Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        );

        assert_eq!(super::scan_folders(&state).await.unwrap(), 1);
        let title: String = sqlx::query_scalar("SELECT title FROM anime")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(title, "Example Show");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM episode")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );

        tokio::fs::remove_file(media).await.unwrap();
        assert_eq!(super::scan_folders(&state).await.unwrap(), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM episode")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn stream_serves_partial_content_and_rejects_unsatisfiable_ranges() {
        use axum::{body::to_bytes, body::Body, http::Request};
        use sqlx::sqlite::SqlitePoolOptions;
        use std::path::PathBuf;
        use std::sync::Arc;
        use tower::ServiceExt;

        let folder = tempfile::tempdir().unwrap();
        let file_path = folder.path().join("episode.mp4");
        tokio::fs::write(&file_path, b"0123456789").await.unwrap();
        let canonical_path = file_path.canonicalize().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO anime(id,title) VALUES(1,'Test')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO episode(id,anime_id,season,number,file_path) VALUES(1,1,1,1,?)")
            .bind(canonical_path.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        let mut settings = crate::config::Settings::default();
        settings.library.paths = vec![folder.path().to_string_lossy().to_string()];
        let app = super::router(Arc::new(crate::state::AppState::new(
            pool,
            settings,
            Arc::new(crate::services::hls::HlsManager::new(PathBuf::new(), 1, 6)),
            crate::services::metadata::MetadataClient::new().unwrap(),
        )));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/stream/1")
                    .header("range", "bytes=2-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-range"], "bytes 2-5/10");
        assert_eq!(to_bytes(response.into_body(), 32).await.unwrap(), "2345");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/stream/1")
                    .header("range", "bytes=10-")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }

    #[tokio::test]
    async fn hls_reports_unavailable_when_ffmpeg_cannot_start() {
        use axum::{body::Body, http::Request};
        use sqlx::sqlite::SqlitePoolOptions;
        use std::sync::Arc;
        use tower::ServiceExt;

        let folder = tempfile::tempdir().unwrap();
        let file_path = folder.path().join("episode.mkv");
        tokio::fs::write(&file_path, b"not a real video")
            .await
            .unwrap();
        let canonical_path = file_path.canonicalize().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO anime(id,title) VALUES(1,'Test')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO episode(id,anime_id,season,number,file_path) VALUES(1,1,1,1,?)")
            .bind(canonical_path.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        let mut settings = crate::config::Settings::default();
        settings.library.paths = vec![folder.path().to_string_lossy().to_string()];
        settings.ffmpeg.ffmpeg_path = "anime-stream-missing-ffmpeg".into();
        let hls = Arc::new(crate::services::hls::HlsManager::new(
            folder.path().join("hls"),
            1,
            6,
        ));
        let app = super::router(Arc::new(crate::state::AppState::new(
            pool,
            settings,
            hls,
            crate::services::metadata::MetadataClient::new().unwrap(),
        )));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/hls/1/master.m3u8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
