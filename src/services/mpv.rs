//! mpv integration: executable discovery, argument building, and a JSON IPC
//! client over a Windows named pipe (or Unix socket) so playback progress in
//! mpv is written back to the database ("lanjutkan menonton" keeps working).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Locate mpv.exe. Order: explicit setting, bundled sidecar next to the
/// executable, PATH, then common install locations (Program Files, scoop,
/// chocolatey, winget links). Returns `None` when nothing is found — the
/// caller shows a dialog instead of crashing.
pub fn find_mpv(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured.filter(|p| !p.trim().is_empty()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(sidecar) = sidecar_candidate("mpv.exe") {
        return Some(sidecar);
    }
    if let Some(from_path) = which("mpv") {
        return Some(from_path);
    }
    candidates().into_iter().find(|p| p.is_file())
}

/// Same lookup order for ffmpeg/ffprobe with an extra fallback to bare names
/// on PATH (classic server mode keeps working without bundled binaries).
pub fn find_tool(configured: &str, exe_name: &str) -> String {
    let bare = exe_name.trim_end_matches(".exe");
    let trimmed = configured.trim();
    if !trimmed.is_empty() && trimmed != bare {
        let path = PathBuf::from(trimmed);
        if path.is_file() {
            return trimmed.to_string();
        }
    }
    if let Some(sidecar) = sidecar_candidate(exe_name) {
        return sidecar.to_string_lossy().to_string();
    }
    configured.to_string()
}

fn sidecar_candidate(exe_name: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FETS_BIN_DIR") {
        let candidate = PathBuf::from(dir).join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join(exe_name);
    candidate.is_file().then_some(candidate)
}

fn which(name: &str) -> Option<PathBuf> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let file = dir.join(format!("{name}{suffix}"));
        if file.is_file() {
            return Some(file);
        }
    }
    None
}

fn candidates() -> Vec<PathBuf> {
    let mut list = Vec::new();
    #[cfg(windows)]
    {
        list.push(PathBuf::from(r"C:\Program Files\mpv\mpv.exe"));
        list.push(PathBuf::from(r"C:\Program Files (x86)\mpv\mpv.exe"));
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let profile = PathBuf::from(profile);
            list.push(profile.join("scoop/apps/mpv/current/mpv.exe"));
            list.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin\mpv.exe"));
            list.push(profile.join("AppData/Local/Microsoft/WinGet/Links/mpv.exe"));
        }
    }
    #[cfg(not(windows))]
    {
        list.push(PathBuf::from("/usr/bin/mpv"));
        list.push(PathBuf::from("/usr/local/bin/mpv"));
    }
    list
}

/// Unique mpv IPC pipe name for one launched session. On Windows this is a
/// named pipe; elsewhere a unix socket path with a guaranteed-safe length.
pub fn ipc_pipe(id: &str) -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\fets-anime-mpv-{id}")
    }
    #[cfg(not(windows))]
    {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in id.as_bytes() {
            hash = (hash ^ *byte).wrapping_mul(0x100000001b3);
        }
        format!("/tmp/fets-mpv-{:016x}.sock", hash)
    }
}

/// Build the fixed mpv argument list for one episode. The input path must be
/// a server-validated canonical path, never something the client chose.
pub fn build_args(pipe: &str, start_sec: f64, file: &Path) -> Vec<String> {
    build_args_with_parent(pipe, start_sec, file, None)
}

