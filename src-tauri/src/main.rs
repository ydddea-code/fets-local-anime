#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn,sqlx=warn".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let data_dir = match handle.path().app_data_dir() {
                    Ok(path) => path,
                    Err(error) => {
                        tracing::error!(%error, "could not resolve application data directory");
                        return;
                    }
                };
                if let Ok(resource_dir) = handle.path().resource_dir() {
                    let static_dir = resource_dir.join("static");
                    if static_dir.join("app.css").is_file() {
                        std::env::set_var("FETS_STATIC_DIR", static_dir);
                    }
                    let bin_dir = resource_dir.join("bin");
                    if bin_dir.join("ffmpeg.exe").is_file() {
                        std::env::set_var("FETS_BIN_DIR", bin_dir);
                    }
                }
                match anime_stream::start_embedded_server(data_dir).await {
                    Ok(server) => {
                        let url = match server.url.parse() {
                            Ok(url) => url,
                            Err(error) => {
                                tracing::error!(%error, "invalid embedded server URL");
                                return;
                            }
                        };
                        if let Err(error) =
                            WebviewWindowBuilder::new(&handle, "main", WebviewUrl::External(url))
                                .title("Fets Anime")
                                .inner_size(1280.0, 820.0)
                                .min_inner_size(900.0, 620.0)
                                .center()
                                .build()
                        {
                            tracing::error!(%error, "could not create desktop window");
                            server.task.abort();
                            return;
                        }
                        // Keep the axum server alive for the lifetime of the window.
                        if let Err(error) = server.task.await {
                            tracing::error!(%error, "embedded server task failed");
                        }
                    }
                    Err(error) => tracing::error!(%error, "could not start embedded server"),
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Fets Anime desktop application");
}
