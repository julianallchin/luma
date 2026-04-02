-- Restore the sync_delete_track_scores trigger that was dropped in
-- 20260329000000_drop_cascade_child_delete_triggers.sql.
--
-- That migration assumed track_scores are only deleted via CASCADE from their
-- parent track, but replace_track_scores() directly deletes individual rows
-- during save/undo/redo. Without this trigger, those deletes are never queued
-- in pending_ops and Supabase never receives a soft-delete.

CREATE TRIGGER IF NOT EXISTS sync_delete_track_scores AFTER DELETE ON track_scores FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_scores', OLD.id, 3, CURRENT_TIMESTAMP);
END;
