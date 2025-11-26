-- App database schema (idempotent for existing installs)

CREATE TABLE IF NOT EXISTS patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_hash TEXT NOT NULL UNIQUE,
    title TEXT,
    artist TEXT,
    album TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    duration_seconds REAL,
    file_path TEXT NOT NULL,
    album_art_path TEXT,
    album_art_mime TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS track_beats (
    track_id INTEGER PRIMARY KEY,
    beats_json TEXT NOT NULL,
    downbeats_json TEXT NOT NULL,
    bpm REAL,
    downbeat_offset REAL,
    beats_per_bar INTEGER,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS track_roots (
    track_id INTEGER PRIMARY KEY,
    sections_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS track_stems (
    track_id INTEGER NOT NULL,
    stem_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(track_id, stem_name),
    FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS track_waveforms (
    track_id INTEGER PRIMARY KEY,
    preview_samples_json TEXT NOT NULL,
    full_samples_json TEXT,
    colors_json TEXT,
    sample_rate INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    preview_colors_json TEXT,
    bands_json TEXT,
    preview_bands_json TEXT,
    FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS track_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id INTEGER NOT NULL,
    pattern_id INTEGER NOT NULL,
    start_time REAL NOT NULL,
    end_time REAL NOT NULL,
    z_index INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE,
    FOREIGN KEY(pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS track_annotations_track_idx ON track_annotations(track_id);

CREATE TABLE IF NOT EXISTS recent_projects (
    path TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    last_opened TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- updated_at triggers
CREATE TRIGGER IF NOT EXISTS patterns_updated_at
AFTER UPDATE ON patterns
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE patterns SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS tracks_updated_at
AFTER UPDATE ON tracks
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE tracks SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS track_beats_updated_at
AFTER UPDATE ON track_beats
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE track_beats SET updated_at = CURRENT_TIMESTAMP WHERE track_id = OLD.track_id;
END;

CREATE TRIGGER IF NOT EXISTS track_roots_updated_at
AFTER UPDATE ON track_roots
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE track_roots SET updated_at = CURRENT_TIMESTAMP WHERE track_id = OLD.track_id;
END;

CREATE TRIGGER IF NOT EXISTS track_stems_updated_at
AFTER UPDATE ON track_stems
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE track_stems
    SET updated_at = CURRENT_TIMESTAMP
    WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name;
END;

CREATE TRIGGER IF NOT EXISTS track_waveforms_updated_at
AFTER UPDATE ON track_waveforms
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE track_waveforms SET updated_at = CURRENT_TIMESTAMP WHERE track_id = OLD.track_id;
END;

CREATE TRIGGER IF NOT EXISTS track_annotations_updated_at
AFTER UPDATE ON track_annotations
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE track_annotations SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id;
END;
