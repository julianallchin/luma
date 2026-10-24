-- The library schema as of 2026-10-24: the 123 migrations from
-- 20251220125200_create_tables through 20261023120000_dmx_routes, squashed.
-- A library migrated through all of them has this exact schema, and
-- `database::migrate_app_db_at` rewrites its history to this one row.
--
-- Tables come in creation order, each with its indexes; then views; then
-- triggers by table. Change the schema with a new migration after this one.

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE "venues" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    name TEXT NOT NULL,
    description TEXT,
    share_code TEXT,
    role TEXT NOT NULL DEFAULT 'owner',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    controller_port TEXT, mixer_port TEXT, mixer_mapping_json TEXT, environment TEXT NOT NULL DEFAULT '{"mode":"indoor","houseLevel":1.0}', groups_initialized INTEGER NOT NULL DEFAULT 0 CHECK (groups_initialized IN (0, 1)), haze TEXT NOT NULL DEFAULT '{"enabled":true,"density":0.24,"appearance":{"cloudiness":0.35,"cloudSize":4.0,"turbulence":0.3,"windSpeed":0.15,"windDirection":30.0}}');
CREATE UNIQUE INDEX idx_venues_share_code ON venues(share_code) WHERE share_code IS NOT NULL;

CREATE TABLE "track_waveforms" (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
    preview_samples_blob BLOB NOT NULL,
    full_samples_blob BLOB,
    bands_blob BLOB,
    preview_bands_blob BLOB,
    decoded_duration REAL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    band_gain_low REAL, band_gain_mid REAL, band_gain_high REAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TABLE "tracks" (
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
    album_art_storage_path TEXT);
CREATE INDEX idx_tracks_hash ON tracks(track_hash);

CREATE TABLE preprocessing_failures (
    track_id      TEXT NOT NULL,
    preprocessor  TEXT NOT NULL,
    version       INTEGER NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 1,
    last_error    TEXT NOT NULL,
    last_attempt  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    next_retry_at TEXT NOT NULL,
    PRIMARY KEY (track_id, preprocessor),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);
CREATE INDEX idx_preprocessing_failures_retry ON preprocessing_failures(next_retry_at);

CREATE TABLE track_mert (
    track_id          TEXT PRIMARY KEY,
    file_path         TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, drum_path TEXT NOT NULL DEFAULT '',
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TABLE stage_pieces (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,

    -- Path to GLB asset, relative to resources/meshes/ (e.g. "stage_lab/truss_q30_box.glb")
    mesh_path TEXT NOT NULL,

    -- Taxonomy used for snap rules and palette grouping
    -- ('floor' | 'truss' | 'speaker' | 'cdj' | 'mixer' | 'guardrail' | 'stand' | 'cable_cover')
    kind TEXT NOT NULL,

    label TEXT,

    pos_x REAL NOT NULL DEFAULT 0.0,
    pos_y REAL NOT NULL DEFAULT 0.0,
    pos_z REAL NOT NULL DEFAULT 0.0,
    rot_x REAL NOT NULL DEFAULT 0.0,
    rot_y REAL NOT NULL DEFAULT 0.0,
    rot_z REAL NOT NULL DEFAULT 0.0,
    scale REAL NOT NULL DEFAULT 1.0,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    parent_piece_id TEXT REFERENCES stage_pieces(id) ON DELETE CASCADE,

    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);
CREATE INDEX idx_stage_pieces_venue ON stage_pieces(venue_id);
CREATE INDEX idx_stage_pieces_parent ON stage_pieces(parent_piece_id);

CREATE TABLE agent_threads (
    id           TEXT PRIMARY KEY,          -- uuid v4
    agent_kind   TEXT NOT NULL,             -- 'track_copilot' | 'pattern_graph'
    subject_kind TEXT,                      -- 'track' | 'pattern' | NULL
    subject_id   TEXT,
    venue_id     TEXT,
    score_id     TEXT,
    title        TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
, uid TEXT, lifecycle_state TEXT NOT NULL DEFAULT 'active'
        CHECK (lifecycle_state IN ('active', 'deleting')), forked_from_thread_id TEXT, forked_at_message_id TEXT, actor TEXT, parent_thread_id TEXT, parent_call_id TEXT, engine TEXT NOT NULL DEFAULT 'api'
    CHECK (engine IN ('api', 'codex', 'claude')), model TEXT, provider TEXT
    CHECK (provider IN ('openrouter', 'vercel-ai-gateway', 'anthropic')), effort TEXT);
CREATE INDEX idx_agent_threads_subject ON agent_threads(subject_kind, subject_id);
CREATE INDEX idx_agent_threads_owner_updated
    ON agent_threads(uid, updated_at DESC);
CREATE INDEX idx_agent_threads_owner_lifecycle_updated
    ON agent_threads(uid, lifecycle_state, updated_at DESC);
CREATE INDEX idx_agent_threads_parent ON agent_threads(parent_thread_id);

CREATE TABLE auth_write_admission (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    armed      INTEGER NOT NULL DEFAULT 0 CHECK (armed IN (0, 1)),
    accepting  INTEGER NOT NULL DEFAULT 0 CHECK (accepting IN (0, 1)),
    maintenance INTEGER NOT NULL DEFAULT 0 CHECK (maintenance IN (0, 1)),
    remote_writes INTEGER NOT NULL DEFAULT 0 CHECK (remote_writes IN (0, 1)),
    active_uid TEXT,
    generation INTEGER NOT NULL DEFAULT 0,
    CHECK (maintenance = 0 OR (accepting = 0 AND remote_writes = 0)),
    CHECK (remote_writes = 0 OR (accepting = 1 AND maintenance = 0 AND active_uid IS NOT NULL))
);

CREATE TABLE relational_upgrade_archived_routes (
    legacy_repository_id TEXT PRIMARY KEY,
    document_kind        TEXT NOT NULL
        CHECK (document_kind IN ('track_score', 'pattern_graph')),
    principal_key        TEXT NOT NULL,
    subject_id           TEXT NOT NULL,
    track_id             TEXT,
    venue_id             TEXT,
    score_id             TEXT,
    implementation_id    TEXT,
    created_at           TEXT NOT NULL,
    archived_at          TEXT NOT NULL,
    CHECK (
        (
            document_kind = 'track_score'
            AND track_id IS NOT NULL
            AND venue_id IS NOT NULL
            AND score_id IS NOT NULL
            AND implementation_id IS NULL
        )
        OR
        (
            document_kind = 'pattern_graph'
            AND track_id IS NULL
            AND venue_id IS NULL
            AND score_id IS NULL
            AND implementation_id IS NOT NULL
        )
    )
);

CREATE TABLE "pending_ops_drain" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal_key TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    op_type TEXT NOT NULL CHECK(op_type IN (
        'upsert', 'delete', 'insert_immutable', 'upsert_explicit',
        'submit_authored_head_proposal', 'integrate_authored_head_proposal',
        'archive_authored_document'
    )),
    table_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    payload_json TEXT,
    conflict_key TEXT NOT NULL DEFAULT 'id',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_pending_ops_next_retry
    ON "pending_ops_drain"(principal_key, next_retry_at);
CREATE INDEX idx_pending_ops_table_record
    ON "pending_ops_drain"(principal_key, table_name, record_id);
CREATE UNIQUE INDEX idx_pending_ops_dedup
    ON "pending_ops_drain"(principal_key, table_name, record_id, op_type);

CREATE TABLE agent_thread_usage (
    thread_id             TEXT PRIMARY KEY,
    -- The model that ran, in whatever vocabulary the writer uses: a `ModelId`
    -- key from the in-app loop, the CLI's own model name from a harness.
    model                 TEXT,
    turns                 INTEGER NOT NULL DEFAULT 0,
    -- Anthropic's convention, the one `agent::model::Usage` already documents:
    -- the four counts do not overlap, and their sum is the whole spend.
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    -- NULL when nobody priced the run. Only a writer that is *told* the cost
    -- fills this in; nothing here estimates from a rate card, because a rate
    -- card in the tree is a second source of truth that silently rots.
    cost_usd              REAL,
    duration_ms           INTEGER NOT NULL DEFAULT 0,
    -- How many children the run fanned out to. Zero is "none", not "unknown".
    subagents             INTEGER NOT NULL DEFAULT 0,
    recorded_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE fixture_group_overrides (
    -- `derived_id(venue_id, path)`. Not a foreign key: the thing it names is
    -- derived, not stored, and a constraint against a row that does not exist
    -- is a constraint that cannot be written.
    group_id TEXT PRIMARY KEY,

    venue_id TEXT NOT NULL,

    -- The derivation path, '/'-joined: 'spots/left wing/top'.
    path TEXT NOT NULL,

    -- Rename. NULL keeps the derived label.
    label TEXT,

    -- Move: the group id this node now hangs under. NULL means "where
    -- derivation put it"; the empty string means the top level.
    parent_id TEXT,

    -- Merge: the group id this node's fixtures are counted under instead. The
    -- node itself stops being shown — saying *where* it went is what a plain
    -- "hidden" flag could not, and it is what lets the merge be undone by
    -- deleting one row.
    merged_into TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),

    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);
CREATE INDEX idx_fixture_group_overrides_venue ON fixture_group_overrides(venue_id);

CREATE TABLE agent_thread_runs (
    uid TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    PRIMARY KEY (uid, thread_id)
);

CREATE TABLE clips (
    id TEXT PRIMARY KEY,
    uid TEXT NOT NULL,
    score_id TEXT NOT NULL,
    graph TEXT NOT NULL,
    start REAL NOT NULL,
    duration REAL NOT NULL,
    seed TEXT NOT NULL,
    selection_seed TEXT,
    selection_json TEXT NOT NULL,
    z_index INTEGER NOT NULL DEFAULT 0,
    blend_mode TEXT NOT NULL,
    inputs_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')), name TEXT NOT NULL DEFAULT '', graph_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_clips_score ON clips(score_id);

CREATE TABLE drafts (
    id TEXT PRIMARY KEY,
    uid TEXT NOT NULL,
    score_id TEXT NOT NULL,
    thread_id TEXT,
    base_json TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_drafts_score ON drafts(score_id);
CREATE INDEX idx_drafts_thread ON drafts(thread_id);

CREATE TABLE venue_members (
    id TEXT PRIMARY KEY,
    uid TEXT NOT NULL,
    venue_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (venue_id, uid),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_venue_members_venue ON venue_members(venue_id);

CREATE TABLE "scores" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    track_id TEXT NOT NULL,
    venue_id TEXT,
    name TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')), authored_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_scores_track ON scores(track_id);
CREATE INDEX idx_scores_venue ON scores(venue_id);
CREATE INDEX idx_scores_track_venue ON scores(track_id, venue_id);

CREATE TABLE sync_rejections (
    id          TEXT PRIMARY KEY,
    table_name  TEXT NOT NULL,
    row_id      TEXT NOT NULL,
    op          TEXT NOT NULL,
    data_json   TEXT,
    status      INTEGER NOT NULL,
    body        TEXT,
    at          TEXT NOT NULL
);
CREATE INDEX idx_sync_rejections_at ON sync_rejections(at DESC);

CREATE TABLE "fixtures" (
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
CREATE INDEX idx_fixtures_venue ON fixtures(venue_id);
CREATE INDEX idx_fixtures_universe ON fixtures(universe);

CREATE TABLE "fixture_groups" (
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
CREATE INDEX idx_fixture_groups_venue ON fixture_groups(venue_id);
CREATE UNIQUE INDEX idx_fixture_groups_venue_name
    ON fixture_groups(venue_id, name);

CREATE TABLE "fixture_group_members" (
    id TEXT NOT NULL PRIMARY KEY,
    fixture_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    head_index INTEGER NOT NULL DEFAULT -1,
    display_order INTEGER NOT NULL DEFAULT 0,
    uid TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, venue_id TEXT,
    UNIQUE (fixture_id, group_id, head_index),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);
CREATE INDEX idx_fixture_group_members_venue ON fixture_group_members(venue_id);

CREATE TABLE "venue_node_params" (
    node_id TEXT NOT NULL,
    uid TEXT,
    key TEXT NOT NULL,
    value REAL NOT NULL,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), id TEXT GENERATED ALWAYS AS (node_id || ':' || key) VIRTUAL, venue_id TEXT,

    PRIMARY KEY (node_id, key),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX idx_venue_node_params_id ON venue_node_params(id);
CREATE INDEX idx_venue_node_params_venue ON venue_node_params(venue_id);

CREATE TABLE "venue_constraints" (
    node_id TEXT NOT NULL,
    uid TEXT,
    my_socket TEXT NOT NULL,
    target_node TEXT NOT NULL,
    target_socket TEXT NOT NULL,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')), id TEXT GENERATED ALWAYS AS (node_id || ':' || my_socket) VIRTUAL, venue_id TEXT,

    PRIMARY KEY (node_id, my_socket),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (target_node) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_venue_constraints_target ON venue_constraints(target_node);
CREATE UNIQUE INDEX idx_venue_constraints_id ON venue_constraints(id);
CREATE INDEX idx_venue_constraints_venue ON venue_constraints(venue_id);

CREATE TABLE "track_beats" (
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
CREATE UNIQUE INDEX idx_track_beats_id ON track_beats(id);

CREATE TABLE "track_roots" (
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
CREATE UNIQUE INDEX idx_track_roots_id ON track_roots(id);

CREATE TABLE "track_stems" (
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
CREATE UNIQUE INDEX idx_track_stems_id ON track_stems(id);

CREATE TABLE "track_drum_onsets" (
    track_id          TEXT PRIMARY KEY,
    uid               TEXT,
    onsets_json       TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX idx_track_drum_onsets_id ON track_drum_onsets(id);

CREATE TABLE "track_bar_classifications" (
    track_id           TEXT PRIMARY KEY,
    uid                TEXT,
    classifications_json TEXT NOT NULL,
    tag_order_json     TEXT NOT NULL,
    processor_version  INTEGER NOT NULL DEFAULT 1,
    created_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX idx_track_bar_classifications_id ON track_bar_classifications(id);

CREATE TABLE "track_genres" (
    track_id          TEXT PRIMARY KEY,
    uid               TEXT,
    genres_json       TEXT NOT NULL,
    labels_json       TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX idx_track_genres_id ON track_genres(id);

CREATE TABLE "track_beat_validations" (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    uid TEXT,
    track_hash TEXT NOT NULL,
    grid_json TEXT NOT NULL,
    processor_version INTEGER NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('unreviewed', 'correct', 'incorrect')),
    reason TEXT CHECK (reason IS NULL OR (verdict = 'incorrect' AND reason IN ('tempo', 'offset', 'drift', 'bar_phase'))),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL);
CREATE UNIQUE INDEX idx_track_beat_validations_id ON track_beat_validations(id);

CREATE TABLE "midi_modifiers" (
    id          TEXT PRIMARY KEY,
    uid         TEXT,
    venue_id    TEXT NOT NULL,
    name        TEXT NOT NULL,
    input_json  TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_midi_modifiers_venue ON midi_modifiers(venue_id);

CREATE TABLE "midi_bindings" (
    id                      TEXT PRIMARY KEY,
    uid                     TEXT,
    venue_id                TEXT NOT NULL,
    trigger_json            TEXT NOT NULL,
    required_modifiers_json TEXT NOT NULL DEFAULT '[]',
    exclusive               INTEGER NOT NULL DEFAULT 0,
    action_json             TEXT NOT NULL,
    display_order           INTEGER NOT NULL DEFAULT 0,
    created_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_midi_bindings_venue ON midi_bindings(venue_id);

CREATE TABLE "agent_thread_messages" (
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
CREATE INDEX idx_agent_thread_messages_parent
    ON agent_thread_messages(parent_message_id);
CREATE INDEX idx_agent_thread_messages_owner_created
    ON agent_thread_messages(uid, created_at, id);
CREATE INDEX idx_agent_thread_messages_principal_created
    ON agent_thread_messages(principal_key, created_at, id);
CREATE INDEX idx_agent_thread_messages_created_in_thread
    ON agent_thread_messages(created_in_thread_id, created_at, id);

CREATE TABLE "agent_thread_transcript_heads" (
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
CREATE INDEX idx_agent_thread_transcript_heads_owner
    ON agent_thread_transcript_heads(uid, updated_at DESC);
CREATE UNIQUE INDEX idx_agent_thread_transcript_heads_id
    ON agent_thread_transcript_heads(id);

CREATE TABLE folders (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_folders_venue ON folders(venue_id);
CREATE UNIQUE INDEX idx_folders_id_venue ON folders(id, venue_id);

CREATE TABLE folder_tracks (
    folder_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    uid TEXT,
    venue_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    id TEXT GENERATED ALWAYS AS (folder_id || ':' || track_id) VIRTUAL,
    PRIMARY KEY (folder_id, track_id),
    FOREIGN KEY (folder_id, venue_id) REFERENCES folders(id, venue_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX idx_folder_tracks_id ON folder_tracks(id);
CREATE INDEX idx_folder_tracks_venue ON folder_tracks(venue_id);
CREATE INDEX idx_folder_tracks_track ON folder_tracks(track_id);

CREATE TABLE "venue_nodes" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,

    -- 'venue' (root) | 'stage' | 'run' | 'tower' | 'piece' | 'fixture' | 'array'.
    -- The closed alphabet of `luma_scene::venue::NodeKind`; a new set object is
    -- a 'piece' with sockets, never a new kind.
    kind TEXT NOT NULL CHECK (kind IN ('venue', 'stage', 'run', 'tower', 'piece', 'fixture', 'array')),

    -- What geometry this node has: a catalog piece id ('truss/straight',
    -- 'stage_lab/….glb') for structure, a fixture's bundle path for a light.
    -- NULL on the root, which is the venue frame and has no geometry.
    catalog_ref TEXT,

    label TEXT,

    -- Where it hangs: the parent's socket it mates, by which of its own, and
    -- its turn about the shared normal. NULL on the root and only there.
    parent_id TEXT,
    my_socket TEXT,
    their_socket TEXT,
    roll REAL,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),

    CHECK (CASE WHEN kind = 'venue'
                THEN parent_id IS NULL AND my_socket IS NULL
                     AND their_socket IS NULL AND roll IS NULL
                ELSE parent_id IS NOT NULL AND my_socket IS NOT NULL
                     AND their_socket IS NOT NULL AND roll IS NOT NULL
           END),
    CHECK (parent_id <> id),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (parent_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_venue_nodes_venue ON venue_nodes(venue_id);
CREATE INDEX idx_venue_nodes_parent ON venue_nodes(parent_id);
CREATE UNIQUE INDEX idx_venue_nodes_root ON venue_nodes(venue_id) WHERE kind = 'venue';

CREATE TABLE agent_figure_uploads (
    path TEXT PRIMARY KEY NOT NULL
);

CREATE TABLE agent_open_messages (
    thread_id TEXT PRIMARY KEY NOT NULL,
    uid TEXT,
    message_id TEXT NOT NULL,
    parent_message_id TEXT,
    role TEXT NOT NULL,
    parts_json TEXT NOT NULL
);

CREATE TABLE dmx_routes (
    universe INTEGER NOT NULL CHECK (universe >= 0),
    device_kind TEXT NOT NULL CHECK (device_kind IN ('art-net', 'usb')),
    device_id TEXT NOT NULL,
    -- Art-Net: the node's own 15-bit port address. USB: the output's index.
    port INTEGER NOT NULL CHECK (port BETWEEN 0 AND 32767),
    device_name TEXT,
    -- Art-Net: the `ip:port` to send to until the node answers a poll.
    address TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (universe, device_kind, device_id, port)
);

CREATE VIEW auth_venue_access AS
SELECT
    venue.id AS venue_id,
    CASE
        WHEN admission.active_uid IS NULL
             AND venue.uid IS NULL
             AND venue.role != 'member'
            THEN 1
        WHEN admission.active_uid IS NOT NULL
             AND venue.uid IS admission.active_uid
            THEN 1
        ELSE 0
    END AS owner_access
FROM venues AS venue
CROSS JOIN auth_write_admission AS admission
WHERE admission.singleton = 1
  AND admission.armed = 1
  AND admission.accepting = 1
  AND admission.maintenance = 0
  AND admission.remote_writes = 0
  AND (
      (admission.active_uid IS NULL
       AND venue.uid IS NULL
       AND venue.role != 'member')
      OR
      (admission.active_uid IS NOT NULL AND (
          venue.uid IS admission.active_uid
          OR EXISTS (
              SELECT 1
              FROM venue_members AS membership
              WHERE membership.venue_id = venue.id
                AND membership.uid = admission.active_uid
          )
      ))
  );

CREATE VIEW auth_visible_tracks AS
SELECT DISTINCT track.id AS track_id
FROM tracks AS track
CROSS JOIN auth_write_admission AS admission
WHERE admission.singleton = 1
  AND admission.armed = 1
  AND admission.accepting = 1
  AND admission.maintenance = 0
  AND admission.remote_writes = 0
  AND (
      track.uid IS admission.active_uid
      OR EXISTS (
          SELECT 1
          FROM scores AS score
          JOIN auth_venue_access AS access ON access.venue_id = score.venue_id
          WHERE score.track_id = track.id
      )
  );

CREATE TRIGGER venues_updated_at AFTER UPDATE ON venues FOR EACH ROW
BEGIN UPDATE venues SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER auth_admit_track_waveforms_insert BEFORE INSERT ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_waveforms_update BEFORE UPDATE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_waveforms_delete BEFORE DELETE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER track_waveforms_updated_at AFTER UPDATE ON track_waveforms FOR EACH ROW
BEGIN UPDATE track_waveforms SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER tracks_updated_at AFTER UPDATE ON tracks FOR EACH ROW
BEGIN UPDATE tracks SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER track_mert_updated_at AFTER UPDATE ON track_mert FOR EACH ROW
    BEGIN
        UPDATE track_mert
            SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
        WHERE track_id = OLD.track_id;
    END;

CREATE TRIGGER auth_venue_stage_piece_insert
BEFORE INSERT ON stage_pieces FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
      OR (NEW.parent_piece_id IS NOT NULL AND NOT EXISTS (
          SELECT 1 FROM stage_pieces AS parent
          WHERE parent.id = NEW.parent_piece_id AND parent.venue_id = NEW.venue_id
      ))
 )
BEGIN SELECT RAISE(ABORT, 'stage piece write is not authorized'); END;

CREATE TRIGGER auth_venue_stage_piece_update
BEFORE UPDATE ON stage_pieces FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
      OR (NEW.parent_piece_id IS NOT NULL AND NOT EXISTS (
          SELECT 1 FROM stage_pieces AS parent
          WHERE parent.id = NEW.parent_piece_id AND parent.venue_id = NEW.venue_id
      ))
 )
BEGIN SELECT RAISE(ABORT, 'stage piece write is not authorized'); END;

CREATE TRIGGER auth_venue_stage_piece_delete
BEFORE DELETE ON stage_pieces FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'stage piece write is not authorized'); END;

CREATE TRIGGER stage_pieces_updated_at AFTER UPDATE ON stage_pieces FOR EACH ROW
BEGIN UPDATE stage_pieces SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER agent_threads_updated_at
AFTER UPDATE ON agent_threads FOR EACH ROW
WHEN COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    UPDATE agent_threads
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
    WHERE id = OLD.id;
END;

CREATE TRIGGER agent_thread_create_empty_transcript
AFTER INSERT ON agent_threads FOR EACH ROW
BEGIN
    INSERT OR IGNORE INTO agent_thread_transcript_heads (thread_id, uid, created_at, updated_at)
    VALUES (NEW.id, NEW.uid, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
            strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;

CREATE TRIGGER auth_group_override_insert
BEFORE INSERT ON fixture_group_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'group override write is not authorized'); END;

CREATE TRIGGER auth_group_override_update
BEFORE UPDATE ON fixture_group_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.group_id IS NOT OLD.group_id OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
 )
BEGIN SELECT RAISE(ABORT, 'group override write is not authorized'); END;

CREATE TRIGGER auth_group_override_delete
BEFORE DELETE ON fixture_group_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'group override write is not authorized'); END;

CREATE TRIGGER fixture_group_overrides_updated_at AFTER UPDATE ON fixture_group_overrides FOR EACH ROW
BEGIN UPDATE fixture_group_overrides SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE group_id = OLD.group_id; END;

CREATE TRIGGER clips_updated_at AFTER UPDATE ON clips FOR EACH ROW
BEGIN UPDATE clips SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER drafts_updated_at AFTER UPDATE ON drafts FOR EACH ROW
BEGIN UPDATE drafts SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER venue_members_updated_at AFTER UPDATE ON venue_members FOR EACH ROW
BEGIN UPDATE venue_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER scores_updated_at AFTER UPDATE ON scores FOR EACH ROW
BEGIN UPDATE scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

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

CREATE TRIGGER fixture_groups_updated_at AFTER UPDATE ON fixture_groups FOR EACH ROW
BEGIN UPDATE fixture_groups SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
BEGIN UPDATE fixture_group_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER venue_node_params_updated_at AFTER UPDATE ON venue_node_params FOR EACH ROW
BEGIN UPDATE venue_node_params SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE node_id = OLD.node_id AND key = OLD.key; END;

CREATE TRIGGER venue_constraints_updated_at AFTER UPDATE ON venue_constraints FOR EACH ROW
BEGIN UPDATE venue_constraints SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE node_id = OLD.node_id AND my_socket = OLD.my_socket; END;

CREATE TRIGGER track_beats_updated_at AFTER UPDATE ON track_beats FOR EACH ROW
BEGIN UPDATE track_beats SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_roots_updated_at AFTER UPDATE ON track_roots FOR EACH ROW
BEGIN UPDATE track_roots SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_stems_updated_at AFTER UPDATE ON track_stems FOR EACH ROW
BEGIN UPDATE track_stems SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name; END;

CREATE TRIGGER track_drum_onsets_updated_at AFTER UPDATE ON track_drum_onsets FOR EACH ROW
BEGIN UPDATE track_drum_onsets SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_bar_classifications_updated_at AFTER UPDATE ON track_bar_classifications FOR EACH ROW
BEGIN UPDATE track_bar_classifications SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_genres_updated_at AFTER UPDATE ON track_genres FOR EACH ROW
BEGIN UPDATE track_genres SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_beat_validations_updated_at AFTER UPDATE ON track_beat_validations FOR EACH ROW
BEGIN UPDATE track_beat_validations SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

CREATE TRIGGER midi_modifiers_updated_at AFTER UPDATE ON midi_modifiers FOR EACH ROW
BEGIN UPDATE midi_modifiers SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER midi_bindings_updated_at AFTER UPDATE ON midi_bindings FOR EACH ROW
BEGIN UPDATE midi_bindings SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER agent_thread_messages_updated_at
AFTER UPDATE ON agent_thread_messages FOR EACH ROW
BEGIN UPDATE agent_thread_messages SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER folders_updated_at AFTER UPDATE ON folders FOR EACH ROW
BEGIN UPDATE folders SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TRIGGER venue_nodes_updated_at AFTER UPDATE ON venue_nodes FOR EACH ROW
BEGIN UPDATE venue_nodes SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

INSERT INTO auth_write_admission (singleton, armed, accepting, maintenance, remote_writes, active_uid, generation) VALUES (1,0,0,0,0,NULL,0);
