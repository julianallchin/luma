-- The row model. Scores become rows, the authored-document revision system and
-- the custom sync engine are removed, and the legacy `track_scores` format is
-- retired into a local-only backup table.
--
-- Graph score documents are parked in `scores_pending_cutover` rather than split
-- here: SQLite's `json_extract` cannot return a u64 seed without losing
-- precision, and a document older than the current `Score::VERSION` needs the
-- Rust migration in `luma_patterns`. `database::local::scores::cutover` finishes
-- the conversion on the next startup.

-- ---------------------------------------------------------------------------
-- New tables
-- ---------------------------------------------------------------------------

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
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_clips_score ON clips(score_id);

CREATE TABLE score_definitions (
    id TEXT PRIMARY KEY,
    uid TEXT NOT NULL,
    score_id TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_score_definitions_score ON score_definitions(score_id);

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

-- No foreign keys: a change outlives the row it describes.
CREATE TABLE changes (
    id TEXT PRIMARY KEY,
    uid TEXT NOT NULL,
    table_name TEXT NOT NULL,
    row_id TEXT NOT NULL,
    op TEXT NOT NULL,
    before_json TEXT,
    after_json TEXT,
    actor TEXT,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_changes_row ON changes(table_name, row_id);
CREATE INDEX idx_changes_at ON changes(at);

-- `uid` is the member: a membership row belongs to the person it admits, so
-- there is no second `user_id` column saying the same thing.
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

-- ---------------------------------------------------------------------------
-- Memberships become rows with an id
-- ---------------------------------------------------------------------------

DROP VIEW auth_venue_access;
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

INSERT INTO venue_members (id, uid, venue_id, role, created_at, updated_at)
SELECT m.venue_id || ':' || m.user_id, m.user_id, m.venue_id, m.role, m.created_at, m.created_at
FROM venue_memberships AS m;

DROP TABLE venue_memberships;

-- A membership is granted by the server, never by the device: only a remote
-- write may add one, only to the signed-in principal, and only for a venue
-- somebody else owns.
CREATE TRIGGER auth_venue_membership_insert
BEFORE INSERT ON venue_members FOR EACH ROW
WHEN NOT COALESCE((
    SELECT armed = 1 AND accepting = 1 AND maintenance = 0
           AND remote_writes = 1 AND active_uid IS NOT NULL
           AND NEW.uid IS active_uid AND NEW.role = 'member'
           AND EXISTS (
               SELECT 1 FROM venues AS venue
               WHERE venue.id = NEW.venue_id
                 AND venue.uid IS NOT NEW.uid
           )
    FROM auth_write_admission WHERE singleton = 1
), 0)
BEGIN SELECT RAISE(ABORT, 'venue membership grant is not authorized'); END;

CREATE TRIGGER auth_venue_membership_update
BEFORE UPDATE ON venue_members FOR EACH ROW
WHEN NEW.venue_id IS NOT OLD.venue_id
  OR NEW.uid IS NOT OLD.uid
  OR NEW.role IS NOT OLD.role
  OR NEW.role != 'member'
  OR NOT COALESCE((
      SELECT armed = 1 AND accepting = 1 AND maintenance = 0
             AND remote_writes = 1 AND active_uid IS NOT NULL
             AND OLD.uid IS active_uid
      FROM auth_write_admission WHERE singleton = 1
  ), 0)
BEGIN SELECT RAISE(ABORT, 'venue membership update is not authorized'); END;

CREATE TRIGGER auth_venue_membership_delete
BEFORE DELETE ON venue_members FOR EACH ROW
WHEN NOT COALESCE((
    SELECT armed = 1 AND (
        maintenance = 1
        OR (
            accepting = 1 AND maintenance = 0
            AND active_uid IS NOT NULL AND OLD.uid IS active_uid
        )
    )
    FROM auth_write_admission WHERE singleton = 1
), 0)
BEGIN SELECT RAISE(ABORT, 'venue membership deletion is not authorized'); END;

-- ---------------------------------------------------------------------------
-- Retire the legacy score format
-- ---------------------------------------------------------------------------

CREATE TABLE legacy_scores_backup (
    score_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL
);

INSERT INTO legacy_scores_backup (score_id, payload_json)
SELECT
    s.id,
    json_object(
        'score', json_object(
            'id', s.id,
            'uid', s.uid,
            'track_id', s.track_id,
            'venue_id', s.venue_id,
            'name', s.name,
            'created_at', s.created_at,
            'updated_at', s.updated_at
        ),
        'track_scores', (
            SELECT json_group_array(json_object(
                'id', t.id,
                'uid', t.uid,
                'score_id', t.score_id,
                'pattern_id', t.pattern_id,
                'start_time', t.start_time,
                'end_time', t.end_time,
                'z_index', t.z_index,
                'blend_mode', t.blend_mode,
                'args_json', t.args_json,
                'created_at', t.created_at,
                'updated_at', t.updated_at
            ))
            FROM track_scores AS t WHERE t.score_id = s.id
        )
    )
FROM scores AS s
WHERE s.graph_document_json IS NULL
  AND EXISTS (SELECT 1 FROM track_scores AS t WHERE t.score_id = s.id);

DROP VIEW auth_visible_patterns;
DROP TRIGGER auth_venue_track_score_insert;
DROP TRIGGER auth_venue_track_score_update;
DROP TRIGGER auth_venue_track_score_delete;
DROP TRIGGER clip_pattern_scope_insert;
DROP TRIGGER clip_pattern_scope_update;
DROP TRIGGER graph_score_rejects_legacy_clip_insert;
DROP TRIGGER graph_score_rejects_legacy_clip_update;
DROP TABLE track_scores;

CREATE VIEW auth_visible_patterns AS
SELECT DISTINCT pattern.id AS pattern_id
FROM patterns AS pattern
CROSS JOIN auth_write_admission AS admission
WHERE admission.singleton = 1
  AND admission.armed = 1
  AND admission.accepting = 1
  AND admission.maintenance = 0
  AND admission.remote_writes = 0
  AND (
      pattern.uid IS admission.active_uid
      OR pattern.is_verified = 1
      OR EXISTS (
          SELECT 1
          FROM cues AS cue
          JOIN auth_venue_access AS access ON access.venue_id = cue.venue_id
          WHERE cue.pattern_id = pattern.id
      )
      OR EXISTS (
          SELECT 1
          FROM venue_implementation_overrides AS override
          JOIN auth_venue_access AS access ON access.venue_id = override.venue_id
          WHERE override.pattern_id = pattern.id
      )
  );

-- ---------------------------------------------------------------------------
-- Retire the authored-document system and the custom sync engine
-- ---------------------------------------------------------------------------

DROP TRIGGER IF EXISTS authored_document_identity_is_immutable;
DROP TRIGGER IF EXISTS authored_document_history_is_permanent;
DROP TRIGGER IF EXISTS authored_document_archive_is_terminal;
DROP TRIGGER IF EXISTS authored_document_archive_is_immutable;
DROP TRIGGER IF EXISTS authored_document_archive_is_permanent;
DROP TRIGGER IF EXISTS authored_document_head_requires_active_document;
DROP TRIGGER IF EXISTS authored_document_head_advance_requires_active_document;
DROP TRIGGER IF EXISTS authored_document_head_identity_is_immutable;
DROP TRIGGER IF EXISTS authored_document_head_is_permanent;
DROP TRIGGER IF EXISTS authored_document_head_is_strict_cas_counter;
DROP TRIGGER IF EXISTS authored_document_head_updated_at;
DROP TRIGGER IF EXISTS authored_revision_is_permanent;
DROP TRIGGER IF EXISTS authored_revision_is_immutable;
DROP TRIGGER IF EXISTS authored_revision_restore_stays_in_document;
DROP TRIGGER IF EXISTS authored_revision_file_is_permanent;
DROP TRIGGER IF EXISTS authored_revision_file_is_immutable;
DROP TRIGGER IF EXISTS authored_revision_parent_matches_declared_shape;
DROP TRIGGER IF EXISTS authored_revision_parent_is_permanent;
DROP TRIGGER IF EXISTS authored_revision_parent_is_immutable;
DROP TRIGGER IF EXISTS authored_turn_preparation_is_permanent;
DROP TRIGGER IF EXISTS authored_turn_preparation_is_immutable;
DROP TRIGGER IF EXISTS authored_turn_preparation_requires_admitted_scope;
DROP TRIGGER IF EXISTS authored_turn_message_reservation_requires_unused_id;
DROP TRIGGER IF EXISTS authored_turn_message_id_cannot_be_reused;
DROP TRIGGER IF EXISTS authored_turn_outcome_is_permanent;
DROP TRIGGER IF EXISTS authored_turn_outcome_is_immutable;
DROP TRIGGER IF EXISTS authored_turn_outcome_matches_persisted_assistant;
DROP TRIGGER IF EXISTS authored_turn_outcome_insert_requires_owner_admission;
DROP TRIGGER IF EXISTS authored_operation_principal_matches_document;
DROP TRIGGER IF EXISTS authored_operation_outcome_is_permanent;
DROP TRIGGER IF EXISTS authored_operation_outcome_is_immutable;
DROP TRIGGER IF EXISTS authored_workspace_scope_matches_thread;
DROP TRIGGER IF EXISTS authored_subagent_workspace_identity_is_immutable;
DROP TRIGGER IF EXISTS authored_subagent_workspace_transition_is_strict;
DROP TRIGGER IF EXISTS authored_head_proposal_is_permanent;
DROP TRIGGER IF EXISTS authored_head_proposal_identity_is_immutable;
DROP TRIGGER IF EXISTS authored_head_proposal_principal_matches_document;
DROP TRIGGER IF EXISTS authored_head_integration_principal_matches_proposal;
DROP TRIGGER IF EXISTS authored_head_integration_is_immutable;
DROP TRIGGER IF EXISTS authored_head_integration_is_permanent;
DROP TRIGGER IF EXISTS authored_archive_principal_matches_document;
DROP TRIGGER IF EXISTS authored_device_identity_is_immutable;
DROP TRIGGER IF EXISTS authored_device_identity_is_permanent;
DROP TRIGGER IF EXISTS authored_track_identity_immutable;
DROP TRIGGER IF EXISTS authored_pattern_identity_immutable;
DROP TRIGGER IF EXISTS authored_implementation_identity_immutable;
DROP TRIGGER IF EXISTS authored_score_identity_immutable;
DROP TRIGGER IF EXISTS assistant_message_requires_prepared_authored_turn;
DROP TRIGGER IF EXISTS prevent_archived_score_resurrection;
DROP TRIGGER IF EXISTS prevent_archived_pattern_resurrection;
DROP TRIGGER IF EXISTS require_archived_score_before_delete;
DROP TRIGGER IF EXISTS require_archived_pattern_before_delete;
DROP TRIGGER IF EXISTS require_archived_implementation_before_delete;
DROP TRIGGER IF EXISTS agent_threads_validate_authored_route_insert;
DROP TRIGGER IF EXISTS agent_threads_validate_authored_route_update;
DROP TRIGGER IF EXISTS agent_thread_deletion_receipt_requires_terminal_scope;
DROP TRIGGER IF EXISTS agent_thread_deletion_receipt_is_permanent;
DROP TRIGGER IF EXISTS agent_thread_deletion_receipt_is_immutable;
DROP TRIGGER IF EXISTS agent_thread_deletion_is_terminal;
DROP TRIGGER IF EXISTS agent_thread_append_has_valid_range;
DROP TRIGGER IF EXISTS agent_thread_append_receipt_is_immutable;
DROP TRIGGER IF EXISTS agent_thread_append_insert_requires_owner_admission;
DROP TRIGGER IF EXISTS agent_thread_append_delete_requires_owner_admission;
DROP TRIGGER IF EXISTS active_agent_thread_append_receipt_cannot_be_deleted;
-- A retried conversation write is an upsert, and a deleted thread takes its
-- messages with it.
DROP TRIGGER IF EXISTS agent_thread_message_cannot_be_updated;
DROP TRIGGER IF EXISTS agent_thread_message_cannot_be_deleted;
DROP TRIGGER IF EXISTS auth_admit_sync_state_insert;
DROP TRIGGER IF EXISTS auth_admit_sync_state_update;
DROP TRIGGER IF EXISTS auth_admit_sync_state_delete;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_venues;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_tracks;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_fixtures;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_patterns;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_fixture_groups;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_fixture_group_members;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_scores;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_cues;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_midi_modifiers;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_midi_bindings;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_venue_nodes;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_venue_edges;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_venue_node_params;
DROP TRIGGER IF EXISTS guard_unrecorded_delete_venue_constraints;

DROP TABLE IF EXISTS authored_head_integrations;
DROP TABLE IF EXISTS authored_head_proposals;
DROP TABLE IF EXISTS authored_document_archives;
DROP TABLE IF EXISTS authored_subagent_workspaces;
DROP TABLE IF EXISTS authored_operation_outcomes;
DROP TABLE IF EXISTS authored_turn_outcomes;
DROP TABLE IF EXISTS authored_turn_preparations;
DROP TABLE IF EXISTS authored_revision_parents;
DROP TABLE IF EXISTS authored_revision_files;
DROP TABLE IF EXISTS authored_document_heads;
DROP TABLE IF EXISTS authored_revisions;
DROP TABLE IF EXISTS authored_documents;
DROP TABLE IF EXISTS authored_device_identity;
DROP TABLE IF EXISTS agent_thread_message_appends;
DROP TABLE IF EXISTS agent_thread_deletions;
DROP TABLE IF EXISTS pending_ops;
DROP TABLE IF EXISTS pending_ops_principalized;
DROP TABLE IF EXISTS sync_push_failures;
DROP TABLE IF EXISTS sync_state;
DROP TABLE IF EXISTS sync_tombstones;
DROP TABLE IF EXISTS venue_constraints_synced;
DROP TABLE IF EXISTS venue_edges_synced;
DROP TABLE IF EXISTS venue_node_params_synced;

-- ---------------------------------------------------------------------------
-- Rebuild `scores` without the document column or the sync bookkeeping
-- ---------------------------------------------------------------------------

DROP VIEW auth_visible_tracks;

CREATE TABLE scores_pending_cutover (
    score_id TEXT PRIMARY KEY,
    document_json TEXT NOT NULL
);

INSERT INTO scores_pending_cutover (score_id, document_json)
SELECT id, graph_document_json FROM scores WHERE graph_document_json IS NOT NULL;

DELETE FROM scores WHERE graph_document_json IS NULL;

DROP TRIGGER scores_updated_at;
DROP TRIGGER auth_venue_score_insert;
DROP TRIGGER auth_venue_score_update;
DROP TRIGGER auth_venue_score_delete;
DROP TRIGGER pattern_score_owner_insert;
DROP TRIGGER pattern_score_owner_update;
DROP INDEX idx_scores_track;
DROP INDEX idx_scores_venue;
DROP INDEX idx_scores_track_venue;

CREATE TABLE scores_new (
    id TEXT PRIMARY KEY,
    uid TEXT,
    track_id TEXT NOT NULL,
    venue_id TEXT,
    name TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO scores_new (id, uid, track_id, venue_id, name, created_at, updated_at)
SELECT id, uid, track_id, venue_id, name, created_at, updated_at FROM scores;
DROP TABLE scores;
ALTER TABLE scores_new RENAME TO scores;

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

CREATE INDEX idx_scores_track ON scores(track_id);
CREATE INDEX idx_scores_venue ON scores(venue_id);
CREATE INDEX idx_scores_track_venue ON scores(track_id, venue_id);

CREATE TRIGGER auth_venue_score_insert
BEFORE INSERT ON scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'score write is not authorized'); END;

CREATE TRIGGER auth_venue_score_update
BEFORE UPDATE ON scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid
      OR NEW.venue_id IS NOT OLD.venue_id OR NEW.track_id IS NOT OLD.track_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'score write is not authorized'); END;

CREATE TRIGGER auth_venue_score_delete
BEFORE DELETE ON scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'score write is not authorized'); END;

