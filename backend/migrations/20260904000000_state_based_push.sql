-- State-based push: the local tables become the only record of what push owes
-- the server. See docs/design/sync-push-v2.md.
--
-- DDL only. Venue admission triggers ABORT any UPDATE of venues / venue_nodes /
-- venue_edges / venue_node_params / venue_constraints, so a migration may not
-- write rows in those tables (20260902000000 had to rebuild them for the same
-- reason). Nothing here needs a backfill: a row mid-flight already satisfies the
-- new dirty predicate (`synced_at IS NULL`), the new `synced_at` columns default
-- NULL, and `pending_ops` is translated at runtime by
-- `sync::transition::drain_legacy_push_queue`.

-- -----------------------------------------------------------------------------
-- The deletion fact
-- -----------------------------------------------------------------------------

-- The one piece of truth a table cannot hold, because the row is gone. No
-- payload, no op_type, no retry state: at most one statement per identity, so
-- the "queued upsert plus queued delete for the same row" pair that wedged the
-- old queue is unrepresentable. A tombstone is deleted once the remote has
-- accepted it; the remote keeps `deleted_at` forever because *other* devices
-- need it, and this device already knows by the row's absence.
CREATE TABLE sync_tombstones (
    principal_key TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    table_name TEXT NOT NULL,
    -- `registry::record_id`: primary-key values joined with U+001F.
    record_id TEXT NOT NULL,
    deleted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (principal_key, table_name, record_id)
);

-- -----------------------------------------------------------------------------
-- Retry state — the only state in this design that is not derivable
-- -----------------------------------------------------------------------------

CREATE TABLE sync_push_failures (
    principal_key TEXT NOT NULL,
    table_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    -- Which of the two things that can be owed for one identity failed: the
    -- row ('row') or its deletion ('tombstone'). Without it they share a
    -- budget, and a deletion the server refuses twenty times would hand its
    -- `permanent` verdict to whatever row next occupies that primary key —
    -- with a NULL `seen_version` that the content-changed escape can never
    -- clear, so the identity would be dead forever.
    subject TEXT NOT NULL CHECK (subject IN ('row', 'tombstone')),
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- The row `version` observed when the failure was recorded. A later version
    -- is different content and restarts the budget. NULL means the subject has
    -- no version — immutable rows and tombstones — and therefore never resets.
    seen_version INTEGER,
    -- Delivery can never succeed as written: an identity the remote column type
    -- cannot hold, an immutable row that collided with different bytes, or an
    -- exhausted attempt budget. Skipped and quiet until the content changes.
    permanent INTEGER NOT NULL DEFAULT 0 CHECK (permanent IN (0, 1)),
    PRIMARY KEY (principal_key, table_name, record_id, subject)
);

CREATE INDEX idx_sync_push_failures_ready
    ON sync_push_failures(principal_key, next_retry_at);

-- -----------------------------------------------------------------------------
-- Delivery markers for the tables that had none
-- -----------------------------------------------------------------------------

-- Immutable traces and the mutable thread projection kept their delivery state
-- in `pending_ops` alone. It belongs on the row: `synced_at IS NULL` is "not yet
-- delivered", and pull stamps it so a row that arrived from the server is not
-- pushed straight back. These columns are local-only; they are absent from
-- every registry `columns` list and never reach PostgREST.
ALTER TABLE agent_threads ADD COLUMN synced_at TEXT;
ALTER TABLE agent_thread_messages ADD COLUMN synced_at TEXT;
ALTER TABLE agent_thread_message_appends ADD COLUMN synced_at TEXT;
ALTER TABLE agent_thread_deletions ADD COLUMN synced_at TEXT;
ALTER TABLE authored_documents ADD COLUMN synced_at TEXT;
ALTER TABLE authored_revisions ADD COLUMN synced_at TEXT;
ALTER TABLE authored_revision_files ADD COLUMN synced_at TEXT;
ALTER TABLE authored_revision_parents ADD COLUMN synced_at TEXT;
ALTER TABLE authored_turn_preparations ADD COLUMN synced_at TEXT;
ALTER TABLE authored_turn_outcomes ADD COLUMN synced_at TEXT;
ALTER TABLE authored_operation_outcomes ADD COLUMN synced_at TEXT;


