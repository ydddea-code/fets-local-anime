use crate::config::Settings;
use crate::services::hls::HlsManager;
use crate::services::metadata::MetadataClient;
use sqlx::SqlitePool;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::process::Child;
use tokio::sync::RwLock as AsyncRwLock;

pub struct ScanJob {
    pub id: String,
    pub total: std::sync::atomic::AtomicU64,
    pub done: std::sync::atomic::AtomicU64,
    pub current: Mutex<String>,
}

/// Callback the desktop shell (Tauri) registers to be told about freshly
/// launched mpv processes: (session id, OS pid, episode id).
type MpvLaunchFn = Box<dyn Fn(&str, u32, i64) + Send + Sync + 'static>;
type MpvLaunchListener = Arc<RwLock<Option<MpvLaunchFn>>>;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub settings: Arc<RwLock<Settings>>,
    pub hls: Arc<HlsManager>,
    pub metadata: MetadataClient,
    pub sessions: Arc<AsyncRwLock<HashMap<String, Instant>>>,
    pub scan: Arc<Mutex<Option<Arc<ScanJob>>>>,
    pub last_scan: Arc<Mutex<Option<String>>>,
    pub scan_listeners: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>>,
    pub mpv_sessions: Arc<Mutex<HashMap<String, Child>>>,
    pub watch_roots: Arc<Mutex<Vec<PathBuf>>>,
    pub watch_events: Arc<Mutex<VecDeque<()>>>,
    pub mpv_launch_listener: MpvLaunchListener,
}

impl AppState {
    pub fn new(
        pool: SqlitePool,
        settings: Settings,
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
            mpv_launch_listener: Arc::default(),
        }
    }

    pub fn settings_snapshot(&self) -> Settings {
        self.settings.read().expect("settings lock").clone()
    }

    /// Announce a freshly launched mpv process so the desktop shell (Tauri)
    /// can reparent its window into the application frame. The core library
    /// stays UI-agnostic: without a registered listener this is a no-op and
    /// mpv simply keeps its own standalone window.
    pub fn set_mpv_launch_listener(&self, listener: MpvLaunchFn) {
        *self.mpv_launch_listener.write().expect("mpv listener lock") = Some(listener);
    }

    pub(crate) fn notify_mpv_launch(&self, id: &str, pid: u32, episode_id: i64) {
        let listener = self.mpv_launch_listener.read().expect("mpv listener lock");
        if let Some(listener) = listener.as_ref() {
            listener(id, pid, episode_id);
        }
    }
}
