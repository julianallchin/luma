-- Auto-enqueue delete ops into pending_ops when rows are deleted.
-- This makes deletion sync automatic for all syncable tables —
-- no manual enqueue_delete calls needed in command handlers.

-- Tier 0
CREATE TRIGGER sync_delete_venues AFTER DELETE ON venues FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'venues', OLD.id, 0, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_tracks AFTER DELETE ON tracks FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'tracks', OLD.id, 0, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_pattern_categories AFTER DELETE ON pattern_categories FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'pattern_categories', OLD.id, 0, CURRENT_TIMESTAMP);
END;

-- Tier 1
CREATE TRIGGER sync_delete_fixtures AFTER DELETE ON fixtures FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'fixtures', OLD.id, 1, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_patterns AFTER DELETE ON patterns FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'patterns', OLD.id, 1, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_fixture_groups AFTER DELETE ON fixture_groups FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'fixture_groups', OLD.id, 1, CURRENT_TIMESTAMP);
END;

-- Tier 2
CREATE TRIGGER sync_delete_implementations AFTER DELETE ON implementations FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'implementations', OLD.id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_scores AFTER DELETE ON scores FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'scores', OLD.id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_beats AFTER DELETE ON track_beats FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_beats', OLD.track_id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_roots AFTER DELETE ON track_roots FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_roots', OLD.track_id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_stems AFTER DELETE ON track_stems FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_stems', OLD.track_id || ':' || OLD.stem_name, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_fixture_group_members AFTER DELETE ON fixture_group_members FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'fixture_group_members', OLD.fixture_id || ':' || OLD.group_id, 2, CURRENT_TIMESTAMP);
END;

-- Tier 3
CREATE TRIGGER sync_delete_track_scores AFTER DELETE ON track_scores FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_scores', OLD.id, 3, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_venue_impl_overrides AFTER DELETE ON venue_implementation_overrides FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'venue_implementation_overrides', OLD.venue_id || ':' || OLD.pattern_id, 3, CURRENT_TIMESTAMP);
END;
