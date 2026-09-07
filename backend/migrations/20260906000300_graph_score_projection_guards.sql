-- This column is a local projection of score.luma, delivered by authored
-- history. Updating it must not dirty the independently synced score metadata.
DROP TRIGGER scores_updated_at;
CREATE TRIGGER scores_updated_at
AFTER UPDATE OF id, uid, track_id, venue_id, name, created_at ON scores FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
    UPDATE scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
        version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;

-- Once migrated, a score cannot accumulate a second set of authored clips.
-- A restore of legacy history clears the graph projection first, atomically.
CREATE TRIGGER graph_score_rejects_legacy_clip_insert
BEFORE INSERT ON track_scores FOR EACH ROW
WHEN EXISTS(SELECT 1 FROM scores WHERE id = NEW.score_id AND graph_document_json IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'edit graph score clips through its authored document');
END;
CREATE TRIGGER graph_score_rejects_legacy_clip_update
BEFORE UPDATE ON track_scores FOR EACH ROW
WHEN EXISTS(SELECT 1 FROM scores WHERE id IN (OLD.score_id, NEW.score_id) AND graph_document_json IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'edit graph score clips through its authored document');
END;
