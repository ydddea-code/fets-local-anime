mod config;
mod desktop;
mod error;
mod routes;
mod services;
mod state;

use crate::config::Settings;
use crate::services::hls::HlsManager;
use crate::services::metadata::MetadataClient;
use crate::state::AppState;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn,sqlx=warn".into()),
        )
        .init();

    let data_dir = desktop::data_dir();
    let settings = Settings::load_desktop(&data_dir);
    ensure_layout(&data_dir)?;

    let database_path = data_dir.join("app.db");
    let options = SqliteConnectOptions::from_str(&database_path.to_string_lossy())?
        .create_if_missing(true)
        .foreign_keys(false)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let hls = Arc::new(HlsManager::new(
        data_dir.join("hls"),
        settings.ffmpeg.max_concurrent_sessions,
        settings.ffmpeg.hls_segment_seconds,
    ));
    let metadata = MetadataClient::new()?;
    let state = Arc::new(AppState::new(pool, settings, hls, metadata));
    let app = routes::router(Arc::clone(&state));

    if state.settings.read().expect("settings lock").library.watch {
        tokio::spawn(services::watcher::watch_libraries(Arc::clone(&state)));
    }
    routes::scan_startup(&state).await.ok();

    // Loopback-only HTTP server on a free port (8010..8030 by default).
    let listener = tokio::net::TcpListener::bind((
        state.settings.read().expect("settings lock").server.host.as_str(),
        state.settings.read().expect("settings lock").server.port,
    ))
    .await?;
    tracing::info!("Fets Anime ready at http://127.0.0.1:{}", state.settings.read().expect("settings lock").server.port);
    axum::serve(listener, app).await?;
    Ok(())
}

fn ensure_layout(data_dir: &Path) -> anyhow::Result<()> {
    for directory in ["thumbs", "subtitles", "hls"] {
        std::fs::create_dir_all(data_dir.join(directory))?;
    }
    Ok(())
}
