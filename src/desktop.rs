//! Windows desktop integration helpers.
//!
//! The default build targets Windows 10/11 (x64) as a Tauri desktop app: the
//! axum server runs embedded inside the app process, bound to a free random
//! port on 127.0.0.1, and the WebView2 window points at that port. Auth is
//! disabled in this mode. Data lives under `%APPDATA%\FetsAnime`.
//!
//! On non-Windows hosts the same code runs in "server" mode (used for tests
//! and development on this repository's CI sandbox), keeping `cargo test`
//! green without a Windows toolchain.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

/// Default per-user data directory: `%APPDATA%\FetsAnime` on Windows,
/// `<home>/.local/share/FetsAnime` elsewhere (fallback: `./data`).
pub fn default_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("FetsAnime");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(".local").join("share").join("FetsAnime");
        }
    }
    PathBuf::from("data")
}

/// Find a free TCP port on the loopback interface by binding an ephemeral
/// port and handing it back before the server rebinds it. There is a small
/// race window, which is acceptable for a single-user local app; the caller
/// retries if binding fails.
pub fn find_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Check whether something is already listening on the given loopback port
/// (used by the single-instance guard: a running app owns the port stored in
/// `port.json` inside the data dir).
pub fn is_port_in_use(port: u16) -> bool {
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}

/// Strip the extended-length `\\?\` prefix so paths never leak it into the UI.
pub fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.into_owned()
}

#[cfg(windows)]
pub mod windows_spawn {
    //! Spawn child processes without flashing a console window.
    use std::process::Command as StdCommand;
    use tokio::process::Command;

    /// `CREATE_NO_WINDOW` (0x08000000) from the Win32 process creation flags.
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Attach the hidden-window flag to a tokio command (ffmpeg, ffprobe...).
    pub fn no_window(cmd: &mut Command) -> &mut Command {
        cmd.creation_flags(CREATE_NO_WINDOW)
    }

    /// Attach the hidden-window flag to a std command.
    #[allow(dead_code)]
    pub fn no_window_std(cmd: &mut StdCommand) -> &mut StdCommand {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW)
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn finds_a_free_port() {
        let port = find_free_port().unwrap();
        assert!(!is_port_in_use(port));
        let _listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        assert!(is_port_in_use(port));
    }

    #[test]
    fn strips_extended_length_prefixes() {
        use std::path::Path;
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Anime\Frieren - 01.mkv")),
            r"C:\Anime\Frieren - 01.mkv"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\NAS\anime\One Piece")),
            r"\\NAS\anime\One Piece"
        );
        assert_eq!(display_path(Path::new("/mnt/data/anime")), "/mnt/data/anime");
    }
}