CREATE TRIGGER pattern_score_owner_insert BEFORE INSERT ON patterns
WHEN NEW.score_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM scores WHERE id = NEW.score_id AND uid IS NEW.uid
)
BEGIN SELECT RAISE(ABORT, 'A local Pattern must have the same owner as its score'); END;

CREATE TRIGGER pattern_score_owner_update BEFORE UPDATE OF score_id, uid ON patterns
WHEN NEW.score_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM scores WHERE id = NEW.score_id AND uid IS NEW.uid
)
BEGIN SELECT RAISE(ABORT, 'A local Pattern must have the same owner as its score'); END;

-- ---------------------------------------------------------------------------
-- `version` / `synced_at` / `origin` leave every table that carries them
-- ---------------------------------------------------------------------------

DROP TRIGGER implementations_updated_at;
DROP TRIGGER venue_implementation_overrides_updated_at;
DROP TRIGGER track_waveforms_updated_at;
DROP TRIGGER stage_pieces_updated_at;
DROP TRIGGER venues_updated_at;
DROP TRIGGER tracks_updated_at;
DROP TRIGGER fixtures_updated_at;
DROP TRIGGER pattern_categories_updated_at;
DROP TRIGGER patterns_updated_at;
DROP TRIGGER fixture_groups_updated_at;
DROP TRIGGER track_beats_updated_at;
DROP TRIGGER track_roots_updated_at;
DROP TRIGGER track_stems_updated_at;
DROP TRIGGER track_drum_onsets_updated_at;
DROP TRIGGER track_bar_classifications_updated_at;
DROP TRIGGER track_genres_updated_at;
DROP TRIGGER track_beat_validations_updated_at;
DROP TRIGGER fixture_group_members_updated_at;
DROP TRIGGER fixture_group_overrides_updated_at;
DROP TRIGGER cues_updated_at;
DROP TRIGGER midi_modifiers_updated_at;
DROP TRIGGER midi_bindings_updated_at;
DROP TRIGGER venue_nodes_updated_at;
DROP TRIGGER venue_edges_updated_at;
DROP TRIGGER venue_node_params_updated_at;
DROP TRIGGER venue_constraints_updated_at;

