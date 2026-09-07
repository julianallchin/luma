-- Replace `preprocessing_runs` with `processor_version` on the artifact tables.
--
-- The artifact rows themselves are now the source of truth for "is preprocessor
-- X done at version V for track T?" — no separate state table to drift from
-- the data, no special-case for sync-pulled artifacts.
--
-- Failures keep their own table so reconcile can apply backoff without
-- re-attempting hopelessly broken inputs every cycle.
--
-- Also drops `pending_ops.tier`: tier is now derived at runtime from the
-- sync registry's parents-based topological order (one source of truth).
-- All `sync_delete_*` triggers are recreated without the literal tier value.

-- ── 1. processor_version on artifact tables ──────────────────────────────────
-- DEFAULT 1 backfills existing rows; matches the current Preprocessor::version().
ALTER TABLE track_beats ADD COLUMN processor_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE track_stems ADD COLUMN processor_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE track_roots ADD COLUMN processor_version INTEGER NOT NULL DEFAULT 1;

-- ── 2. Drop the old runs table ───────────────────────────────────────────────
DROP TABLE IF EXISTS preprocessing_runs;

-- ── 3. Local-only failure log with retry backoff ─────────────────────────────
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

-- ── 4. Drop pending_ops.tier and recreate every delete-sync trigger ──────────
-- Triggers cannot reference a column that's been dropped, so they all need
-- to be recreated in the same migration. The new triggers omit `tier`;
-- push order is computed in Rust from the sync registry topology.

DROP TRIGGER IF EXISTS sync_delete_venues;
DROP TRIGGER IF EXISTS sync_delete_tracks;
DROP TRIGGER IF EXISTS sync_delete_pattern_categories;
DROP TRIGGER IF EXISTS sync_delete_fixtures;
DROP TRIGGER IF EXISTS sync_delete_patterns;
DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
DROP TRIGGER IF EXISTS sync_delete_implementations;
DROP TRIGGER IF EXISTS sync_delete_scores;
DROP TRIGGER IF EXISTS sync_delete_track_beats;
DROP TRIGGER IF EXISTS sync_delete_track_roots;
DROP TRIGGER IF EXISTS sync_delete_track_stems;
DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
DROP TRIGGER IF EXISTS sync_delete_track_scores;
DROP TRIGGER IF EXISTS sync_delete_venue_impl_overrides;
DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
DROP TRIGGER IF EXISTS sync_delete_cues;
DROP TRIGGER IF EXISTS sync_delete_midi_bindings;

ALTER TABLE pending_ops DROP COLUMN tier;

-- Recreated triggers, identical except no tier column.
CREATE TRIGGER sync_delete_venues AFTER DELETE ON venues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'venues', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_tracks AFTER DELETE ON tracks FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'tracks', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_pattern_categories AFTER DELETE ON pattern_categories FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'pattern_categories', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_fixtures AFTER DELETE ON fixtures FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'fixtures', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_patterns AFTER DELETE ON patterns FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'patterns', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_fixture_groups AFTER DELETE ON fixture_groups FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'fixture_groups', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_implementations AFTER DELETE ON implementations FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'implementations', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_scores AFTER DELETE ON scores FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'scores', OLD.id, CURRENT_TIMESTAMP);
END;

-- track_beats / track_roots / track_stems have no delete-sync trigger:
-- the parent track's soft-delete cascades through Supabase's new
-- cascade_soft_delete_track_children trigger.

CREATE TRIGGER sync_delete_fixture_group_members AFTER DELETE ON fixture_group_members FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'fixture_group_members', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_scores AFTER DELETE ON track_scores FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'track_scores', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_venue_impl_overrides AFTER DELETE ON venue_implementation_overrides FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'venue_implementation_overrides', OLD.venue_id || ':' || OLD.pattern_id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_midi_modifiers AFTER DELETE ON midi_modifiers FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'midi_modifiers', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_cues AFTER DELETE ON cues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'cues', OLD.id, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_midi_bindings AFTER DELETE ON midi_bindings FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'midi_bindings', OLD.id, CURRENT_TIMESTAMP);
END;
