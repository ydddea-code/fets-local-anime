#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;
    pub type EnumWindowsProc = unsafe extern "system" fn(Hwnd, isize) -> isize;

    pub const GWL_STYLE: i32 = -16;
    pub const WS_CHILD: usize = 0x0000_0010;
    pub const WS_VISIBLE: usize = 0x1000_0000;
    pub const WS_CAPTION: usize = 0x00C0_0000;
    pub const WS_THICKFRAME: usize = 0x0004_0000;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_SHOWWINDOW: u32 = 0x0040;

    #[repr(C)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    extern "system" {
        pub fn EnumWindows(lp_enum_func: EnumWindowsProc, lparam: isize) -> i32;
        pub fn GetWindowThreadProcessId(hwnd: Hwnd, lpdw_process_id: *mut u32) -> u32;
        pub fn IsWindowVisible(hwnd: Hwnd) -> i32;
        pub fn SetParent(hwnd_child: Hwnd, hwnd_new_parent: Hwnd) -> Hwnd;
        pub fn GetWindowLongPtrW(hwnd: Hwnd, gn_index: i32) -> isize;
        pub fn SetWindowLongPtrW(hwnd: Hwnd, gn_index: i32, dw_new_long: isize) -> isize;
        pub fn GetClientRect(hwnd: Hwnd, lp_rect: *mut Rect) -> i32;
        pub fn MoveWindow(
            hwnd: Hwnd,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            repaint: i32,
        ) -> i32;
        pub fn SetWindowPos(
            hwnd: Hwnd,
            hwnd_insert_after: Hwnd,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> i32;
    }
}

/// Windows-only: attach mpv's freshly created video window to the app frame.
/// mpv is launched without `--wid` (which rendered into a stale webview child
/// and produced a black screen); instead we locate its new top-level window by
/// OS process id and reparent it with the Win32 API.
#[cfg(windows)]
fn embed_mpv_window(pid: u32, parent: tauri::WebviewWindow) {
    use win32::*;

    unsafe extern "system" enum_windows_proc(hwnd: Hwnd, lparam: isize) -> isize {
        let state = &mut *(lparam as *mut (u32, Option<Hwnd>));
        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id == state.0 && !hwnd.is_null() && IsWindowVisible(hwnd) != 0 {
            state.1 = Some(hwnd);
            return 0; // stop enumerating
        }
        1
    }

    std::thread::spawn(move || {
        // mpv creates its window a moment after launch; poll for up to ~10s.
        for _ in 0..40 {
            let mut state = (pid, None::<Hwnd>);
            unsafe {
                EnumWindows(Some(enum_windows_proc), &mut state as *mut _ as isize);
            }
            let Some(hwnd) = state.1 else {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue;
            };
            let Ok(inner) = parent.inner_size() else {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue;
            };
            let Ok(parent_hwnd) = parent.hwnd() else {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue;
            };
            unsafe {
                // Turn mpv's window into a borderless child filling the frame.
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as usize;
                SetWindowLongPtrW(
                    hwnd,
                    GWL_STYLE,
                    ((style & !(WS_CAPTION | WS_THICKFRAME)) | WS_CHILD | WS_VISIBLE) as isize,
                );
                SetParent(hwnd, parent_hwnd.0 as Hwnd);
                MoveWindow(hwnd, 0, 0, inner.width as i32, inner.height as i32, 1);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    inner.width as i32,
                    inner.height as i32,
                    SWP_NOZORDER | SWP_SHOWWINDOW,
                );
            }
            tracing::info!(pid, "mpv window embedded into Fets Anime frame");
            return;
        }
        tracing::warn!(pid, "mpv window not found; playback continues standalone");
    });
}

/// Keeps the embedded mpv window in sync with the app frame while resizing.
#[cfg(windows)]
fn attach_resize_sync(window: tauri::WebviewWindow) {
    use win32::*;

    let on_window_event = move |event: tauri::Event| {
        if !matches!(event, tauri::WindowEvent::Resized(_)) {
            return;
        }
        let pid = match anime_stream::active_mpv_pid() {
            Some(pid) => pid,
            None => return,
        };
        let Ok(inner) = window.inner_size() else { return };
        let mut state = (pid, None::<Hwnd>);
        unsafe extern "system" fn proc(hwnd: Hwnd, lparam: isize) -> isize {
            let state = &mut *(lparam as *mut (u32, Option<Hwnd>));
            let mut process_id = 0u32;
            GetWindowThreadProcessId(hwnd, &mut process_id);
            if process_id == state.0 && !hwnd.is_null() {
                state.1 = Some(hwnd);
                return 0;
            }
            1
        }
        unsafe {
            EnumWindows(Some(proc), &mut state as *mut _ as isize);
        }
        if let Some(hwnd) = state.1 {
            unsafe {
                MoveWindow(hwnd, 0, 0, inner.width as i32, inner.height as i32, 1);
            }
        }
    };
    if let Err(error) = window.on_window_event(on_window_event) {
        tracing::warn!(%error, "could not listen for resize events");
    }
}

#[cfg(not(windows))]
fn embed_mpv_window(_pid: u32, _parent: tauri::WebviewWindow) {}

#[cfg(not(windows))]
fn attach_resize_sync(_window: tauri::WebviewWindow) {}

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
                        let window = match WebviewWindowBuilder::new(
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
                        // Register the hook that reparents freshly launched mpv
                        // windows into this frame (fixes "video tidak terputar").
                        let embed_target = window.clone();
                        server.state.set_mpv_launch_listener(Box::new(move |_id, pid, _episode| {
                            embed_mpv_window(pid, embed_target.clone());
                        }));
                        attach_resize_sync(window.clone());
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
