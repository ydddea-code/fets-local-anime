mod config;
mod error;
mod routes;
mod services;
mod state;

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let settings = config::Settings::load()?;
    tokio::fs::create_dir_all(&settings.data.dir).await?;
    let db_path = PathBuf::from(&settings.data.dir).join("app.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("connect to SQLite")?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    if settings.server.host == "0.0.0.0" && !settings.auth.enabled {
        tracing::warn!("server binds to all interfaces without authentication");
    }
    let bind: SocketAddr = format!("{}:{}", settings.server.host, settings.server.port)
        .parse()
        .context("invalid server host/port")?;
    let hls = Arc::new(services::hls::HlsManager::new(
        PathBuf::from(&settings.data.dir).join("hls"),
        settings.ffmpeg.max_concurrent_transcodes,
        settings.ffmpeg.hls_segment_seconds,
    ));
    let cleanup_hls = Arc::clone(&hls);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            cleanup_hls.cleanup_idle(Duration::from_secs(120)).await;
        }
    });
    let state = Arc::new(state::AppState {
        pool,
        settings,
        hls,
        metadata: services::metadata::MetadataClient::new()?,
        sessions: Arc::default(),
    });
    if state.settings.library.scan_on_startup {
        routes::scan_startup(&state).await?;
    }
    if state.settings.library.watch {
        tokio::spawn(services::watcher::watch_libraries(Arc::clone(&state)));
    }
    let app = routes::router(state);
    let listener = TcpListener::bind(bind).await?;
    info!(%bind, "anime-stream is listening");
    axum::serve(listener, app).await?;
    Ok(())
}
