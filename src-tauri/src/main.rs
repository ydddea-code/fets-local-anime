#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Windows-only: attach mpv's freshly created video window to the app frame.
#[cfg(windows)]
fn embed_mpv_window(pid: u32, parent: tauri::Window) {
    use windows_rs::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SetParent};

    std::thread::spawn(move || {
        // mpv creates its window a moment after launch; poll for up to ~10s.
        for _ in 0..40 {
            let mut found = None::<isize>;
            unsafe {
                EnumWindows(
                    Some(|hwnd, lparam| {
                        let mut process_id = 0u32;
                        GetWindowThreadProcessId(hwnd, &mut process_id);
                        if process_id == *(lparam as *const u32) && hwnd != 0 {
                            found = Some(hwnd);
                            0 // stop enumerating
                        } else {
                            1
                        }
                    }),
                    &pid as *const u32 as isize,
                );
            }
            if let Some(hwnd) = found.filter(|_| unsafe { IsWindowVisible(hwnd) } != 0) {
                let Ok(child_rect) = get_window_rect(hwnd) else {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    continue;
                };
                if child_rect.is_empty() {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    continue;
                }
                if let (Ok(scale_factor), Ok(inner)) = (parent.scale_factor(), parent.inner_size())
                {
                    let width = inner.width as f64 / scale_factor;
                    let height = inner.height as f64 / scale_factor;
                    unsafe {
                        SetParent(hwnd, parent.hwnd().map(|h| h.0 as isize).unwrap_or(0));
                        set_window_pos_fill(hwnd, width as i32, height as i32);
                    }
                    tracing::info!(pid, "mpv window embedded into Fets Anime frame");
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        tracing::warn!(pid, "mpv window not found; playback continues standalone");
    });
}

#[cfg(windows)]
fn get_window_rect(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    extern "system" {
        fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    }
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    Some((rect.left, rect.top, rect.right, rect.bottom))
}

#[cfg(windows)]
unsafe fn set_window_pos_fill(hwnd: isize, width: i32, height: i32) {
    extern "system" {
        fn MoveWindow(
            hwnd: isize,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            repaint: i32,
        ) -> i32;
    }
    MoveWindow(hwnd, 0, 0, width, height, 1);
}

#[cfg(not(windows))]
fn embed_mpv_window(_pid: u32, _parent: tauri::Window) {}
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
                        let _window = match WebviewWindowBuilder::new(
                            &handle,
                            "main",
                            WebviewUrl::External(url),
                        )
                        .title("Fets Anime")
                        .inner_size(1280.0, 820.0)
                        .min_inner_size(900.0, 620.0)
                        .center()
                        .build()
                        {
                            Ok(window) => window,
                            Err(error) => {
                                tracing::error!(%error, "could not create desktop window");
                                server.task.abort();
                                return;
                            }
                        };
                        #[cfg(target_os = "windows")]
                        match _window.hwnd() {
                            Ok(hwnd) => {
                                let id = hwnd.0 as usize as u32;
                                std::env::set_var("FETS_MPV_PARENT_HWND", id.to_string());
                            }
                            Err(error) => tracing::warn!(%error, "mpv will use a separate window"),
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
