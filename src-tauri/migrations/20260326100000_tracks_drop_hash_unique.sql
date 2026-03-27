-- Drop UNIQUE constraint on track_hash to allow multiple track rows
-- with the same hash (e.g. imported from different DJ library sources).
-- File storage is deduplicated by hash, but DB rows are per-import.

CREATE TABLE tracks_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    track_hash TEXT NOT NULL,
    title TEXT,
    artist TEXT,
    album TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    duration_seconds REAL,
    file_path TEXT NOT NULL,
    storage_path TEXT,
    album_art_path TEXT,
    album_art_mime TEXT,
    source_type TEXT,
    source_id TEXT,
    source_filename TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

INSERT INTO tracks_new
SELECT * FROM tracks;

DROP TABLE tracks;
ALTER TABLE tracks_new RENAME TO tracks;

-- Recreate index for fast hash lookups (non-unique)
CREATE INDEX idx_tracks_hash ON tracks(track_hash);
