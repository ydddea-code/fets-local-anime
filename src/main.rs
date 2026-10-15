#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anime_stream::start_embedded_server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn,sqlx=warn".into()),
        )
        .init();

    let data_dir = anime_stream::desktop::data_dir();
    let server = start_embedded_server(data_dir).await?;
    tracing::info!("Fets Anime ready at {}", server.url);
    server.task.await??;
    Ok(())
}
