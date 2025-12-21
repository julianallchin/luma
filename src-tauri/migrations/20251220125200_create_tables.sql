PRAGMA foreign_keys = ON;

CREATE TABLE venues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER venues_updated_at
    AFTER UPDATE ON venues
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venues SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE fixtures (
    id TEXT PRIMARY KEY,
    remote_id TEXT UNIQUE,
    venue_id INTEGER NOT NULL,
    universe INTEGER NOT NULL DEFAULT 1,
    address INTEGER NOT NULL,
    num_channels INTEGER NOT NULL,
    manufacturer TEXT NOT NULL,
    model TEXT NOT NULL,
    mode_name TEXT NOT NULL,
    fixture_path TEXT NOT NULL,
    label TEXT,
    pos_x REAL DEFAULT 0.0,
    pos_y REAL DEFAULT 0.0,
    pos_z REAL DEFAULT 0.0,
    rot_x REAL DEFAULT 0.0,
    rot_y REAL DEFAULT 0.0,
    rot_z REAL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

CREATE INDEX idx_fixtures_venue ON fixtures(venue_id);
CREATE INDEX idx_fixtures_universe ON fixtures(universe);

CREATE TRIGGER fixtures_updated_at
    AFTER UPDATE ON fixtures
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE fixtures SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE pattern_categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER pattern_categories_updated_at
    AFTER UPDATE ON pattern_categories
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE pattern_categories SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    category_id INTEGER REFERENCES pattern_categories(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER patterns_updated_at
    AFTER UPDATE ON patterns
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE patterns SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE implementations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    pattern_id INTEGER NOT NULL,
    name TEXT,
    graph_json TEXT NOT NULL DEFAULT '{"nodes":[],"edges":[]}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
);

CREATE INDEX idx_implementations_pattern ON implementations(pattern_id);

CREATE TRIGGER implementations_updated_at
    AFTER UPDATE ON implementations
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE implementations SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

ALTER TABLE patterns
    ADD COLUMN default_implementation_id INTEGER REFERENCES implementations(id);

CREATE TABLE venue_implementation_overrides (
    venue_id INTEGER NOT NULL,
    pattern_id INTEGER NOT NULL,
    implementation_id INTEGER NOT NULL,
    remote_id TEXT UNIQUE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    PRIMARY KEY (venue_id, pattern_id),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE,
    FOREIGN KEY (implementation_id) REFERENCES implementations(id) ON DELETE CASCADE
);

CREATE TRIGGER venue_implementation_overrides_updated_at
    AFTER UPDATE ON venue_implementation_overrides
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venue_implementation_overrides SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE venue_id = OLD.venue_id AND pattern_id = OLD.pattern_id;
    END;

CREATE TABLE tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    track_hash TEXT NOT NULL UNIQUE,
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
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER tracks_updated_at
    AFTER UPDATE ON tracks
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE tracks SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE track_beats (
    track_id INTEGER PRIMARY KEY,
    beats_json TEXT NOT NULL,
    downbeats_json TEXT NOT NULL,
    bpm REAL,
    downbeat_offset REAL,
    beats_per_bar INTEGER,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_beats_updated_at
    AFTER UPDATE ON track_beats
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_beats SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;

CREATE TABLE track_roots (
    track_id INTEGER PRIMARY KEY,
    sections_json TEXT NOT NULL,
    logits_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_roots_updated_at
    AFTER UPDATE ON track_roots
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_roots SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;

CREATE TABLE track_waveforms (
    track_id INTEGER PRIMARY KEY,
    preview_samples_json TEXT NOT NULL,
    preview_colors_json TEXT,
    preview_bands_json TEXT,
    full_samples_json TEXT,
    colors_json TEXT,
    bands_json TEXT,
    sample_rate INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_waveforms_updated_at
    AFTER UPDATE ON track_waveforms
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_waveforms SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;

CREATE TABLE track_stems (
    track_id INTEGER NOT NULL,
    stem_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    storage_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    PRIMARY KEY (track_id, stem_name),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_stems_updated_at
    AFTER UPDATE ON track_stems
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_stems SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name;
    END;

CREATE TABLE scores (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    track_id INTEGER NOT NULL,
    name TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE INDEX idx_scores_track ON scores(track_id);

CREATE TRIGGER scores_updated_at
    AFTER UPDATE ON scores
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE scores SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE track_scores (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    score_id INTEGER NOT NULL,
    pattern_id INTEGER NOT NULL,
    start_time REAL NOT NULL,
    end_time REAL NOT NULL,
    z_index INTEGER NOT NULL DEFAULT 0,
    blend_mode TEXT NOT NULL DEFAULT 'replace',
    args_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
);

CREATE INDEX idx_score_annotations_score ON track_scores(score_id);

CREATE TRIGGER score_annotations_updated_at
    AFTER UPDATE ON track_scores
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_scores SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