ALTER TABLE venues DROP COLUMN version;
ALTER TABLE venues DROP COLUMN synced_at;
ALTER TABLE venues DROP COLUMN origin;
ALTER TABLE tracks DROP COLUMN version;
ALTER TABLE tracks DROP COLUMN synced_at;
ALTER TABLE tracks DROP COLUMN origin;
ALTER TABLE fixtures DROP COLUMN version;
ALTER TABLE fixtures DROP COLUMN synced_at;
ALTER TABLE fixtures DROP COLUMN origin;
ALTER TABLE fixture_groups DROP COLUMN version;
ALTER TABLE fixture_groups DROP COLUMN synced_at;
ALTER TABLE fixture_groups DROP COLUMN origin;
ALTER TABLE fixture_group_members DROP COLUMN version;
ALTER TABLE fixture_group_members DROP COLUMN synced_at;
ALTER TABLE fixture_group_members DROP COLUMN origin;
ALTER TABLE fixture_group_overrides DROP COLUMN version;
ALTER TABLE fixture_group_overrides DROP COLUMN synced_at;
ALTER TABLE patterns DROP COLUMN version;
ALTER TABLE patterns DROP COLUMN synced_at;
ALTER TABLE patterns DROP COLUMN origin;
ALTER TABLE pattern_categories DROP COLUMN version;
ALTER TABLE pattern_categories DROP COLUMN synced_at;
ALTER TABLE implementations DROP COLUMN version;
ALTER TABLE implementations DROP COLUMN synced_at;
ALTER TABLE implementations DROP COLUMN origin;
ALTER TABLE venue_implementation_overrides DROP COLUMN version;
ALTER TABLE venue_implementation_overrides DROP COLUMN synced_at;
ALTER TABLE cues DROP COLUMN version;
ALTER TABLE cues DROP COLUMN synced_at;
ALTER TABLE cues DROP COLUMN origin;
ALTER TABLE midi_modifiers DROP COLUMN version;
ALTER TABLE midi_modifiers DROP COLUMN synced_at;
ALTER TABLE midi_modifiers DROP COLUMN origin;
ALTER TABLE midi_bindings DROP COLUMN version;
ALTER TABLE midi_bindings DROP COLUMN synced_at;
ALTER TABLE midi_bindings DROP COLUMN origin;
ALTER TABLE stage_pieces DROP COLUMN version;
ALTER TABLE stage_pieces DROP COLUMN synced_at;
ALTER TABLE track_waveforms DROP COLUMN version;
ALTER TABLE track_waveforms DROP COLUMN synced_at;
ALTER TABLE track_beats DROP COLUMN version;
ALTER TABLE track_beats DROP COLUMN synced_at;
ALTER TABLE track_beats DROP COLUMN origin;
ALTER TABLE track_roots DROP COLUMN version;
ALTER TABLE track_roots DROP COLUMN synced_at;
ALTER TABLE track_roots DROP COLUMN origin;
ALTER TABLE track_stems DROP COLUMN version;
ALTER TABLE track_stems DROP COLUMN synced_at;
ALTER TABLE track_stems DROP COLUMN origin;
ALTER TABLE track_drum_onsets DROP COLUMN version;
ALTER TABLE track_drum_onsets DROP COLUMN synced_at;
ALTER TABLE track_drum_onsets DROP COLUMN origin;
ALTER TABLE track_bar_classifications DROP COLUMN version;
ALTER TABLE track_bar_classifications DROP COLUMN synced_at;
ALTER TABLE track_bar_classifications DROP COLUMN origin;
ALTER TABLE track_genres DROP COLUMN version;
ALTER TABLE track_genres DROP COLUMN synced_at;
ALTER TABLE track_genres DROP COLUMN origin;
ALTER TABLE track_beat_validations DROP COLUMN version;
ALTER TABLE track_beat_validations DROP COLUMN synced_at;
ALTER TABLE track_beat_validations DROP COLUMN origin;
ALTER TABLE venue_nodes DROP COLUMN version;
ALTER TABLE venue_nodes DROP COLUMN synced_at;
ALTER TABLE venue_nodes DROP COLUMN origin;
ALTER TABLE venue_edges DROP COLUMN version;
ALTER TABLE venue_edges DROP COLUMN synced_at;
ALTER TABLE venue_edges DROP COLUMN origin;
ALTER TABLE venue_node_params DROP COLUMN version;
ALTER TABLE venue_node_params DROP COLUMN synced_at;
ALTER TABLE venue_node_params DROP COLUMN origin;
ALTER TABLE venue_constraints DROP COLUMN version;
ALTER TABLE venue_constraints DROP COLUMN synced_at;
ALTER TABLE venue_constraints DROP COLUMN origin;
ALTER TABLE agent_threads DROP COLUMN synced_at;
ALTER TABLE agent_thread_messages DROP COLUMN synced_at;

