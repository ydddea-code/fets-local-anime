use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub library: LibraryConfig,
    pub ffmpeg: FfmpegConfig,
    pub data: DataConfig,
    pub metadata: MetadataConfig,
    pub auth: AuthConfig,
    pub desktop: DesktopConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LibraryConfig {
    pub paths: Vec<String>,
    pub extensions: Vec<String>,
    /// Watch the configured folders for file changes and rescan automatically.
    pub watch: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FfmpegConfig {
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub hls_segment_seconds: u32,
    pub max_concurrent_sessions: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DataConfig {
    pub dir: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MetadataConfig {
    pub anilist_enabled: bool,
    pub cache_ttl_hours: i64,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Optional password protection; only meaningful in `server-mode`. The
    /// desktop app ships with this disabled (local-only loopback server).
    pub enabled: bool,
    pub password: String,
}

/// Desktop-shell settings, editable from the Pengaturan page and persisted
/// to `settings.toml` in the data directory on every change.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DesktopConfig {
    /// "auto" | "html5" | "mpv"
    pub player: String,
    /// Empty = auto-discover mpv.exe (sidecar, PATH, common install dirs).
    pub mpv_path: String,
    /// "dark" | "light" | "system"
    pub theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 8010,
            },
            library: LibraryConfig {
                paths: vec![default_library_dir()],
                extensions: vec![
                    "mkv".into(),
                    "mp4".into(),
                    "webm".into(),
                    "avi".into(),
                    "m4v".into(),
                ],
                watch: true,
            },
            ffmpeg: FfmpegConfig {
                ffmpeg_path: if cfg!(windows) {
                    "ffmpeg.exe".into()
                } else {
                    "ffmpeg".into()
                },
                ffprobe_path: if cfg!(windows) {
                    "ffprobe.exe".into()
                } else {
                    "ffprobe".into()
                },
                hls_segment_seconds: 6,
                max_concurrent_sessions: 2,
            },
            data: DataConfig {
                dir: default_data_dir(),
            },
            metadata: MetadataConfig {
                anilist_enabled: true,
                cache_ttl_hours: 168,
                timeout_secs: 10,
            },
            auth: AuthConfig::default(),
            desktop: DesktopConfig::default(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8010,
        }
    }
}
impl Default for LibraryConfig {
    fn default() -> Self {
        Config::default().library
    }
}
impl Default for FfmpegConfig {
    fn default() -> Self {
        Config::default().ffmpeg
    }
}
impl Default for DataConfig {
    fn default() -> Self {
        Config::default().data
    }
}
impl Default for MetadataConfig {
    fn default() -> Self {
        Config::default().metadata
    }
}
impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            player: "auto".into(),
            mpv_path: String::new(),
            theme: "dark".into(),
        }
    }
}

fn default_data_dir() -> String {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from);
    #[cfg(not(windows))]
    let base = directories_hint();

    #[cfg(not(windows))]
    fn directories_hint() -> Option<std::path::PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".local/share"))
            })
    }

    base.map(|dir| dir.join("FetsAnime").to_string_lossy().into_owned())
        .unwrap_or_else(|| "./data".into())
}

fn default_library_dir() -> String {
    #[cfg(windows)]
    let profile = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let profile = std::env::var_os("HOME");
    profile
        .map(|p| {
            std::path::PathBuf::from(p)
                .join("Videos/Anime")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "./library".into())
}

pub type Settings = Config;

impl Settings {
    /// Load the classic layered config (config.toml + environment). Used by
    /// the optional `server-mode` build.
    pub fn load() -> anyhow::Result<Self> {
        let builder = config::Environment::with_prefix("ANIME_STREAM")
            .separator("__")
            .try_parsing(true);
        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config"))
            .add_source(builder)
            .build()?;
        Ok(cfg.try_deserialize()?)
    }

    /// Desktop mode: everything lives under %APPDATA%\fets-anime (or the
    /// XDG equivalent). A pre-existing settings.toml overrides defaults;
    /// otherwise one is created so users can see where configuration lives.
    pub fn load_desktop(data_dir: &Path) -> Self {
        let _ = fs::create_dir_all(data_dir);
        let path = data_dir.join("settings.toml");
        let mut settings = match fs::read_to_string(&path) {
            Ok(contents) => toml::from_str::<Settings>(&contents).unwrap_or_default(),
            Err(_) => Settings::default(),
        };
        // Desktop app is single-user local software: never require login.
        settings.auth.enabled = false;
        settings.data.dir = data_dir.to_string_lossy().into_owned();
        if !path.exists() {
            let _ = settings.save();
        }
        settings
    }

    /// Persist the current settings next to the database (settings.toml).
    pub fn save(&self) -> Result<(), AppError> {
        let path = Path::new(&self.data.dir).join("settings.toml");
        let text = toml::to_string_pretty(self)
            .map_err(|e| crate::error::AppError::Internal(e.to_string()))?;
        fs::write(path, text).map_err(|e| crate::error::AppError::Internal(e.to_string()))
    }
}

use crate::error::AppError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_toml() {
        let settings: Settings = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 9000
[library]
paths = ["/anime"]
extensions = ["mkv", "mp4"]
watch = false
[auth]
enabled = true
password = "secret"
"#,
        )
        .unwrap();
        assert_eq!(settings.server.port, 9000);
        assert_eq!(settings.library.paths, vec!["/anime"]);
        assert!(!settings.library.watch);
        assert!(settings.auth.enabled);
    }

    #[test]
    fn desktop_defaults_disable_auth_and_enable_watcher() {
        let settings = Settings::default();
        assert!(!settings.auth.enabled);
        assert!(settings.library.watch);
        assert_eq!(settings.desktop.player, "auto");
        assert_eq!(settings.desktop.theme, "dark");
    }

    #[test]
    fn load_desktop_creates_settings_file_and_forces_auth_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut seeded = Settings::default();
        seeded.auth.enabled = true;
        seeded.auth.password = "x".into();
        seeded.desktop.player = "mpv".into();
        std::fs::write(
            dir.path().join("settings.toml"),
            toml::to_string_pretty(&seeded).unwrap(),
        )
        .unwrap();
        let settings = Settings::load_desktop(dir.path());
        assert!(!settings.auth.enabled);
        assert_eq!(settings.desktop.player, "mpv");
        assert_eq!(settings.data.dir, dir.path().to_string_lossy());
    }

    #[test]
    fn save_roundtrips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::load_desktop(dir.path());
        settings
            .library
            .paths
            .push(dir.path().join("Anime").to_string_lossy().into_owned());
        settings.desktop.theme = "light".into();
        settings.save().unwrap();
        let reloaded = Settings::load_desktop(dir.path());
        assert_eq!(reloaded.library.paths.len(), 2);
        assert_eq!(reloaded.desktop.theme, "light");
    }
}
