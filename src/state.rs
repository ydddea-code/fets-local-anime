use crate::config::Settings;
use crate::services::hls::HlsManager;
use crate::services::metadata::MetadataClient;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub settings: Settings,
    pub hls: Arc<HlsManager>,
    pub metadata: MetadataClient,
    pub sessions: Arc<RwLock<HashMap<String, Instant>>>,
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
            settings,
            hls,
            metadata,
            sessions: Arc::default(),
        }
    }
}
