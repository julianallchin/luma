-- Foreign keys on synced tables become deferrable.
--
-- A download is a consistent snapshot of the server, applied in whatever
-- order the checkpoint delivers it: a clip's score, a stem's track, a
-- message's thread may all arrive after the row that points at them. The
-- sync SDK writes on its own connection with foreign keys enforced, so an
-- immediate check turns a valid snapshot into a rejected one and the whole
-- checkpoint rolls back — nothing arrives, forever. Deferring the check to
-- the end of the transaction asks the same question of the same snapshot,
-- at the only moment the answer is meaningful.
--
-- The row-model migration already wrote its new tables this way; these are
-- the ones that predate it. Rebuilds, because SQLite has no ALTER for a
-- foreign key.

PRAGMA legacy_alter_table = ON;

-- fixtures
CREATE TABLE "fixtures__deferrable" (
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
    address_pinned INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "fixtures__deferrable" ("id", "uid", "venue_id", "universe", "address", "num_channels", "manufacturer", "model", "mode_name", "fixture_path", "label", "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "created_at", "updated_at", "address_pinned") SELECT "id", "uid", "venue_id", "universe", "address", "num_channels", "manufacturer", "model", "mode_name", "fixture_path", "label", "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "created_at", "updated_at", "address_pinned" FROM "fixtures";
DROP TABLE "fixtures";
ALTER TABLE "fixtures__deferrable" RENAME TO "fixtures";
CREATE INDEX idx_fixtures_venue ON fixtures(venue_id);
CREATE INDEX idx_fixtures_universe ON fixtures(universe);
CREATE TRIGGER fixtures_address_fits_universe_insert
BEFORE INSERT ON fixtures FOR EACH ROW
WHEN NEW.address < 1 OR NEW.num_channels < 1 OR NEW.address + NEW.num_channels - 1 > 512
BEGIN SELECT RAISE(ABORT, 'fixture footprint leaves its universe'); END;
CREATE TRIGGER fixtures_address_fits_universe_update
BEFORE UPDATE ON fixtures FOR EACH ROW
WHEN NEW.address < 1 OR NEW.num_channels < 1 OR NEW.address + NEW.num_channels - 1 > 512
BEGIN SELECT RAISE(ABORT, 'fixture footprint leaves its universe'); END;
CREATE TRIGGER fixtures_updated_at AFTER UPDATE ON fixtures FOR EACH ROW
BEGIN UPDATE fixtures SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- fixture_groups
CREATE TABLE "fixture_groups__deferrable" (
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
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "fixture_groups__deferrable" ("id", "uid", "venue_id", "name", "axis_lr", "axis_fb", "axis_ab", "movement_config", "display_order", "created_at", "updated_at") SELECT "id", "uid", "venue_id", "name", "axis_lr", "axis_fb", "axis_ab", "movement_config", "display_order", "created_at", "updated_at" FROM "fixture_groups";
DROP TABLE "fixture_groups";
ALTER TABLE "fixture_groups__deferrable" RENAME TO "fixture_groups";
CREATE INDEX idx_fixture_groups_venue ON fixture_groups(venue_id);
CREATE UNIQUE INDEX idx_fixture_groups_venue_name
    ON fixture_groups(venue_id, name);
CREATE TRIGGER fixture_groups_updated_at AFTER UPDATE ON fixture_groups FOR EACH ROW
BEGIN UPDATE fixture_groups SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- fixture_group_members
CREATE TABLE "fixture_group_members__deferrable" (
    id TEXT NOT NULL PRIMARY KEY,
    fixture_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    head_index INTEGER NOT NULL DEFAULT -1,
    display_order INTEGER NOT NULL DEFAULT 0,
    uid TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (fixture_id, group_id, head_index),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "fixture_group_members__deferrable" ("id", "fixture_id", "group_id", "head_index", "display_order", "uid", "created_at", "updated_at") SELECT "id", "fixture_id", "group_id", "head_index", "display_order", "uid", "created_at", "updated_at" FROM "fixture_group_members";
DROP TABLE "fixture_group_members";
ALTER TABLE "fixture_group_members__deferrable" RENAME TO "fixture_group_members";
CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);
CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
BEGIN UPDATE fixture_group_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- venue_nodes
CREATE TABLE "venue_nodes__deferrable" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,

    -- 'venue' (root) | 'stage' | 'run' | 'tower' | 'piece' | 'fixture' | 'array'.
    -- The closed alphabet of `luma_scene::venue::NodeKind`; a new set object is
    -- a 'piece' with sockets, never a new kind.
    kind TEXT NOT NULL CHECK (kind IN ('venue', 'stage', 'run', 'tower', 'piece', 'fixture', 'array')),

    -- What geometry this node has: a catalog piece id ('truss/straight',
    -- 'stage_lab/…​.glb') for structure, a `fixtures` row id for a fixture
    -- node. NULL on the root, which is the venue frame and has no geometry.
    catalog_ref TEXT,

    label TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),

    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "venue_nodes__deferrable" ("id", "uid", "venue_id", "kind", "catalog_ref", "label", "created_at", "updated_at") SELECT "id", "uid", "venue_id", "kind", "catalog_ref", "label", "created_at", "updated_at" FROM "venue_nodes";
DROP TABLE "venue_nodes";
ALTER TABLE "venue_nodes__deferrable" RENAME TO "venue_nodes";
CREATE INDEX idx_venue_nodes_venue ON venue_nodes(venue_id);
CREATE UNIQUE INDEX idx_venue_nodes_root ON venue_nodes(venue_id) WHERE kind = 'venue';
CREATE TRIGGER venue_nodes_updated_at AFTER UPDATE ON venue_nodes FOR EACH ROW
BEGIN UPDATE venue_nodes SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- venue_edges
CREATE TABLE "venue_edges__deferrable" (
    child_id TEXT PRIMARY KEY,
    uid TEXT,
    parent_id TEXT NOT NULL,
    my_socket TEXT NOT NULL,
    their_socket TEXT NOT NULL,
    roll REAL NOT NULL DEFAULT 0.0,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), id TEXT GENERATED ALWAYS AS (child_id) VIRTUAL,

    CHECK (child_id <> parent_id),
    FOREIGN KEY (child_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (parent_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "venue_edges__deferrable" ("child_id", "uid", "parent_id", "my_socket", "their_socket", "roll", "created_at", "updated_at") SELECT "child_id", "uid", "parent_id", "my_socket", "their_socket", "roll", "created_at", "updated_at" FROM "venue_edges";
DROP TABLE "venue_edges";
ALTER TABLE "venue_edges__deferrable" RENAME TO "venue_edges";
CREATE INDEX idx_venue_edges_parent ON venue_edges(parent_id);
CREATE UNIQUE INDEX idx_venue_edges_id ON venue_edges(id);
CREATE TRIGGER venue_edges_updated_at AFTER UPDATE ON venue_edges FOR EACH ROW
BEGIN UPDATE venue_edges SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE child_id = OLD.child_id; END;

-- venue_node_params
CREATE TABLE "venue_node_params__deferrable" (
    node_id TEXT NOT NULL,
    uid TEXT,
    key TEXT NOT NULL,
    value REAL NOT NULL,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), id TEXT GENERATED ALWAYS AS (node_id || ':' || key) VIRTUAL,

    PRIMARY KEY (node_id, key),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "venue_node_params__deferrable" ("node_id", "uid", "key", "value", "created_at", "updated_at") SELECT "node_id", "uid", "key", "value", "created_at", "updated_at" FROM "venue_node_params";
DROP TABLE "venue_node_params";
ALTER TABLE "venue_node_params__deferrable" RENAME TO "venue_node_params";
CREATE UNIQUE INDEX idx_venue_node_params_id ON venue_node_params(id);
CREATE TRIGGER venue_node_params_updated_at AFTER UPDATE ON venue_node_params FOR EACH ROW
BEGIN UPDATE venue_node_params SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE node_id = OLD.node_id AND key = OLD.key; END;

-- venue_constraints
CREATE TABLE "venue_constraints__deferrable" (
    node_id TEXT NOT NULL,
    uid TEXT,
    my_socket TEXT NOT NULL,
    target_node TEXT NOT NULL,
    target_socket TEXT NOT NULL,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), id TEXT GENERATED ALWAYS AS (node_id || ':' || my_socket) VIRTUAL,

    PRIMARY KEY (node_id, my_socket),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (target_node) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "venue_constraints__deferrable" ("node_id", "uid", "my_socket", "target_node", "target_socket", "created_at", "updated_at") SELECT "node_id", "uid", "my_socket", "target_node", "target_socket", "created_at", "updated_at" FROM "venue_constraints";
DROP TABLE "venue_constraints";
ALTER TABLE "venue_constraints__deferrable" RENAME TO "venue_constraints";
CREATE INDEX idx_venue_constraints_target ON venue_constraints(target_node);
CREATE UNIQUE INDEX idx_venue_constraints_id ON venue_constraints(id);
CREATE TRIGGER venue_constraints_updated_at AFTER UPDATE ON venue_constraints FOR EACH ROW
BEGIN UPDATE venue_constraints SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE node_id = OLD.node_id AND my_socket = OLD.my_socket; END;

-- track_beats
CREATE TABLE "track_beats__deferrable" (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
    beats_json TEXT NOT NULL,
    downbeats_json TEXT NOT NULL,
    bpm REAL,
    downbeat_offset REAL,
    beats_per_bar INTEGER,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processor_version INTEGER NOT NULL DEFAULT 1, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "track_beats__deferrable" ("track_id", "uid", "beats_json", "downbeats_json", "bpm", "downbeat_offset", "beats_per_bar", "created_at", "updated_at", "processor_version") SELECT "track_id", "uid", "beats_json", "downbeats_json", "bpm", "downbeat_offset", "beats_per_bar", "created_at", "updated_at", "processor_version" FROM "track_beats";
DROP TABLE "track_beats";
ALTER TABLE "track_beats__deferrable" RENAME TO "track_beats";
CREATE UNIQUE INDEX idx_track_beats_id ON track_beats(id);
CREATE TRIGGER track_beats_updated_at AFTER UPDATE ON track_beats FOR EACH ROW
BEGIN UPDATE track_beats SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

-- track_roots
CREATE TABLE "track_roots__deferrable" (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
    sections_json TEXT NOT NULL,
    logits_path TEXT,
    logits_storage_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processor_version INTEGER NOT NULL DEFAULT 1, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "track_roots__deferrable" ("track_id", "uid", "sections_json", "logits_path", "logits_storage_path", "created_at", "updated_at", "processor_version") SELECT "track_id", "uid", "sections_json", "logits_path", "logits_storage_path", "created_at", "updated_at", "processor_version" FROM "track_roots";
DROP TABLE "track_roots";
ALTER TABLE "track_roots__deferrable" RENAME TO "track_roots";
CREATE UNIQUE INDEX idx_track_roots_id ON track_roots(id);
CREATE TRIGGER track_roots_updated_at AFTER UPDATE ON track_roots FOR EACH ROW
BEGIN UPDATE track_roots SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

-- track_stems
CREATE TABLE "track_stems__deferrable" (
    track_id TEXT NOT NULL,
    uid TEXT,
    stem_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    storage_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processor_version INTEGER NOT NULL DEFAULT 1, id TEXT GENERATED ALWAYS AS (track_id || ':' || stem_name) VIRTUAL,
    PRIMARY KEY (track_id, stem_name),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "track_stems__deferrable" ("track_id", "uid", "stem_name", "file_path", "storage_path", "created_at", "updated_at", "processor_version") SELECT "track_id", "uid", "stem_name", "file_path", "storage_path", "created_at", "updated_at", "processor_version" FROM "track_stems";
DROP TABLE "track_stems";
ALTER TABLE "track_stems__deferrable" RENAME TO "track_stems";
CREATE UNIQUE INDEX idx_track_stems_id ON track_stems(id);
CREATE TRIGGER track_stems_updated_at AFTER UPDATE ON track_stems FOR EACH ROW
BEGIN UPDATE track_stems SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name; END;

-- track_drum_onsets
CREATE TABLE "track_drum_onsets__deferrable" (
    track_id          TEXT PRIMARY KEY,
    uid               TEXT,
    onsets_json       TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "track_drum_onsets__deferrable" ("track_id", "uid", "onsets_json", "processor_version", "created_at", "updated_at") SELECT "track_id", "uid", "onsets_json", "processor_version", "created_at", "updated_at" FROM "track_drum_onsets";
DROP TABLE "track_drum_onsets";
ALTER TABLE "track_drum_onsets__deferrable" RENAME TO "track_drum_onsets";
CREATE UNIQUE INDEX idx_track_drum_onsets_id ON track_drum_onsets(id);
CREATE TRIGGER track_drum_onsets_updated_at AFTER UPDATE ON track_drum_onsets FOR EACH ROW
BEGIN UPDATE track_drum_onsets SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

-- track_bar_classifications
CREATE TABLE "track_bar_classifications__deferrable" (
    track_id           TEXT PRIMARY KEY,
    uid                TEXT,
    classifications_json TEXT NOT NULL,
    tag_order_json     TEXT NOT NULL,
    processor_version  INTEGER NOT NULL DEFAULT 1,
    created_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "track_bar_classifications__deferrable" ("track_id", "uid", "classifications_json", "tag_order_json", "processor_version", "created_at", "updated_at") SELECT "track_id", "uid", "classifications_json", "tag_order_json", "processor_version", "created_at", "updated_at" FROM "track_bar_classifications";
DROP TABLE "track_bar_classifications";
ALTER TABLE "track_bar_classifications__deferrable" RENAME TO "track_bar_classifications";
CREATE UNIQUE INDEX idx_track_bar_classifications_id ON track_bar_classifications(id);
CREATE TRIGGER track_bar_classifications_updated_at AFTER UPDATE ON track_bar_classifications FOR EACH ROW
BEGIN UPDATE track_bar_classifications SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

-- track_genres
CREATE TABLE "track_genres__deferrable" (
    track_id          TEXT PRIMARY KEY,
    uid               TEXT,
    genres_json       TEXT NOT NULL,
    labels_json       TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "track_genres__deferrable" ("track_id", "uid", "genres_json", "labels_json", "processor_version", "created_at", "updated_at") SELECT "track_id", "uid", "genres_json", "labels_json", "processor_version", "created_at", "updated_at" FROM "track_genres";
DROP TABLE "track_genres";
ALTER TABLE "track_genres__deferrable" RENAME TO "track_genres";
CREATE UNIQUE INDEX idx_track_genres_id ON track_genres(id);
CREATE TRIGGER track_genres_updated_at AFTER UPDATE ON track_genres FOR EACH ROW
BEGIN UPDATE track_genres SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

-- track_beat_validations
CREATE TABLE "track_beat_validations__deferrable" (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    uid TEXT,
    track_hash TEXT NOT NULL,
    grid_json TEXT NOT NULL,
    processor_version INTEGER NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('unreviewed', 'correct', 'incorrect')),
    reason TEXT CHECK (reason IS NULL OR (verdict = 'incorrect' AND reason IN ('tempo', 'offset', 'drift', 'bar_phase'))),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL);
INSERT INTO "track_beat_validations__deferrable" ("track_id", "uid", "track_hash", "grid_json", "processor_version", "verdict", "reason", "created_at", "updated_at") SELECT "track_id", "uid", "track_hash", "grid_json", "processor_version", "verdict", "reason", "created_at", "updated_at" FROM "track_beat_validations";
DROP TABLE "track_beat_validations";
ALTER TABLE "track_beat_validations__deferrable" RENAME TO "track_beat_validations";
CREATE UNIQUE INDEX idx_track_beat_validations_id ON track_beat_validations(id);
CREATE TRIGGER track_beat_validations_updated_at AFTER UPDATE ON track_beat_validations FOR EACH ROW
BEGIN UPDATE track_beat_validations SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

-- patterns
CREATE TABLE "patterns__deferrable" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    name TEXT NOT NULL,
    description TEXT,
    category_id TEXT REFERENCES pattern_categories(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
    is_verified INTEGER NOT NULL DEFAULT 0,
    author_name TEXT,
    forked_from_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    category_name TEXT, score_id TEXT REFERENCES scores(id) DEFERRABLE INITIALLY DEFERRED);
INSERT INTO "patterns__deferrable" ("id", "uid", "name", "description", "category_id", "is_verified", "author_name", "forked_from_id", "created_at", "updated_at", "category_name", "score_id") SELECT "id", "uid", "name", "description", "category_id", "is_verified", "author_name", "forked_from_id", "created_at", "updated_at", "category_name", "score_id" FROM "patterns";
DROP TABLE "patterns";
ALTER TABLE "patterns__deferrable" RENAME TO "patterns";
CREATE INDEX patterns_score_id ON patterns(score_id);
CREATE TRIGGER patterns_updated_at AFTER UPDATE ON patterns FOR EACH ROW
BEGIN UPDATE patterns SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- implementations
CREATE TABLE "implementations__deferrable" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    pattern_id TEXT NOT NULL,
    name TEXT,
    graph_json TEXT NOT NULL DEFAULT '{"nodes":[],"edges":[]}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "implementations__deferrable" ("id", "uid", "pattern_id", "name", "graph_json", "created_at", "updated_at") SELECT "id", "uid", "pattern_id", "name", "graph_json", "created_at", "updated_at" FROM "implementations";
DROP TABLE "implementations";
ALTER TABLE "implementations__deferrable" RENAME TO "implementations";
CREATE INDEX idx_implementations_pattern ON implementations(pattern_id);
CREATE TRIGGER implementations_updated_at AFTER UPDATE ON implementations FOR EACH ROW
BEGIN UPDATE implementations SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- cues
CREATE TABLE "cues__deferrable" (
    id                  TEXT PRIMARY KEY,
    uid                 TEXT,
    venue_id            TEXT NOT NULL,
    name                TEXT NOT NULL,
    pattern_id          TEXT NOT NULL,
    args_json           TEXT NOT NULL DEFAULT '{}',
    z_index             INTEGER NOT NULL DEFAULT 1,
    blend_mode          TEXT NOT NULL DEFAULT 'Replace',
    default_target_json TEXT NOT NULL DEFAULT '"All"',
    execution_mode_json TEXT NOT NULL DEFAULT '"Loop"',
    display_order       INTEGER NOT NULL DEFAULT 0,
    display_x           INTEGER NOT NULL DEFAULT 0,
    display_y           INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "cues__deferrable" ("id", "uid", "venue_id", "name", "pattern_id", "args_json", "z_index", "blend_mode", "default_target_json", "execution_mode_json", "display_order", "display_x", "display_y", "created_at", "updated_at") SELECT "id", "uid", "venue_id", "name", "pattern_id", "args_json", "z_index", "blend_mode", "default_target_json", "execution_mode_json", "display_order", "display_x", "display_y", "created_at", "updated_at" FROM "cues";
DROP TABLE "cues";
ALTER TABLE "cues__deferrable" RENAME TO "cues";
CREATE INDEX idx_cues_venue ON cues(venue_id);
CREATE TRIGGER cues_updated_at AFTER UPDATE ON cues FOR EACH ROW
BEGIN UPDATE cues SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- midi_modifiers
CREATE TABLE "midi_modifiers__deferrable" (
    id          TEXT PRIMARY KEY,
    uid         TEXT,
    venue_id    TEXT NOT NULL,
    name        TEXT NOT NULL,
    input_json  TEXT NOT NULL,
    groups_json TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "midi_modifiers__deferrable" ("id", "uid", "venue_id", "name", "input_json", "groups_json", "created_at", "updated_at") SELECT "id", "uid", "venue_id", "name", "input_json", "groups_json", "created_at", "updated_at" FROM "midi_modifiers";
DROP TABLE "midi_modifiers";
ALTER TABLE "midi_modifiers__deferrable" RENAME TO "midi_modifiers";
CREATE INDEX idx_midi_modifiers_venue ON midi_modifiers(venue_id);
CREATE TRIGGER midi_modifiers_updated_at AFTER UPDATE ON midi_modifiers FOR EACH ROW
BEGIN UPDATE midi_modifiers SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- midi_bindings
CREATE TABLE "midi_bindings__deferrable" (
    id                      TEXT PRIMARY KEY,
    uid                     TEXT,
    venue_id                TEXT NOT NULL,
    trigger_json            TEXT NOT NULL,
    required_modifiers_json TEXT NOT NULL DEFAULT '[]',
    exclusive               INTEGER NOT NULL DEFAULT 0,
    mode_json               TEXT NOT NULL DEFAULT '"Toggle"',
    action_json             TEXT NOT NULL,
    target_override_json    TEXT,
    display_order           INTEGER NOT NULL DEFAULT 0,
    created_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO "midi_bindings__deferrable" ("id", "uid", "venue_id", "trigger_json", "required_modifiers_json", "exclusive", "mode_json", "action_json", "target_override_json", "display_order", "created_at", "updated_at") SELECT "id", "uid", "venue_id", "trigger_json", "required_modifiers_json", "exclusive", "mode_json", "action_json", "target_override_json", "display_order", "created_at", "updated_at" FROM "midi_bindings";
DROP TABLE "midi_bindings";
ALTER TABLE "midi_bindings__deferrable" RENAME TO "midi_bindings";
CREATE INDEX idx_midi_bindings_venue ON midi_bindings(venue_id);
CREATE TRIGGER midi_bindings_updated_at AFTER UPDATE ON midi_bindings FOR EACH ROW
BEGIN UPDATE midi_bindings SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- agent_thread_messages
CREATE TABLE "agent_thread_messages__deferrable" (
    id                   TEXT PRIMARY KEY,
    uid        TEXT,
    principal_key        TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    created_in_thread_id TEXT NOT NULL,
    parent_message_id    TEXT,
    depth                INTEGER NOT NULL CHECK (depth >= 0),
    role                 TEXT NOT NULL,
    parts_json           TEXT NOT NULL
        CHECK (json_valid(parts_json) AND json_type(parts_json) = 'array'),
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')), updated_at TEXT,
    FOREIGN KEY (parent_message_id) REFERENCES agent_thread_messages(id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (parent_message_id IS NULL AND depth = 0)
        OR (parent_message_id IS NOT NULL AND depth > 0)
    )
);
INSERT INTO "agent_thread_messages__deferrable" ("id", "uid", "principal_key", "created_in_thread_id", "parent_message_id", "depth", "role", "parts_json", "created_at", "updated_at") SELECT "id", "uid", "principal_key", "created_in_thread_id", "parent_message_id", "depth", "role", "parts_json", "created_at", "updated_at" FROM "agent_thread_messages";
DROP TABLE "agent_thread_messages";
ALTER TABLE "agent_thread_messages__deferrable" RENAME TO "agent_thread_messages";
CREATE INDEX idx_agent_thread_messages_parent
    ON agent_thread_messages(parent_message_id);
CREATE INDEX idx_agent_thread_messages_owner_created
    ON agent_thread_messages(uid, created_at, id);
CREATE INDEX idx_agent_thread_messages_principal_created
    ON agent_thread_messages(principal_key, created_at, id);
CREATE INDEX idx_agent_thread_messages_created_in_thread
    ON agent_thread_messages(created_in_thread_id, created_at, id);
CREATE TRIGGER agent_thread_messages_updated_at
AFTER UPDATE ON agent_thread_messages FOR EACH ROW
BEGIN UPDATE agent_thread_messages SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- agent_thread_transcript_heads
CREATE TABLE "agent_thread_transcript_heads__deferrable" (
    thread_id        TEXT PRIMARY KEY,
    uid    TEXT,
    head_message_id  TEXT,
    message_count    INTEGER NOT NULL DEFAULT 0 CHECK (message_count >= 0),
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')), created_at TEXT, id TEXT GENERATED ALWAYS AS (thread_id) VIRTUAL,
    FOREIGN KEY (thread_id) REFERENCES agent_threads(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (head_message_id) REFERENCES agent_thread_messages(id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (head_message_id IS NULL AND message_count = 0)
        OR (head_message_id IS NOT NULL AND message_count > 0)
    )
);
INSERT INTO "agent_thread_transcript_heads__deferrable" ("thread_id", "uid", "head_message_id", "message_count", "updated_at", "created_at") SELECT "thread_id", "uid", "head_message_id", "message_count", "updated_at", "created_at" FROM "agent_thread_transcript_heads";
DROP TABLE "agent_thread_transcript_heads";
ALTER TABLE "agent_thread_transcript_heads__deferrable" RENAME TO "agent_thread_transcript_heads";
CREATE INDEX idx_agent_thread_transcript_heads_owner
    ON agent_thread_transcript_heads(uid, updated_at DESC);
CREATE UNIQUE INDEX idx_agent_thread_transcript_heads_id
    ON agent_thread_transcript_heads(id);

