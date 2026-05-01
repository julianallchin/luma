-- Add `origin` column to cues / midi_modifiers / midi_bindings.
--
-- These tables were recreated in 20260403100000_midi_sync_columns.sql to add
-- (uid, version, synced_at) for sync, but `origin` was omitted, so the pull
-- engine fails with `table X has no column named origin` when upserting.
--
-- Also rebuild their delete-sync triggers with `WHEN OLD.origin = 'local'`
-- so deletes of pulled (remote-origin) rows don't push spurious tombstones.

ALTER TABLE cues           ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE midi_modifiers ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE midi_bindings  ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';

DROP TRIGGER IF EXISTS sync_delete_cues;
DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
DROP TRIGGER IF EXISTS sync_delete_midi_bindings;

CREATE TRIGGER sync_delete_cues AFTER DELETE ON cues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'cues', OLD.id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_midi_modifiers AFTER DELETE ON midi_modifiers FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'midi_modifiers', OLD.id, 1, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_midi_bindings AFTER DELETE ON midi_bindings FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'midi_bindings', OLD.id, 2, CURRENT_TIMESTAMP);
END;

-- Drop any stale delete ops queued from cascade-delete of pulled rows.
DELETE FROM pending_ops
WHERE op_type = 'delete'
  AND table_name IN ('cues', 'midi_modifiers', 'midi_bindings');
