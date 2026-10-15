CREATE TABLE anime (
  id INTEGER PRIMARY KEY,
  title TEXT NOT NULL,
  title_native TEXT,
  synopsis TEXT,
  poster_path TEXT,
  year INTEGER,
  status TEXT,
  anilist_id INTEGER UNIQUE,
  folder_path TEXT UNIQUE,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE genre (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE anime_genre (
  anime_id INTEGER NOT NULL REFERENCES anime(id) ON DELETE CASCADE,
  genre_id INTEGER NOT NULL REFERENCES genre(id) ON DELETE CASCADE,
  PRIMARY KEY(anime_id, genre_id)
);
CREATE TABLE episode (
  id INTEGER PRIMARY KEY,
  anime_id INTEGER NOT NULL REFERENCES anime(id) ON DELETE CASCADE,
  season INTEGER NOT NULL DEFAULT 1,
  number REAL NOT NULL,
  title TEXT,
  file_path TEXT NOT NULL UNIQUE,
  file_size INTEGER,
  duration_sec REAL,
  video_codec TEXT,
  audio_codec TEXT,
  container TEXT,
  streams_json TEXT,
  thumb_path TEXT,
  file_mtime INTEGER,
  added_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE subtitle (
  id INTEGER PRIMARY KEY,
  episode_id INTEGER NOT NULL REFERENCES episode(id) ON DELETE CASCADE,
  lang TEXT,
  label TEXT,
  format TEXT NOT NULL,
  file_path TEXT,
  stream_idx INTEGER
);
CREATE INDEX idx_episode_anime ON episode(anime_id, season, number);
CREATE TABLE progress (
  episode_id INTEGER PRIMARY KEY REFERENCES episode(id) ON DELETE CASCADE,
  position_sec REAL NOT NULL DEFAULT 0,
  watched INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
