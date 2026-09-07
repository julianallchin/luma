-- Add provenance tracking to all synced tables.
-- `origin` distinguishes locally-created data ('local') from data pulled
-- from the remote ('remote'). Delete-sync triggers only fire for local data,
-- preventing cascade cleanup of pulled data from pushing unwanted remote
-- deletions.

-- ============================================================================
-- 1. Add `origin` column to every synced table
-- ============================================================================

ALTER TABLE venues ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE tracks ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE fixtures ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE patterns ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE fixture_groups ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE implementations ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE scores ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE track_beats ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE track_roots ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE track_stems ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';
ALTER TABLE track_scores ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';

-- ============================================================================
-- 2. Recreate fixture_group_members with a UUID `id` PK and `origin` column
-- ============================================================================

CREATE TABLE fixture_group_members_new (
    id TEXT NOT NULL PRIMARY KEY,
    fixture_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    display_order INTEGER NOT NULL DEFAULT 0,
    uid TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    origin TEXT NOT NULL DEFAULT 'local',
    UNIQUE (fixture_id, group_id),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE
);

INSERT INTO fixture_group_members_new (id, fixture_id, group_id, display_order, uid, created_at, updated_at, version, synced_at)
SELECT lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)),2) || '-a' || substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6))),
       fixture_id, group_id, display_order, uid, created_at, updated_at, version, synced_at
FROM fixture_group_members;

DROP TABLE fixture_group_members;
ALTER TABLE fixture_group_members_new RENAME TO fixture_group_members;

CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);

CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixture_group_members SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1
    WHERE id = OLD.id; END;

-- ============================================================================
-- 3. Rebuild delete-sync triggers with origin = 'local' guard
-- ============================================================================

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

-- Tier 0
CREATE TRIGGER sync_delete_venues AFTER DELETE ON venues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'venues', OLD.id, 0, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_tracks AFTER DELETE ON tracks FOR EACH ROW
WHEN OLD.origin = 'local'
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
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'fixtures', OLD.id, 1, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_patterns AFTER DELETE ON patterns FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'patterns', OLD.id, 1, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_fixture_groups AFTER DELETE ON fixture_groups FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'fixture_groups', OLD.id, 1, CURRENT_TIMESTAMP);
END;

-- Tier 2
CREATE TRIGGER sync_delete_implementations AFTER DELETE ON implementations FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'implementations', OLD.id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_scores AFTER DELETE ON scores FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'scores', OLD.id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_beats AFTER DELETE ON track_beats FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_beats', OLD.track_id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_roots AFTER DELETE ON track_roots FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_roots', OLD.track_id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_track_stems AFTER DELETE ON track_stems FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_stems', OLD.track_id || ':' || OLD.stem_name, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_fixture_group_members AFTER DELETE ON fixture_group_members FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'fixture_group_members', OLD.id, 2, CURRENT_TIMESTAMP);
END;

-- Tier 3
CREATE TRIGGER sync_delete_track_scores AFTER DELETE ON track_scores FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'track_scores', OLD.id, 3, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_venue_impl_overrides AFTER DELETE ON venue_implementation_overrides FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'venue_implementation_overrides', OLD.venue_id || ':' || OLD.pattern_id, 3, CURRENT_TIMESTAMP);
END;

-- ============================================================================
-- 4. Clean up stale pending delete ops from prior cascade issues
-- ============================================================================

DELETE FROM pending_ops WHERE op_type = 'delete';
