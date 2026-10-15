use crate::services::hls::HlsManager;
use crate::services::metadata::MetadataClient;
use sqlx::SqlitePool;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::process::Child;
use tokio::sync::RwLock as AsyncRwLock;

/// One running scan job with atomic progress counters so the UI can poll or
/// subscribe over SSE while the walk happens on a blocking thread.
pub struct ScanJob {
    pub total: std::sync::atomic::AtomicU64,
    pub done: std::sync::atomic::AtomicU64,
    pub current: Mutex<String>,
}

/// Shared mutable settings: edited from the Pengaturan page and persisted to
/// `settings.toml` immediately (no manual config file in desktop mode).
pub type SharedSettings = Arc<RwLock<crate::config::Settings>>;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub settings: SharedSettings,
    pub hls: Arc<HlsManager>,
    pub metadata: MetadataClient,
    /// Auth session tokens -> expiry (only used when auth is enabled, i.e.
    /// the optional `server-mode` build; disabled in the desktop app).
    pub sessions: Arc<AsyncRwLock<HashMap<String, Instant>>>,
    /// Currently running library scan, if any.
    pub scan: Arc<Mutex<Option<Arc<ScanJob>>>>,
    /// Result summary of the most recent finished scan.
    pub last_scan: Arc<Mutex<Option<String>>>,
    /// Subscribers of the `/api/library/events` SSE stream.
    pub scan_listeners: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>,
    /// Live mpv child processes, kept so none are orphaned at shutdown.
    pub mpv_sessions: Arc<Mutex<HashMap<String, Child>>>,
    /// Library folders currently watched by the file watcher task.
    pub watch_roots: Arc<Mutex<Vec<PathBuf>>>,
    /// Debounce queue fed by the notify watcher callback.
    pub watch_events: Arc<Mutex<VecDeque<()>>>,
}

impl AppState {
    pub fn new(
        pool: SqlitePool,
        settings: crate::config::Settings,
        hls: Arc<HlsManager>,
        metadata: MetadataClient,
    ) -> Self {
        Self {
            pool,
            settings: Arc::new(RwLock::new(settings)),
            hls,
            metadata,
            sessions: Arc::default(),
            scan: Arc::default(),
            last_scan: Arc::default(),
            scan_listeners: Arc::default(),
            mpv_sessions: Arc::default(),
            watch_roots: Arc::default(),
            watch_events: Arc::default(),
        }
    }

    /// Snapshot of the ffmpeg tool paths after applying sidecar detection
    /// (bundled binaries next to the executable win over PATH names).
    pub fn ffmpeg_paths(&self) -> (String, String) {
        let settings = self.settings.read().expect("settings lock");
        (
            crate::services::mpv::find_tool(&settings.ffmpeg.ffmpeg_path, "ffmpeg"),
            crate::services::mpv::find_tool(&settings.ffmpeg.ffprobe_path, "ffprobe"),
        )
    }
}
