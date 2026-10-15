ALTER TABLE episode ADD COLUMN removed_at TEXT;
CREATE INDEX idx_episode_removed ON episode(removed_at);
