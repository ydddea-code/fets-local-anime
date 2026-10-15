pub mod config;
pub mod desktop;
pub mod error;
pub mod routes;
pub mod services;
pub mod state;

use services::{hls::HlsManager, metadata::MetadataClient};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::{path::PathBuf, str::FromStr, sync::Arc};
use tokio::{net::TcpListener, task::JoinHandle};

pub struct EmbeddedServer {
    pub url: String,
    pub task: JoinHandle<anyhow::Result<()>>,
}

/// Start the local-only backend for the desktop shell on an OS-selected port.
pub async fn start_embedded_server(data_dir: PathBuf) -> anyhow::Result<EmbeddedServer> {
    std::fs::create_dir_all(&data_dir)?;
    for directory in ["thumbs", "subtitles", "hls", "covers"] {
        std::fs::create_dir_all(data_dir.join(directory))?;
    }

    let settings = config::Settings::load_desktop(&data_dir);
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
    let state = Arc::new(state::AppState::new(pool, settings, hls, metadata));
    let app = routes::router(Arc::clone(&state));

    if state.settings_snapshot().library.watch {
        tokio::spawn(services::watcher::watch_libraries(Arc::clone(&state)));
    }
    let _ = routes::start_scan(Arc::clone(&state));

    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let url = format!("http://127.0.0.1:{}", address.port());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await?;
        Ok(())
    });
    Ok(EmbeddedServer { url, task })
}