/// Build arguments for mpv embedded in a native parent window when available.
pub fn build_args_with_parent(
    pipe: &str,
    start_sec: f64,
    file: &Path,
    parent_hwnd: Option<u32>,
) -> Vec<String> {
    let _ = parent_hwnd; // Reparenting happens after launch via SetParent.
    let start = if start_sec.is_finite() && start_sec > 0.5 {
        format!("--start={start_sec:.3}")
    } else {
        "--start=0".to_string()
    };
    vec![
        // Always show a real video window; mpv used to "play" invisibly for
        // some files which looked like nothing was happening.
        "--force-window=immediate".into(),
        "--keep-open=yes".into(),
        // Idle mode keeps the IPC server alive so progress reporting never
        // dies mid-session.
        "--idle=once".into(),
        // Borderless: the desktop shell reparents this window into the app.
        "--border=no".into(),
        start,
        "--sub-auto=fuzzy".into(),
        format!("--input-ipc-server={pipe}"),
        "--no-terminal".into(),
        file.to_string_lossy().into_owned(),
    ]
}

/// Convert an mpv property-change event into a database progress update.
/// `time-pos` yields the current position; `eof-reached=true` marks the
/// episode as watched. Other events (pause, idle...) are ignored here.
pub fn progress_from_event(event: &Value) -> Option<(f64, bool)> {
    if event
        .get("event")
        .or_else(|| event.get("event_id"))
        .and_then(Value::as_str)?
        != "property-change"
    {
        return None;
    }
    let data = event.get("data")?;
    let value = data.get("value").unwrap_or(data);
    match event.get("name").and_then(Value::as_str)? {
        "time-pos" => Some((value.as_f64()?, false)),
        "eof-reached" => value
            .as_bool()
            .filter(|reached| *reached)
            .map(|_| (0.0, true)),
        _ => None,
    }
}

/// A running mpv child. Dropping the session kills the process, so the app
/// can never leave orphan mpv instances behind.
pub struct MpvSession {
    pub id: String,
    child: Child,
}

