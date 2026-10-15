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
    pub total: std::sync::atomic::AtomicU64,
    pub done: std::sync::atomic::AtomicU64,
    pub current: Mutex<String>,
}

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
        }
    }

    pub fn settings_snapshot(&self) -> Settings {
        self.settings.read().expect("settings lock").clone()
    }
}
