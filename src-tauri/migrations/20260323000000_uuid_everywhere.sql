-- ============================================================================
-- UUID Everywhere: Replace INTEGER AUTOINCREMENT PKs with TEXT UUID PKs
-- Remove all remote_id columns (local id IS the cloud id now)
-- ============================================================================
-- Strategy: For each table (root-to-leaf FK order), create new table with TEXT
-- id, copy data with UUID substitution, drop old, rename new.

-- Helper: generate a v4 UUID in pure SQL
-- lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
--   substr(hex(randomblob(2)),2) || '-' ||
--   substr('89ab', abs(random()) % 4 + 1, 1) ||
--   substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))

-- ============================================================================
-- 1. VENUES (root table)
-- ============================================================================
CREATE TABLE _uuid_map_venues (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_venues (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM venues;

CREATE TABLE venues_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    name TEXT NOT NULL,
    description TEXT,
    share_code TEXT,
    role TEXT NOT NULL DEFAULT 'owner',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

INSERT INTO venues_new (id, uid, name, description, share_code, role, created_at, updated_at, version, synced_at)
SELECT m.new_id, v.uid, v.name, v.description, v.share_code, v.role, v.created_at, v.updated_at, v.version, v.synced_at
FROM venues v JOIN _uuid_map_venues m ON v.id = m.old_id;

-- ============================================================================
-- 2. TRACKS (root table)
-- ============================================================================
CREATE TABLE _uuid_map_tracks (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_tracks (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM tracks;

CREATE TABLE tracks_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
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
    source_type TEXT,
    source_id TEXT,
    source_filename TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

INSERT INTO tracks_new (id, uid, track_hash, title, artist, album, track_number, disc_number,
    duration_seconds, file_path, storage_path, album_art_path, album_art_mime,
    source_type, source_id, source_filename, created_at, updated_at, version, synced_at)
SELECT m.new_id, t.uid, t.track_hash, t.title, t.artist, t.album, t.track_number, t.disc_number,
    t.duration_seconds, t.file_path, t.storage_path, t.album_art_path, t.album_art_mime,
    t.source_type, t.source_id, t.source_filename, t.created_at, t.updated_at, t.version, t.synced_at
FROM tracks t JOIN _uuid_map_tracks m ON t.id = m.old_id;

-- ============================================================================
-- 3. PATTERN_CATEGORIES (root table)
-- ============================================================================
CREATE TABLE _uuid_map_categories (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_categories (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM pattern_categories;

CREATE TABLE pattern_categories_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

INSERT INTO pattern_categories_new (id, uid, name, created_at, updated_at, version, synced_at)
SELECT m.new_id, c.uid, c.name, c.created_at, c.updated_at, c.version, c.synced_at
FROM pattern_categories c JOIN _uuid_map_categories m ON c.id = m.old_id;

-- ============================================================================
-- 4. PATTERNS (depends on categories)
-- ============================================================================
CREATE TABLE _uuid_map_patterns (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_patterns (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM patterns;

CREATE TABLE patterns_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    name TEXT NOT NULL,
    description TEXT,
    category_id TEXT REFERENCES pattern_categories(id) ON DELETE SET NULL,
    is_published INTEGER NOT NULL DEFAULT 0,
    author_name TEXT,
    forked_from_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

INSERT INTO patterns_new (id, uid, name, description, category_id, is_published, author_name,
    forked_from_id, created_at, updated_at, version, synced_at)
SELECT mp.new_id, p.uid, p.name, p.description,
    mc.new_id,
    p.is_published, p.author_name,
    p.forked_from_remote_id,
    p.created_at, p.updated_at, p.version, p.synced_at
FROM patterns p
JOIN _uuid_map_patterns mp ON p.id = mp.old_id
LEFT JOIN _uuid_map_categories mc ON p.category_id = mc.old_id;

-- ============================================================================
-- 5. IMPLEMENTATIONS (depends on patterns)
-- ============================================================================
CREATE TABLE _uuid_map_implementations (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_implementations (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM implementations;

CREATE TABLE implementations_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    pattern_id TEXT NOT NULL,
    name TEXT,
    graph_json TEXT NOT NULL DEFAULT '{"nodes":[],"edges":[]}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
);

INSERT INTO implementations_new (id, uid, pattern_id, name, graph_json, created_at, updated_at, version, synced_at)
SELECT mi.new_id, i.uid, mp.new_id, i.name, i.graph_json, i.created_at, i.updated_at, i.version, i.synced_at
FROM implementations i
JOIN _uuid_map_implementations mi ON i.id = mi.old_id
JOIN _uuid_map_patterns mp ON i.pattern_id = mp.old_id;

-- ============================================================================
-- 6. FIXTURES (depends on venues, id is already TEXT/UUID)
-- ============================================================================
-- Fixtures already use TEXT UUIDs for id, just need to remap venue_id and drop remote_id
CREATE TABLE fixtures_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
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

INSERT INTO fixtures_new (id, uid, venue_id, universe, address, num_channels,
    manufacturer, model, mode_name, fixture_path, label,
    pos_x, pos_y, pos_z, rot_x, rot_y, rot_z,
    created_at, updated_at, version, synced_at)
SELECT f.id, f.uid, mv.new_id, f.universe, f.address, f.num_channels,
    f.manufacturer, f.model, f.mode_name, f.fixture_path, f.label,
    f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z,
    f.created_at, f.updated_at, f.version, f.synced_at
FROM fixtures f
JOIN _uuid_map_venues mv ON f.venue_id = mv.old_id;

-- ============================================================================
-- 7. VENUE_IMPLEMENTATION_OVERRIDES (depends on venues, patterns, implementations)
-- ============================================================================
CREATE TABLE venue_implementation_overrides_new (
    venue_id TEXT NOT NULL,
    pattern_id TEXT NOT NULL,
    implementation_id TEXT NOT NULL,
    uid TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    PRIMARY KEY (venue_id, pattern_id),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE,
    FOREIGN KEY (implementation_id) REFERENCES implementations(id) ON DELETE CASCADE
);

INSERT INTO venue_implementation_overrides_new (venue_id, pattern_id, implementation_id, uid,
    created_at, updated_at, version, synced_at)
SELECT mv.new_id, mp.new_id, mi.new_id, o.uid,
    o.created_at, o.updated_at, o.version, o.synced_at
FROM venue_implementation_overrides o
JOIN _uuid_map_venues mv ON o.venue_id = mv.old_id
JOIN _uuid_map_patterns mp ON o.pattern_id = mp.old_id
JOIN _uuid_map_implementations mi ON o.implementation_id = mi.old_id;

-- ============================================================================
-- 8. TRACK_BEATS (depends on tracks, keyed by track_id)
-- ============================================================================
CREATE TABLE track_beats_new (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
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

INSERT INTO track_beats_new (track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset,
    beats_per_bar, created_at, updated_at, version, synced_at)
SELECT mt.new_id, b.uid, b.beats_json, b.downbeats_json, b.bpm, b.downbeat_offset,
    b.beats_per_bar, b.created_at, b.updated_at, b.version, b.synced_at
FROM track_beats b
JOIN _uuid_map_tracks mt ON b.track_id = mt.old_id;

-- ============================================================================
-- 9. TRACK_ROOTS (depends on tracks, keyed by track_id)
-- ============================================================================
CREATE TABLE track_roots_new (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
    sections_json TEXT NOT NULL,
    logits_path TEXT,
    logits_storage_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

INSERT INTO track_roots_new (track_id, uid, sections_json, logits_path, logits_storage_path,
    created_at, updated_at, version, synced_at)
SELECT mt.new_id, r.uid, r.sections_json, r.logits_path, r.logits_storage_path,
    r.created_at, r.updated_at, r.version, r.synced_at
FROM track_roots r
JOIN _uuid_map_tracks mt ON r.track_id = mt.old_id;

-- ============================================================================
-- 10. TRACK_WAVEFORMS (depends on tracks, keyed by track_id)
-- ============================================================================
CREATE TABLE track_waveforms_new (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
    preview_samples_blob BLOB NOT NULL,
    full_samples_blob BLOB,
    colors_blob BLOB,
    preview_colors_blob BLOB,
    bands_blob BLOB,
    preview_bands_blob BLOB,
    sample_rate INTEGER NOT NULL,
    decoded_duration REAL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

INSERT INTO track_waveforms_new (track_id, uid, preview_samples_blob, full_samples_blob,
    colors_blob, preview_colors_blob, bands_blob, preview_bands_blob,
    sample_rate, decoded_duration, created_at, updated_at, version, synced_at)
SELECT mt.new_id, w.uid, w.preview_samples_blob, w.full_samples_blob,
    w.colors_blob, w.preview_colors_blob, w.bands_blob, w.preview_bands_blob,
    w.sample_rate, w.decoded_duration, w.created_at, w.updated_at, w.version, w.synced_at
FROM track_waveforms w
JOIN _uuid_map_tracks mt ON w.track_id = mt.old_id;

-- ============================================================================
-- 11. TRACK_STEMS (depends on tracks, composite PK)
-- ============================================================================
CREATE TABLE track_stems_new (
    track_id TEXT NOT NULL,
    uid TEXT,
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

INSERT INTO track_stems_new (track_id, uid, stem_name, file_path, storage_path,
    created_at, updated_at, version, synced_at)
SELECT mt.new_id, s.uid, s.stem_name, s.file_path, s.storage_path,
    s.created_at, s.updated_at, s.version, s.synced_at
FROM track_stems s
JOIN _uuid_map_tracks mt ON s.track_id = mt.old_id;

-- ============================================================================
-- 12. FIXTURE_GROUPS (depends on venues)
-- ============================================================================
CREATE TABLE _uuid_map_groups (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_groups (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM fixture_groups;

CREATE TABLE fixture_groups_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    name TEXT,
    axis_lr REAL,
    axis_fb REAL,
    axis_ab REAL,
    movement_config TEXT,
    display_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

INSERT INTO fixture_groups_new (id, uid, venue_id, name, axis_lr, axis_fb, axis_ab,
    movement_config, display_order, created_at, updated_at, version, synced_at)
SELECT mg.new_id, g.uid, mv.new_id, g.name, g.axis_lr, g.axis_fb, g.axis_ab,
    g.movement_config, g.display_order, g.created_at, g.updated_at, g.version, g.synced_at
FROM fixture_groups g
JOIN _uuid_map_groups mg ON g.id = mg.old_id
JOIN _uuid_map_venues mv ON g.venue_id = mv.old_id;

-- ============================================================================
-- 13. FIXTURE_GROUP_MEMBERS (depends on fixtures, groups)
-- ============================================================================
CREATE TABLE fixture_group_members_new (
    fixture_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    display_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (fixture_id, group_id),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE
);

INSERT INTO fixture_group_members_new (fixture_id, group_id, display_order, created_at)
SELECT m.fixture_id, mg.new_id, m.display_order, m.created_at
FROM fixture_group_members m
JOIN _uuid_map_groups mg ON m.group_id = mg.old_id;

-- ============================================================================
-- 14. SCORES (depends on tracks, venues)
-- ============================================================================
CREATE TABLE _uuid_map_scores (old_id INTEGER PRIMARY KEY, new_id TEXT NOT NULL);
INSERT INTO _uuid_map_scores (old_id, new_id)
SELECT id, lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
  substr(hex(randomblob(2)),2) || '-' ||
  substr('89ab', abs(random()) % 4 + 1, 1) ||
  substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))
FROM scores;

CREATE TABLE scores_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    track_id TEXT NOT NULL,
    venue_id TEXT,
    name TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

INSERT INTO scores_new (id, uid, track_id, venue_id, name, created_at, updated_at, version, synced_at)
SELECT ms.new_id, s.uid, mt.new_id, mv.new_id, s.name, s.created_at, s.updated_at, s.version, s.synced_at
FROM scores s
JOIN _uuid_map_scores ms ON s.id = ms.old_id
JOIN _uuid_map_tracks mt ON s.track_id = mt.old_id
LEFT JOIN _uuid_map_venues mv ON s.venue_id = mv.old_id;

-- ============================================================================
-- 15. TRACK_SCORES (depends on scores, patterns)
-- ============================================================================
CREATE TABLE track_scores_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    score_id TEXT NOT NULL,
    pattern_id TEXT NOT NULL,
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

INSERT INTO track_scores_new (id, uid, score_id, pattern_id, start_time, end_time,
    z_index, blend_mode, args_json, created_at, updated_at, version, synced_at)
SELECT
    lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
      substr(hex(randomblob(2)),2) || '-' ||
      substr('89ab', abs(random()) % 4 + 1, 1) ||
      substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6))),
    ts.uid, ms.new_id, mp.new_id, ts.start_time, ts.end_time,
    ts.z_index, ts.blend_mode, ts.args_json, ts.created_at, ts.updated_at, ts.version, ts.synced_at
FROM track_scores ts
JOIN _uuid_map_scores ms ON ts.score_id = ms.old_id
JOIN _uuid_map_patterns mp ON ts.pattern_id = mp.old_id;

-- ============================================================================
-- DROP OLD TABLES (leaf-to-root order)
-- ============================================================================
DROP TABLE IF EXISTS fixture_group_members;
DROP TABLE IF EXISTS track_scores;
DROP TABLE IF EXISTS scores;
DROP TABLE IF EXISTS venue_implementation_overrides;
DROP TABLE IF EXISTS track_waveforms;
DROP TABLE IF EXISTS track_stems;
DROP TABLE IF EXISTS track_roots;
DROP TABLE IF EXISTS track_beats;
DROP TABLE IF EXISTS fixture_groups;
DROP TABLE IF EXISTS fixtures;
DROP TABLE IF EXISTS implementations;
DROP TABLE IF EXISTS patterns;
DROP TABLE IF EXISTS pattern_categories;
DROP TABLE IF EXISTS tracks;
DROP TABLE IF EXISTS venues;

-- ============================================================================
-- RENAME NEW TABLES
-- ============================================================================
ALTER TABLE venues_new RENAME TO venues;
ALTER TABLE tracks_new RENAME TO tracks;
ALTER TABLE pattern_categories_new RENAME TO pattern_categories;
ALTER TABLE patterns_new RENAME TO patterns;
ALTER TABLE implementations_new RENAME TO implementations;
ALTER TABLE fixtures_new RENAME TO fixtures;
ALTER TABLE venue_implementation_overrides_new RENAME TO venue_implementation_overrides;
ALTER TABLE track_beats_new RENAME TO track_beats;
ALTER TABLE track_roots_new RENAME TO track_roots;
ALTER TABLE track_waveforms_new RENAME TO track_waveforms;
ALTER TABLE track_stems_new RENAME TO track_stems;
ALTER TABLE fixture_groups_new RENAME TO fixture_groups;
ALTER TABLE fixture_group_members_new RENAME TO fixture_group_members;
ALTER TABLE scores_new RENAME TO scores;
ALTER TABLE track_scores_new RENAME TO track_scores;

-- ============================================================================
-- DROP UUID MAPPING TABLES
-- ============================================================================
DROP TABLE IF EXISTS _uuid_map_venues;
DROP TABLE IF EXISTS _uuid_map_tracks;
DROP TABLE IF EXISTS _uuid_map_categories;
DROP TABLE IF EXISTS _uuid_map_patterns;
DROP TABLE IF EXISTS _uuid_map_implementations;
DROP TABLE IF EXISTS _uuid_map_groups;
DROP TABLE IF EXISTS _uuid_map_scores;

-- ============================================================================
-- RECREATE INDEXES
-- ============================================================================
CREATE INDEX idx_fixtures_venue ON fixtures(venue_id);
CREATE INDEX idx_fixtures_universe ON fixtures(universe);
CREATE INDEX idx_implementations_pattern ON implementations(pattern_id);
CREATE INDEX idx_scores_track ON scores(track_id);
CREATE INDEX idx_scores_venue ON scores(venue_id);
CREATE UNIQUE INDEX idx_scores_track_venue ON scores(track_id, venue_id);
CREATE INDEX idx_track_scores_score ON track_scores(score_id);
CREATE INDEX idx_fixture_groups_venue ON fixture_groups(venue_id);
CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);
CREATE UNIQUE INDEX idx_venues_share_code ON venues(share_code) WHERE share_code IS NOT NULL;

-- ============================================================================
-- RECREATE TRIGGERS (updated_at auto-update, version-guarded)
-- ============================================================================
CREATE TRIGGER venues_updated_at AFTER UPDATE ON venues FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE venues SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER tracks_updated_at AFTER UPDATE ON tracks FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE tracks SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER pattern_categories_updated_at AFTER UPDATE ON pattern_categories FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE pattern_categories SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER patterns_updated_at AFTER UPDATE ON patterns FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE patterns SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER implementations_updated_at AFTER UPDATE ON implementations FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE implementations SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER fixtures_updated_at AFTER UPDATE ON fixtures FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixtures SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER venue_implementation_overrides_updated_at AFTER UPDATE ON venue_implementation_overrides FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE venue_implementation_overrides SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE venue_id = OLD.venue_id AND pattern_id = OLD.pattern_id; END;

CREATE TRIGGER track_beats_updated_at AFTER UPDATE ON track_beats FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_beats SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_roots_updated_at AFTER UPDATE ON track_roots FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_roots SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_waveforms_updated_at AFTER UPDATE ON track_waveforms FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_waveforms SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_stems_updated_at AFTER UPDATE ON track_stems FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_stems SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name; END;

CREATE TRIGGER scores_updated_at AFTER UPDATE ON scores FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE scores SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER track_scores_updated_at AFTER UPDATE ON track_scores FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_scores SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER fixture_groups_updated_at AFTER UPDATE ON fixture_groups FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixture_groups SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;
