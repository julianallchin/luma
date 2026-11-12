-- Data format 2: the server is the permanent record and a device is a cache
-- of live rows (docs/design/data-layer.md; the server side is
-- supabase/migrations/20261112120000_data_layer.sql).
--
-- A source row's id is a random UUID its writer generates. Six tables built
-- theirs from other columns, as a GENERATED column or as the key itself, and
-- SQLite cannot alter either, so they are rebuilt. Their rows are not carried
-- over: this ships with PROJECTION_VERSION 2, which clears every synced table
-- and downloads it again. `user_version` is left where it was, because that
-- difference is what tells the app to.
--
-- "The one for this parent and key" stays unique here. The server keeps that
-- only among live rows, and a device holds no others, which is also why no
-- table here has `deleted_at` or `deleted_by`.
--
-- Derived tables (stems, roots, drum onsets, bar classifications, genres)
-- keep the ids their key spells.

DROP TABLE venue_node_params;
CREATE TABLE venue_node_params (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL,
    uid TEXT,
    venue_id TEXT,
    key TEXT NOT NULL,
    value REAL NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (node_id, key),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_venue_node_params_venue ON venue_node_params(venue_id);
CREATE TRIGGER venue_node_params_updated_at AFTER UPDATE ON venue_node_params FOR EACH ROW
BEGIN UPDATE venue_node_params SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

DROP TABLE venue_constraints;
CREATE TABLE venue_constraints (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL,
    uid TEXT,
    venue_id TEXT,
    my_socket TEXT NOT NULL,
    target_node TEXT NOT NULL,
    target_socket TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (node_id, my_socket),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (target_node) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_venue_constraints_target ON venue_constraints(target_node);
CREATE INDEX idx_venue_constraints_venue ON venue_constraints(venue_id);
CREATE TRIGGER venue_constraints_updated_at AFTER UPDATE ON venue_constraints FOR EACH ROW
BEGIN UPDATE venue_constraints SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

DROP TABLE folder_tracks;
CREATE TABLE folder_tracks (
    id TEXT PRIMARY KEY NOT NULL,
    folder_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    uid TEXT,
    venue_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (folder_id, track_id),
    FOREIGN KEY (folder_id, venue_id) REFERENCES folders(id, venue_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_folder_tracks_venue ON folder_tracks(venue_id);
CREATE INDEX idx_folder_tracks_track ON folder_tracks(track_id);

-- A beat grid is written once and a track has many: reanalysis adds a row,
-- and the track's current grid is its newest. The stamp is ISO-8601 like the
-- server's, so "newest" is the same row whichever side wrote it.
DROP TABLE track_beats;
CREATE TABLE track_beats (
    id TEXT PRIMARY KEY NOT NULL,
    track_id TEXT NOT NULL,
    uid TEXT,
    beats_json TEXT NOT NULL,
    downbeats_json TEXT NOT NULL,
    bpm REAL,
    downbeat_offset REAL,
    beats_per_bar INTEGER,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_track_beats_track ON track_beats(track_id, created_at);
CREATE TRIGGER track_beats_updated_at AFTER UPDATE ON track_beats FOR EACH ROW
BEGIN UPDATE track_beats SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

DROP TABLE track_beat_validations;
CREATE TABLE track_beat_validations (
    id TEXT PRIMARY KEY NOT NULL,
    track_id TEXT NOT NULL UNIQUE REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    uid TEXT,
    track_hash TEXT NOT NULL,
    grid_id TEXT,
    grid_json TEXT NOT NULL,
    processor_version INTEGER NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('unreviewed', 'correct', 'incorrect')),
    reason TEXT CHECK (reason IS NULL OR (verdict = 'incorrect' AND reason IN ('tempo', 'offset', 'drift', 'bar_phase'))),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TRIGGER track_beat_validations_updated_at AFTER UPDATE ON track_beat_validations FOR EACH ROW
BEGIN UPDATE track_beat_validations SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

DROP TABLE agent_thread_transcript_heads;
CREATE TABLE agent_thread_transcript_heads (
    id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL UNIQUE,
    uid TEXT,
    head_message_id TEXT,
    message_count INTEGER NOT NULL DEFAULT 0 CHECK (message_count >= 0),
    created_at TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    FOREIGN KEY (thread_id) REFERENCES agent_threads(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (head_message_id) REFERENCES agent_thread_messages(id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (head_message_id IS NULL AND message_count = 0)
        OR (head_message_id IS NOT NULL AND message_count > 0)
    )
);
CREATE INDEX idx_agent_thread_transcript_heads_owner
    ON agent_thread_transcript_heads(uid, updated_at DESC);

-- A new thread's empty transcript, which the schema writes itself, so the
-- schema is its writer and generates its id: a version 4 UUID.
DROP TRIGGER agent_thread_create_empty_transcript;
CREATE TRIGGER agent_thread_create_empty_transcript
AFTER INSERT ON agent_threads FOR EACH ROW
BEGIN
    INSERT OR IGNORE INTO agent_thread_transcript_heads (id, thread_id, uid, created_at, updated_at)
    VALUES (
        lower(printf('%s-%s-4%s-%s%s-%s',
            hex(randomblob(4)), hex(randomblob(2)), substr(hex(randomblob(2)), 2),
            substr('89ab', 1 + (abs(random()) % 4), 1), substr(hex(randomblob(2)), 2),
            hex(randomblob(6)))),
        NEW.id, NEW.uid, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
        strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;

-- A laser's limits were keyed by their fixture's id.
DROP TABLE laser_limits;
CREATE TABLE laser_limits (
    id TEXT PRIMARY KEY NOT NULL,
    uid TEXT,
    venue_id TEXT NOT NULL,
    fixture_id TEXT NOT NULL UNIQUE,
    points TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_laser_limits_venue ON laser_limits(venue_id);
CREATE TRIGGER laser_limits_updated_at AFTER UPDATE ON laser_limits FOR EACH ROW
BEGIN UPDATE laser_limits SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- A clip an agent wants to refer to later is named, not addressed by an id
-- it chose.
ALTER TABLE clips ADD COLUMN label TEXT;
CREATE UNIQUE INDEX idx_clips_label ON clips(score_id, label) WHERE label IS NOT NULL;

-- The grid a score was written against. Not a foreign key: a grid is a
-- versioned input, and a score that names one is not its child.
ALTER TABLE scores ADD COLUMN grid_id TEXT;
