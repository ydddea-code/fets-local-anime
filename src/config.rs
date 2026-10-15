use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub server: ServerConfig,
    pub library: LibraryConfig,
    pub data: DataConfig,
    pub ffmpeg: FfmpegConfig,
    pub metadata: MetadataConfig,
    pub auth: AuthConfig,
    #[serde(flatten)]
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
    pub scan_on_startup: bool,
    pub watch: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DataConfig {
    pub dir: String,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FfmpegConfig {
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub hls_segment_seconds: u32,
    pub max_concurrent_transcodes: usize,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MetadataConfig {
    pub anilist_enabled: bool,
    pub cache_ttl_hours: u64,
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    pub enabled: bool,
    pub password: String,
}

/// Desktop-only options (player preferences and tool overrides). These live in
/// `settings.toml` inside the app data directory and are edited from the
/// Pengaturan page instead of a manual config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DesktopConfig {
    /// "auto" (HTML5 first, mpv fallback), "html5", or "mpv".
    pub player: String,
    /// Explicit path to mpv.exe; empty means auto-detect.
    pub mpv_path: String,
    /// Theme: "dark", "light", or "system".
    pub theme: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
        }
    }
}
impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            extensions: vec![
                "mkv".into(),
                "mp4".into(),
                "webm".into(),
                "avi".into(),
                "m4v".into(),
            ],
            scan_on_startup: true,
            // File watcher is on by default in desktop mode so new files appear automatically.
            watch: true,
        }
    }
}
impl Default for DataConfig {
    fn default() -> Self {
        Self {
            dir: crate::desktop::default_data_dir().to_string_lossy().to_string(),
        }
    }
}
impl Default for FfmpegConfig {
    fn default() -> Self {
        Self {
            ffmpeg_path: "ffmpeg".into(),
            ffprobe_path: "ffprobe".into(),
            hls_segment_seconds: 6,
            max_concurrent_transcodes: 2,
        }
    }
}
impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            anilist_enabled: false,
            cache_ttl_hours: 24,
        }
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

impl Settings {
    /// Load settings for classic server mode: optional `config.toml` next to
    /// the binary plus `ANIME__*` environment overrides. Kept behind the
    /// `server-mode` feature; the desktop app uses [`Settings::load_desktop`].
    #[allow(dead_code)]
    pub fn load() -> anyhow::Result<Self> {
        let mut builder = config::Config::builder()
            .set_default("server.host", "127.0.0.1")?
            .set_default("server.port", 8080)?;
        if Path::new("config.toml").exists() {
            builder = builder.add_source(config::File::with_name("config").required(false));
        }
        builder = builder.add_source(config::Environment::with_prefix("ANIME").separator("__"));
        let settings: Self = builder.build()?.try_deserialize()?;
        settings.validate_library_paths()?;
        Ok(settings)
    }

    /// Desktop mode: persisted user settings from `<data dir>/settings.toml`,
    /// falling back to defaults when the file does not exist yet. An optional
    /// `config.toml` beside the executable is still honored but no longer
    /// required. Auth is always disabled in desktop mode.
    pub fn load_desktop(data_dir: &Path) -> Self {
        let mut settings = Self::default();
        settings.data.dir = data_dir.to_string_lossy().to_string();
        let config_toml = data_dir.join("settings.toml");
        if let Ok(text) = std::fs::read_to_string(&config_toml) {
            match toml::from_str::<Settings>(&text) {
                Ok(saved) => {
                    let dir = settings.data.dir.clone();
                    settings = saved;
                    settings.data.dir = dir;
                }
                Err(error) => {
                    tracing::warn!(%error, path = %config_toml.display(), "invalid settings.toml; using defaults")
                }
            }
        } else if Path::new("config.toml").exists() {
            // Optional escape hatch for power users; not part of the normal flow.
            if let Ok(legacy) = Self::load() {
                let dir = settings.data.dir.clone();
                settings = legacy;
                settings.data.dir = dir;
            }
        }
        settings.auth.enabled = false;
        settings.auth.password.clear();
        let _ = settings.validate_library_paths();
        settings
    }

    /// Persist current settings to `<data dir>/settings.toml`.
    pub fn save(&self) -> anyhow::Result<()> {
        let dir = PathBuf::from(&self.data.dir);
        std::fs::create_dir_all(&dir)?;
        let text = toml::to_string_pretty(self)?;
        std::fs::write(dir.join("settings.toml"), text)?;
        Ok(())
    }

    /// Warn about configured library folders that are currently inaccessible
    /// (removed drive, offline NAS share). Entries are kept so an offline UNC
    /// share reappears in scans once it becomes reachable again.
    fn validate_library_paths(&mut self) -> anyhow::Result<()> {
        for path in &self.library.paths {
            if !Path::new(path).is_dir() {
                tracing::warn!(path, "library path is not accessible right now; scans will skip it until it returns");
            }
        }
        if self.auth.enabled && self.auth.password.is_empty() {
            anyhow::bail!("auth.password must be set when authentication is enabled");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::load_desktop(dir.path());
        assert!(!settings.auth.enabled);
        assert!(settings.library.watch);
        settings.desktop.player = "mpv".into();
        settings.library.paths.push(dir.path().join("Anime").to_string_lossy().into());
        settings.save().unwrap();
        let reloaded = Settings::load_desktop(dir.path());
        assert_eq!(reloaded.desktop.player, "mpv");
        assert_eq!(reloaded.library.paths, settings.library.paths);
        assert_eq!(reloaded.data.dir, settings.data.dir);
    }

    #[test]
    fn invalid_settings_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.toml"), "this is not valid toml [[[").unwrap();
        let settings = Settings::load_desktop(dir.path());
        assert_eq!(settings.desktop.player, "auto");
    }
}