-- Every synced table has exactly one owner column, and it is called `uid`.
-- RENAME COLUMN rewrites the triggers and indexes that read it.
ALTER TABLE agent_threads RENAME COLUMN owner_user_id TO uid;
ALTER TABLE agent_thread_messages RENAME COLUMN owner_user_id TO uid;
ALTER TABLE agent_thread_transcript_heads RENAME COLUMN owner_user_id TO uid;

-- `ALTER TABLE ADD COLUMN` refuses an expression default, so these arrive
-- nullable and are backfilled; every writer sets them from here on.
ALTER TABLE agent_thread_messages ADD COLUMN updated_at TEXT;
UPDATE agent_thread_messages SET updated_at = created_at;
ALTER TABLE agent_thread_transcript_heads ADD COLUMN created_at TEXT;
UPDATE agent_thread_transcript_heads SET created_at = updated_at;

-- PowerSync addresses a transcript head by `id`; locally the head *is* its
-- thread, so the column is derived rather than stored twice.
ALTER TABLE agent_thread_transcript_heads
    ADD COLUMN id TEXT GENERATED ALWAYS AS (thread_id) VIRTUAL;
CREATE UNIQUE INDEX idx_agent_thread_transcript_heads_id
    ON agent_thread_transcript_heads(id);

DROP TRIGGER agent_thread_create_empty_transcript;
CREATE TRIGGER agent_thread_create_empty_transcript
AFTER INSERT ON agent_threads FOR EACH ROW
BEGIN
    INSERT INTO agent_thread_transcript_heads (thread_id, uid, created_at, updated_at)
    VALUES (NEW.id, NEW.uid, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
            strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;

CREATE TRIGGER agent_thread_messages_updated_at
AFTER UPDATE ON agent_thread_messages FOR EACH ROW
BEGIN UPDATE agent_thread_messages SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- ---------------------------------------------------------------------------
-- `updated_at` triggers, without the version counter
-- ---------------------------------------------------------------------------

CREATE TRIGGER venues_updated_at AFTER UPDATE ON venues FOR EACH ROW
BEGIN UPDATE venues SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER tracks_updated_at AFTER UPDATE ON tracks FOR EACH ROW
BEGIN UPDATE tracks SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER fixtures_updated_at AFTER UPDATE ON fixtures FOR EACH ROW
BEGIN UPDATE fixtures SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER fixture_groups_updated_at AFTER UPDATE ON fixture_groups FOR EACH ROW
BEGIN UPDATE fixture_groups SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
BEGIN UPDATE fixture_group_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER fixture_group_overrides_updated_at AFTER UPDATE ON fixture_group_overrides FOR EACH ROW
BEGIN UPDATE fixture_group_overrides SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE group_id = OLD.group_id; END;
CREATE TRIGGER patterns_updated_at AFTER UPDATE ON patterns FOR EACH ROW
BEGIN UPDATE patterns SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER pattern_categories_updated_at AFTER UPDATE ON pattern_categories FOR EACH ROW
BEGIN UPDATE pattern_categories SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER implementations_updated_at AFTER UPDATE ON implementations FOR EACH ROW
BEGIN UPDATE implementations SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER venue_implementation_overrides_updated_at AFTER UPDATE ON venue_implementation_overrides FOR EACH ROW
BEGIN UPDATE venue_implementation_overrides SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE venue_id = OLD.venue_id AND pattern_id = OLD.pattern_id; END;
CREATE TRIGGER cues_updated_at AFTER UPDATE ON cues FOR EACH ROW
BEGIN UPDATE cues SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER midi_modifiers_updated_at AFTER UPDATE ON midi_modifiers FOR EACH ROW
BEGIN UPDATE midi_modifiers SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER midi_bindings_updated_at AFTER UPDATE ON midi_bindings FOR EACH ROW
BEGIN UPDATE midi_bindings SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER stage_pieces_updated_at AFTER UPDATE ON stage_pieces FOR EACH ROW
BEGIN UPDATE stage_pieces SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER track_waveforms_updated_at AFTER UPDATE ON track_waveforms FOR EACH ROW
BEGIN UPDATE track_waveforms SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;
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
CREATE TRIGGER venue_nodes_updated_at AFTER UPDATE ON venue_nodes FOR EACH ROW
BEGIN UPDATE venue_nodes SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER venue_edges_updated_at AFTER UPDATE ON venue_edges FOR EACH ROW
BEGIN UPDATE venue_edges SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE child_id = OLD.child_id; END;
CREATE TRIGGER venue_node_params_updated_at AFTER UPDATE ON venue_node_params FOR EACH ROW
BEGIN UPDATE venue_node_params SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE node_id = OLD.node_id AND key = OLD.key; END;
CREATE TRIGGER venue_constraints_updated_at AFTER UPDATE ON venue_constraints FOR EACH ROW
BEGIN UPDATE venue_constraints SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE node_id = OLD.node_id AND my_socket = OLD.my_socket; END;
CREATE TRIGGER scores_updated_at AFTER UPDATE ON scores FOR EACH ROW
BEGIN UPDATE scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER clips_updated_at AFTER UPDATE ON clips FOR EACH ROW
BEGIN UPDATE clips SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER score_definitions_updated_at AFTER UPDATE ON score_definitions FOR EACH ROW
BEGIN UPDATE score_definitions SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER drafts_updated_at AFTER UPDATE ON drafts FOR EACH ROW
BEGIN UPDATE drafts SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
CREATE TRIGGER venue_members_updated_at AFTER UPDATE ON venue_members FOR EACH ROW
BEGIN UPDATE venue_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

-- ---------------------------------------------------------------------------
-- A synced row is addressed by one `id`
-- ---------------------------------------------------------------------------

ALTER TABLE venue_edges ADD COLUMN id TEXT GENERATED ALWAYS AS (child_id) VIRTUAL;
CREATE UNIQUE INDEX idx_venue_edges_id ON venue_edges(id);
ALTER TABLE venue_node_params ADD COLUMN id TEXT GENERATED ALWAYS AS (node_id || ':' || key) VIRTUAL;
CREATE UNIQUE INDEX idx_venue_node_params_id ON venue_node_params(id);
ALTER TABLE venue_constraints ADD COLUMN id TEXT GENERATED ALWAYS AS (node_id || ':' || my_socket) VIRTUAL;
CREATE UNIQUE INDEX idx_venue_constraints_id ON venue_constraints(id);
ALTER TABLE track_beats ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL;
CREATE UNIQUE INDEX idx_track_beats_id ON track_beats(id);
ALTER TABLE track_roots ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL;
CREATE UNIQUE INDEX idx_track_roots_id ON track_roots(id);
ALTER TABLE track_drum_onsets ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL;
CREATE UNIQUE INDEX idx_track_drum_onsets_id ON track_drum_onsets(id);
ALTER TABLE track_bar_classifications ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL;
CREATE UNIQUE INDEX idx_track_bar_classifications_id ON track_bar_classifications(id);
ALTER TABLE track_genres ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL;
CREATE UNIQUE INDEX idx_track_genres_id ON track_genres(id);
ALTER TABLE track_beat_validations ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id) VIRTUAL;
CREATE UNIQUE INDEX idx_track_beat_validations_id ON track_beat_validations(id);
ALTER TABLE track_stems ADD COLUMN id TEXT GENERATED ALWAYS AS (track_id || ':' || stem_name) VIRTUAL;
CREATE UNIQUE INDEX idx_track_stems_id ON track_stems(id);

