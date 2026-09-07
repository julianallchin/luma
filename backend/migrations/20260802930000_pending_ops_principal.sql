-- A pending operation is authority-bearing durable state. Bind it permanently
-- to the app-database principal admitted when it was enqueued, so a later
-- signed-out/signed-in or signed-in/signed-in identity switch can neither
-- flush nor mutate it.
-- `signed-out` and `signed-in:<uid>` are disjoint non-null identities; NULL cannot be
-- used because SQLite uniqueness treats NULL values as distinct.

CREATE TABLE pending_ops_principalized (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal_key TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    op_type TEXT NOT NULL CHECK(op_type IN ('upsert', 'delete')),
    table_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    payload_json TEXT,
    conflict_key TEXT NOT NULL DEFAULT 'id',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Existing upserts normally carry their owner in the immutable queued
-- payload. Old tombstones did not, so bind those to the admission that owns
-- this database at migration time; a closed or signed-out database keeps them
-- in the non-flushable signed-out queue.
INSERT INTO pending_ops_principalized (
    id, principal_key, op_type, table_name, record_id, payload_json,
    conflict_key, attempts, last_error, created_at, next_retry_at
)
SELECT
    op.id,
    CASE
        WHEN op.payload_json IS NOT NULL AND json_valid(op.payload_json) THEN
            CASE
                WHEN json_type(op.payload_json, '$.uid') = 'text'
                     AND length(json_extract(op.payload_json, '$.uid')) > 0
                    THEN 'signed-in:' || json_extract(op.payload_json, '$.uid')
                WHEN json_type(op.payload_json, '$.uid') = 'null'
                    THEN 'signed-out'
                ELSE COALESCE(
                    (
                        SELECT 'signed-in:' || admission.active_uid
                        FROM auth_write_admission AS admission
                        WHERE admission.singleton = 1
                          AND admission.armed = 1
                          AND admission.accepting = 1
                          AND admission.maintenance = 0
                          AND admission.remote_writes = 0
                          AND admission.active_uid IS NOT NULL
                    ),
                    'signed-out'
                )
            END
        ELSE COALESCE(
            (
                SELECT 'signed-in:' || admission.active_uid
                FROM auth_write_admission AS admission
                WHERE admission.singleton = 1
                  AND admission.armed = 1
                  AND admission.accepting = 1
                  AND admission.maintenance = 0
                  AND admission.remote_writes = 0
                  AND admission.active_uid IS NOT NULL
            ),
            'signed-out'
        )
    END,
    op.op_type,
    op.table_name,
    op.record_id,
    op.payload_json,
    op.conflict_key,
    op.attempts,
    op.last_error,
    op.created_at,
    op.next_retry_at
FROM pending_ops AS op;

-- SQLite validates trigger bodies while a referenced table is dropped, so
-- retire every live enqueue trigger before replacing the queue table. They are
-- recreated below against the principalized schema.
DROP TRIGGER IF EXISTS sync_delete_venues;
DROP TRIGGER IF EXISTS sync_delete_tracks;
DROP TRIGGER IF EXISTS sync_delete_pattern_categories;
DROP TRIGGER IF EXISTS sync_delete_fixtures;
DROP TRIGGER IF EXISTS sync_delete_patterns;
DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
DROP TRIGGER IF EXISTS sync_delete_scores;
DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
DROP TRIGGER IF EXISTS sync_delete_cues;
DROP TRIGGER IF EXISTS sync_delete_midi_bindings;

DROP TABLE pending_ops;
ALTER TABLE pending_ops_principalized RENAME TO pending_ops;

CREATE INDEX idx_pending_ops_next_retry
    ON pending_ops(principal_key, next_retry_at);
CREATE INDEX idx_pending_ops_table_record
    ON pending_ops(principal_key, table_name, record_id);
CREATE UNIQUE INDEX idx_pending_ops_dedup
    ON pending_ops(principal_key, table_name, record_id, op_type);

-- Every local tombstone captures the exact open admission in the same SQLite
-- statement as enqueue. Identity switches and maintenance close that admission
-- before deleting anything, so their cascades cannot leak into another queue.

DROP TRIGGER IF EXISTS sync_delete_venues;
CREATE TRIGGER sync_delete_venues AFTER DELETE ON venues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'venues', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_tracks;
CREATE TRIGGER sync_delete_tracks AFTER DELETE ON tracks FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'tracks', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_pattern_categories;
CREATE TRIGGER sync_delete_pattern_categories AFTER DELETE ON pattern_categories FOR EACH ROW
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'pattern_categories', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_fixtures;
CREATE TRIGGER sync_delete_fixtures AFTER DELETE ON fixtures FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'fixtures', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_patterns;
CREATE TRIGGER sync_delete_patterns AFTER DELETE ON patterns FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'patterns', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
CREATE TRIGGER sync_delete_fixture_groups AFTER DELETE ON fixture_groups FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'fixture_groups', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_scores;
CREATE TRIGGER sync_delete_scores AFTER DELETE ON scores FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'scores', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
CREATE TRIGGER sync_delete_fixture_group_members AFTER DELETE ON fixture_group_members FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'fixture_group_members', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
CREATE TRIGGER sync_delete_midi_modifiers AFTER DELETE ON midi_modifiers FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'midi_modifiers', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_cues;
CREATE TRIGGER sync_delete_cues AFTER DELETE ON cues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'cues', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_midi_bindings;
CREATE TRIGGER sync_delete_midi_bindings AFTER DELETE ON midi_bindings FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'midi_bindings', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;
