use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub server: ServerConfig,
    pub library: LibraryConfig,
    pub data: DataConfig,
    pub ffmpeg: FfmpegConfig,
    pub metadata: MetadataConfig,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LibraryConfig {
    pub paths: Vec<String>,
    pub extensions: Vec<String>,
    pub scan_on_startup: bool,
    pub watch: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DataConfig {
    pub dir: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FfmpegConfig {
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub hls_segment_seconds: u32,
    pub max_concurrent_transcodes: usize,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MetadataConfig {
    pub anilist_enabled: bool,
    pub cache_ttl_hours: u64,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub enabled: bool,
    pub password: String,
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
            scan_on_startup: false,
            watch: false,
        }
    }
}
impl Default for DataConfig {
    fn default() -> Self {
        Self {
            dir: "./data".into(),
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
impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let mut builder = config::Config::builder()
            .set_default("server.host", "127.0.0.1")?
            .set_default("server.port", 8080)?;
        if std::path::Path::new("config.toml").exists() {
            builder = builder.add_source(config::File::with_name("config").required(false));
        }
        builder = builder.add_source(config::Environment::with_prefix("ANIME").separator("__"));
        let settings: Self = builder.build()?.try_deserialize()?;
        for path in &settings.library.paths {
            let path = std::path::Path::new(path);
            if !path.is_dir() {
                anyhow::bail!(
                    "library path is not an accessible directory: {}",
                    path.display()
                );
            }
        }
        if settings.auth.enabled && settings.auth.password.is_empty() {
            anyhow::bail!("auth.password must be set when authentication is enabled");
        }
        Ok(settings)
    }
}