-- Every row that exists *now* was delivered under the old engine, whose receipt
-- was the removal of its queue entry — a row still owed to the server has an
-- entry, and `drain_legacy_push_queue` clears the marker again for exactly
-- those. Stamping here rather than at the first push is what makes the boundary
-- the migration itself: a row created after this point, but before the first
-- flush, is genuinely new and must not be presumed delivered.
--
-- These are not venue tables, so the venue-admission landmine does not apply.
-- Their own triggers do: the blanket immutability refusals and the thread
-- projection's `updated_at` rewrite are dropped for the duration and recreated
-- below, which is where they gain their delivery exemption anyway.

DROP TRIGGER authored_revision_is_immutable;
DROP TRIGGER authored_revision_file_is_immutable;
DROP TRIGGER authored_revision_parent_is_immutable;
DROP TRIGGER authored_turn_preparation_is_immutable;
DROP TRIGGER authored_turn_outcome_is_immutable;
DROP TRIGGER authored_operation_outcome_is_immutable;
DROP TRIGGER agent_thread_message_cannot_be_updated;
DROP TRIGGER agent_thread_append_receipt_is_immutable;
DROP TRIGGER agent_thread_deletion_receipt_is_immutable;
DROP TRIGGER agent_threads_updated_at;

UPDATE agent_threads SET synced_at = updated_at;
UPDATE agent_thread_messages SET synced_at = CURRENT_TIMESTAMP;
UPDATE agent_thread_message_appends SET synced_at = CURRENT_TIMESTAMP;
UPDATE agent_thread_deletions SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_documents SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_revisions SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_revision_files SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_revision_parents SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_turn_preparations SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_turn_outcomes SET synced_at = CURRENT_TIMESTAMP;
UPDATE authored_operation_outcomes SET synced_at = CURRENT_TIMESTAMP;

CREATE TRIGGER agent_threads_updated_at
AFTER UPDATE ON agent_threads FOR EACH ROW
WHEN COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    UPDATE agent_threads
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
    WHERE id = OLD.id;
END;

-- -----------------------------------------------------------------------------
-- Immutable rows may still record their own delivery
-- -----------------------------------------------------------------------------

-- These tables refuse every UPDATE, which is why their delivery state used to
-- live in the queue. The refusal now has one hole, as narrow as it can be made:
-- an update that changes `synced_at` *and* runs inside a sync-owned write
-- (`enter_remote_writes`) is a delivery receipt, not an edit to history. Both
-- halves are required — a product-column update inside a pull is still refused,
-- and a receipt written by ordinary application code is still refused.
--
-- `authored_documents` needs no change: its immutability trigger already names
-- the identity columns rather than blanketing the table.

CREATE TRIGGER authored_revision_is_immutable
BEFORE UPDATE ON authored_revisions FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'authored revision is immutable');
END;

CREATE TRIGGER authored_revision_file_is_immutable
BEFORE UPDATE ON authored_revision_files FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'authored revision file is immutable');
END;

CREATE TRIGGER authored_revision_parent_is_immutable
BEFORE UPDATE ON authored_revision_parents FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'authored revision parent is immutable');
END;

CREATE TRIGGER authored_turn_preparation_is_immutable
BEFORE UPDATE ON authored_turn_preparations FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'authored turn preparation is immutable');
END;

CREATE TRIGGER authored_turn_outcome_is_immutable
BEFORE UPDATE ON authored_turn_outcomes FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'authored turn outcome is immutable');
END;

CREATE TRIGGER authored_operation_outcome_is_immutable
BEFORE UPDATE ON authored_operation_outcomes FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'authored operation outcome is immutable');
END;

CREATE TRIGGER agent_thread_message_cannot_be_updated
BEFORE UPDATE ON agent_thread_messages FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'agent thread transcript is append-only');
END;

CREATE TRIGGER agent_thread_append_receipt_is_immutable
BEFORE UPDATE ON agent_thread_message_appends FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt is immutable');
END;

CREATE TRIGGER agent_thread_deletion_receipt_is_immutable
BEFORE UPDATE ON agent_thread_deletions FOR EACH ROW
WHEN NEW.synced_at IS OLD.synced_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
BEGIN
    SELECT RAISE(ABORT, 'agent thread deletion receipt is immutable');
END;

-- -----------------------------------------------------------------------------
-- Delete triggers stop producing sync state and start enforcing an invariant
-- -----------------------------------------------------------------------------

