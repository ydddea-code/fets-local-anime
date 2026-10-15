//! Desktop shell support: per-user data directory, loopback server binding,
//! single-instance guard, and Windows path niceties. The native window is
//! provided by Tauri (see `src/bin/fets-anime-desktop.rs` + src-tauri/); this
//! module keeps the pure logic testable and cross-platform.

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// `%APPDATA%\fets-anime` on Windows, XDG data dir elsewhere. Falls back to
/// `./data` only when no user profile variable exists (CI containers).
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local").join("share"))
        });
    let dir = base
        .map(|base| base.join("fets-anime"))
        .unwrap_or_else(|| PathBuf::from("./data"));
    let _ = std::fs::create_dir_all(&dir);
    dir.canonicalize().unwrap_or(dir)
}

/// Strip the extended-length `\\?\` prefix that canonicalize() adds on
/// Windows so paths look natural in the UI and mpv command lines.
pub fn display_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .trim_start_matches(r"\\.\")
        .to_string()
}

/// Bind a listener on a free port of the requested range; used both to find
/// a port for the embedded server and as the single-instance guard.
pub fn bind_free_port(host: &str, first: u16, last: u16) -> Option<TcpListener> {
    (first..=last).find_map(|port| {
        TcpListener::bind((host, port)).ok()
    })
}

/// True when a Fets Anime server already answers on one of our ports — i.e.
/// another instance is running and the shell should just navigate there.
pub fn existing_instance(first: u16, last: u16) -> Option<u16> {
    (first..=last).find(|port| health_probe(("127.0.0.1", *port)))
}

fn health_probe(addr: impl Into<SocketAddr> + Copy) -> bool {
    let socket: SocketAddr = match addr.into() {
        ok => ok,
        #[cfg(unix)]
        Err(_) => return false,
        #[cfg(not(unix))]
        Err(_) => return false,
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&socket, Duration::from_millis(400)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buffer = [0u8; 256];
    match stream.read(&mut buffer) {
        Ok(count) => String::from_utf8_lossy(&buffer[..count]).contains("200 OK"),
        Err(_) => false,
    }
}

use std::io::Write;

/// Windows-only spawn helpers: ffmpeg/ffprobe must never flash a console
/// window inside the desktop app (mpv intentionally shows its own window).
#[cfg(windows)]
pub mod windows_spawn {
    use std::os::windows::process::CommandExt;

    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn no_window(command: &mut tokio::process::Command) {
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_strips_extended_prefix() {
        assert_eq!(
            display_path(std::path::Path::new(r"\\?\C:\Anime\Frieren - 01.mkv")),
            r"C:\Anime\Frieren - 01.mkv"
        );
        assert_eq!(
            display_path(std::path::Path::new("/home/user/anime.mkv")),
            "/home/user/anime.mkv"
        );
    }

    #[test]
    fn binds_first_free_port_in_range() {
        let listener = bind_free_port("127.0.0.1", 21000, 21010).expect("free port");
        let port = listener.local_addr().unwrap().port();
        assert!((21000..=21010).contains(&port));
        // While held, a second bind must skip to a different port.
        let second = bind_free_port("127.0.0.1", port, port).is_none();
        assert!(second, "occupied port cannot be rebound");
    }

    #[test]
    fn no_existing_instance_on_unused_ports() {
        assert!(existing_instance(23900, 23905).is_none());
    }

    #[test]
    fn detects_running_health_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
                let _ = stream.flush();
            }
        });
        assert_eq!(existing_instance(port, port), Some(port));
    }
}