impl MpvSession {
    /// Spawn mpv with its own visible window (unlike ffmpeg it must show UI).
    /// Uses std::process so no console is ever allocated on Windows, and
    /// redirects stdio so nothing can block or flash. Returns both the tokio
    /// child handle (for waiting/kill_on_drop) and the raw OS pid, which the
    /// desktop shell can use to reparent mpv's window into the app frame.
    pub fn spawn(mpv_path: &Path, id: &str, args: &[String]) -> Result<(Self, u32), String> {
        let std_child = std::process::Command::new(mpv_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start mpv: {e}"))?;
        let os_pid = std_child.id();
        let mut child = Child::from_std(std_child, &tokio::runtime::Handle::current());
        child.kill_on_drop(true);
        if child.id().is_none() {
            return Err("mpv process exited immediately after launch".to_string());
        }
        Ok((
            Self {
                id: id.to_string(),
                child,
            },
            os_pid,
        ))
    }

    /// Take ownership of the child handle (used by the IPC watcher task).
    pub fn into_child(self) -> Child {
        self.child
    }
}

/// Watch an mpv IPC stream line by line and report progress updates. Shared
/// between the Windows named-pipe backend and tests.
pub async fn consume_ipc_lines<R>(
    reader: R,
    mut on_progress: impl FnMut(f64, bool),
) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            if let Some((position, watched)) = progress_from_event(&event) {
                on_progress(position, watched);
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub mod pipe_io {
    //! Blocking named-pipe access wrapped in `spawn_blocking` so the async
    //! runtime is never starved. mpv creates the pipe shortly after startup,
    //! hence the retry loop.

    use std::io;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_SEQUENTIAL_SCAN,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    pub struct PipeHandle {
        handle: HANDLE,
        buffered: std::sync::Mutex<Vec<u8>>,
    }

    // Windows pipe handles may be used across threads; the read buffer is
    // mutex-protected and the OS serializes operations on the handle.
    unsafe impl Send for PipeHandle {}
    unsafe impl Sync for PipeHandle {}

    impl PipeHandle {
        /// Open the mpv IPC pipe, waiting up to `retries` attempts for mpv
        /// to create it.
        pub fn open(pipe_path: &str, retries: u32) -> io::Result<Self> {
            let wide: Vec<u16> = pipe_path.encode_utf16().chain(std::iter::once(0)).collect();
            let mut last_error = io::Error::other("mpv pipe was never created");
            for attempt in 0..retries.max(1) {
                let handle = unsafe {
                    CreateFileW(
                        wide.as_ptr(),
                        GENERIC_READ | GENERIC_WRITE,
                        0,
                        std::ptr::null_mut(),
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN,
                        std::ptr::null_mut(),
                    )
                };
                if handle != INVALID_HANDLE_VALUE {
                    return Ok(Self {
                        handle,
                        buffered: std::sync::Mutex::new(Vec::new()),
                    });
                }
                last_error = io::Error::last_os_error();
                // ERROR_PIPE_BUSY / NOT_FOUND: wait briefly then retry.
                unsafe {
                    WaitNamedPipeW(wide.as_ptr(), 500);
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                let _ = attempt;
            }
            Err(last_error)
        }

        pub fn write_line(&self, payload: &str) -> io::Result<()> {
            let bytes = format!("{payload}\n").into_bytes();
            let mut written = 0u32;
            let ok = unsafe {
                WriteFile(
                    self.handle,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Read one newline-delimited JSON line. Returns empty string at EOF
        /// (mpv closed the pipe => the player window was closed).
        pub fn read_line(&self) -> io::Result<String> {
            let mut buffer = self
                .buffered
                .lock()
                .map_err(|_| io::Error::other("mpv pipe buffer lock poisoned"))?;
            let mut chunk = [0u8; 4096];
            loop {
                if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line = buffer.drain(..=newline).collect::<Vec<_>>();
                    return Ok(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
                }
                let mut read = 0u32;
                let ok = unsafe {
                    ReadFile(
                        self.handle,
                        chunk.as_mut_ptr(),
                        chunk.len() as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || read == 0 {
                    return Ok(String::from_utf8_lossy(&buffer).into_owned());
                }
                buffer.extend_from_slice(&chunk[..read as usize]);
                if buffer.len() > 4 * 1024 * 1024 {
                    buffer.clear(); // runaway guard against malformed streams
                }
            }
        }
    }

    impl Drop for PipeHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(windows))]
pub mod pipe_io {
    //! Non-Windows IPC backend: mpv listens on a unix socket; we connect to
    //! it with the same line-oriented JSON protocol used over named pipes.

    use std::io;
    use tokio::net::UnixStream;
    use tokio::sync::Mutex;

    pub struct PipeHandle {
        stream: Mutex<UnixStream>,
    }

    impl PipeHandle {
        /// Connect to mpv's IPC socket, retrying until mpv has created it.
        pub async fn open(pipe_path: &str, retries: u32) -> io::Result<Self> {
            let mut last_error = io::Error::other("mpv socket was never created");
            for _ in 0..retries.max(1) {
                match UnixStream::connect(pipe_path).await {
                    Ok(stream) => return Ok(Self { stream: Mutex::new(stream) }),
                    Err(error) => last_error = error,
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(last_error)
        }

        pub async fn write_line(&self, payload: &str) -> io::Result<()> {
            use tokio::io::AsyncWriteExt;
            let mut stream = self.stream.lock().await;
            stream.write_all(payload.as_bytes()).await?;
            stream.write_all(b"\n").await?;
            stream.flush().await
        }

        /// Read one newline-delimited JSON line. Empty string means mpv
        /// closed the connection (the player window was closed).
        pub async fn read_line(&self) -> io::Result<String> {
            use tokio::io::AsyncBufReadExt;
            let mut stream = self.stream.lock().await;
            let mut reader = (&mut *stream);
            let mut line = String::new();
            let count = tokio::io::BufReader::new(&mut reader)
                .read_line(&mut line)
                .await?;
            if count == 0 {
                return Ok(String::new());
            }
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            Ok(line)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_expected_arguments() {
        let args = build_args(
            r"\\.\pipe\fets-anime-mpv-abc",
            123.456,
            Path::new(r"D:\Anime\Frieren\Frieren - 01.mkv"),
        );
        assert_eq!(args[0], "--force-window=immediate");
        assert_eq!(args[1], "--keep-open=yes");
        assert_eq!(args[2], "--idle=once");
        assert_eq!(args[3], "--border=no");
        assert_eq!(args[4], "--start=123.456");
        assert_eq!(args[5], "--sub-auto=fuzzy");
        assert_eq!(args[6], r"--input-ipc-server=\\.\pipe\fets-anime-mpv-abc");
        assert_eq!(args.last().unwrap(), r"D:\Anime\Frieren\Frieren - 01.mkv");
    }

    #[test]
    fn embedded_parent_hwnd_no_longer_uses_wid() {
        // --wid was the source of "video tidak terputar": mpv rendered into
        // a stale webview child window. Reparenting now happens after launch.
        let args = build_args_with_parent("pipe", 0.0, Path::new("x.mkv"), Some(12345));
        assert!(!args.iter().any(|a| a.starts_with("--wid=")));
    }

    #[test]
    fn zero_start_is_omitted_and_nan_is_safe() {
        let args = build_args("pipe", 0.0, Path::new("x.mkv"));
        assert!(args.iter().any(|a| a == "--start=0"));
        let args = build_args("pipe", f64::NAN, Path::new("x.mkv"));
        assert!(args.iter().any(|a| a == "--start=0"));
    }

    #[test]
    fn paths_with_spaces_and_unicode_pass_through() {
        let file = Path::new("/tmp/【Group】呪術廻戦 - S01E01 [1080p].mkv");
        let args = build_args("pipe", 10.0, file);
        assert_eq!(args.last().unwrap(), file.to_str().unwrap());
    }

    #[test]
    fn parses_time_pos_and_eof_events() {
        let time = serde_json::json!({"event_id":"property-change","name":"time-pos","data":{"value":42.5}});
        assert_eq!(progress_from_event(&time), Some((42.5, false)));
        let mpv_ipc =
            serde_json::json!({"event":"property-change","id":1,"name":"time-pos","data":17.25});
        assert_eq!(progress_from_event(&mpv_ipc), Some((17.25, false)));
        let eof = serde_json::json!({"event_id":"property-change","name":"eof-reached","data":{"value":true}});
        assert_eq!(progress_from_event(&eof), Some((0.0, true)));
        let not_eof = serde_json::json!({"event_id":"property-change","name":"eof-reached","data":{"value":false}});
        assert_eq!(progress_from_event(&not_eof), None);
        let pause =
            serde_json::json!({"event_id":"property-change","name":"pause","data":{"value":true}});
        assert_eq!(progress_from_event(&pause), None);
        let legacy =
            serde_json::json!({"event_id":"property-change","name":"time-pos","data":99.0});
        assert_eq!(progress_from_event(&legacy), Some((99.0, false)));
        let start_event = serde_json::json!({"event_id":"start"});
        assert_eq!(progress_from_event(&start_event), None);
    }

    #[test]
    fn ipc_pipe_name_format() {
        assert_eq!(ipc_pipe("xyz123"), r"\\.\pipe\fets-anime-mpv-xyz123");
    }

    #[tokio::test]
    async fn consumes_ipc_lines_and_reports_progress() {
        let stream: &[u8] = br#"{"event_id":"start"}
{"event_id":"property-change","name":"time-pos","data":{"value":12.5}}
{"event_id":"property-change","name":"pause","data":{"value":false}}
{"event_id":"property-change","name":"time-pos","data":{"value":30.0}}
{"event_id":"property-change","name":"eof-reached","data":{"value":true}}
"#;
        let mut seen = Vec::new();
        consume_ipc_lines(stream, |position, watched| seen.push((position, watched)))
            .await
            .unwrap();
        assert_eq!(seen, vec![(12.5, false), (30.0, false), (0.0, true)]);
    }
}