DROP TRIGGER IF EXISTS sync_delete_venues;
DROP TRIGGER IF EXISTS sync_delete_tracks;
DROP TRIGGER IF EXISTS sync_delete_fixtures;
DROP TRIGGER IF EXISTS sync_delete_patterns;
DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
DROP TRIGGER IF EXISTS sync_delete_scores;
DROP TRIGGER IF EXISTS sync_delete_cues;
DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
DROP TRIGGER IF EXISTS sync_delete_midi_bindings;
DROP TRIGGER IF EXISTS sync_delete_venue_nodes;
DROP TRIGGER IF EXISTS sync_delete_venue_edges;
DROP TRIGGER IF EXISTS sync_delete_venue_node_params;
DROP TRIGGER IF EXISTS sync_delete_venue_constraints;
-- `pattern_categories` is not in `sync::registry::TABLES`, so its tombstones
-- could only ever be rejected as "not registered for relational sync". It has no
-- `origin` column either, so the trigger had no provenance guard. Audit T3.10.
DROP TRIGGER IF EXISTS sync_delete_pattern_categories;

-- The guards below write nothing. They exist so that a hard delete of a synced
-- row that did not go through `database::local::sync_delete` cannot commit:
-- under state-based push such a delete is silent data divergence rather than a
-- loud failure, and there is no queue left to notice it afterwards.
--
-- The `WHEN` predicate is copied verbatim from the triggers above, so the
-- tombstone surface is unchanged: a delete under `enter_maintenance` (the
-- sign-out projection wipe, `archive_score`) or under `enter_remote_writes`
-- (`pull::delete_local`) is still not a local deletion and is still not
-- recorded.

CREATE TRIGGER guard_unrecorded_delete_venues AFTER DELETE ON venues FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'venues'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced venues row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_tracks AFTER DELETE ON tracks FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'tracks'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced tracks row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_fixtures AFTER DELETE ON fixtures FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'fixtures'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced fixtures row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_patterns AFTER DELETE ON patterns FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'patterns'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced patterns row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_fixture_groups AFTER DELETE ON fixture_groups FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'fixture_groups'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced fixture_groups row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_fixture_group_members
AFTER DELETE ON fixture_group_members FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'fixture_group_members'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced fixture_group_members row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_scores AFTER DELETE ON scores FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'scores'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced scores row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_cues AFTER DELETE ON cues FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'cues'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced cues row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_midi_modifiers AFTER DELETE ON midi_modifiers FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'midi_modifiers'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced midi_modifiers row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_midi_bindings AFTER DELETE ON midi_bindings FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'midi_bindings'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced midi_bindings row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_venue_nodes AFTER DELETE ON venue_nodes FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'venue_nodes'
        AND tombstone.record_id = OLD.id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced venue_nodes row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_venue_edges AFTER DELETE ON venue_edges FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'venue_edges'
        AND tombstone.record_id = OLD.child_id
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced venue_edges row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_venue_node_params
AFTER DELETE ON venue_node_params FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'venue_node_params'
        AND tombstone.record_id = OLD.node_id || char(31) || OLD.key
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced venue_node_params row was not recorded as a tombstone');
END;

CREATE TRIGGER guard_unrecorded_delete_venue_constraints
AFTER DELETE ON venue_constraints FOR EACH ROW
WHEN OLD.origin = 'local'
  AND EXISTS (SELECT 1 FROM auth_write_admission AS admission
              WHERE admission.singleton = 1 AND admission.armed = 1
                AND admission.accepting = 1 AND admission.maintenance = 0
                AND admission.remote_writes = 0)
  AND NOT EXISTS (
      SELECT 1 FROM sync_tombstones AS tombstone, auth_write_admission AS admission
      WHERE admission.singleton = 1
        AND tombstone.table_name = 'venue_constraints'
        AND tombstone.record_id = OLD.node_id || char(31) || OLD.my_socket
        AND tombstone.principal_key = CASE WHEN admission.active_uid IS NULL
                                           THEN 'signed-out'
                                           ELSE 'signed-in:' || admission.active_uid END)
BEGIN
    SELECT RAISE(ABORT, 'delete of a synced venue_constraints row was not recorded as a tombstone');
END;

-- -----------------------------------------------------------------------------
-- The queue, kept only long enough to be translated
-- -----------------------------------------------------------------------------

-- Renamed rather than dropped: a live database can hold unpushed edits here, and
-- translating them needs write admission the migration does not have (a venue
-- row cannot be marked dirty from a migration). `drain_legacy_push_queue` turns
-- each row into a dirty row or a tombstone at the first push under the new
-- engine and then drops this table; its absence is the completion flag.
ALTER TABLE pending_ops RENAME TO pending_ops_drain;
