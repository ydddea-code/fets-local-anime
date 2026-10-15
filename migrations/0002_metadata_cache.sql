ALTER TABLE anime ADD COLUMN metadata_locked INTEGER NOT NULL DEFAULT 0;
CREATE TABLE metadata_cache (
  cache_key TEXT PRIMARY KEY,
  payload_json TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
