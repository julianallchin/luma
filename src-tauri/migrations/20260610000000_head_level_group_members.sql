-- Head-level group membership.
-- A member row is either a whole fixture (head_index = -1) or a single head
-- of it (head_index >= 0). The -1 sentinel (rather than NULL) keeps the
-- UNIQUE constraint and the remote ON CONFLICT semantics simple.

CREATE TABLE fixture_group_members_new (
    id TEXT NOT NULL PRIMARY KEY,
    fixture_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    head_index INTEGER NOT NULL DEFAULT -1,
    display_order INTEGER NOT NULL DEFAULT 0,
    uid TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    origin TEXT NOT NULL DEFAULT 'local',
    UNIQUE (fixture_id, group_id, head_index),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE
);

INSERT INTO fixture_group_members_new
    (id, fixture_id, group_id, head_index, display_order, uid, created_at, updated_at, version, synced_at, origin)
SELECT id, fixture_id, group_id, -1, display_order, uid, created_at, updated_at, version, synced_at, origin
FROM fixture_group_members;

DROP TABLE fixture_group_members;
ALTER TABLE fixture_group_members_new RENAME TO fixture_group_members;

CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);

CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixture_group_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER sync_delete_fixture_group_members AFTER DELETE ON fixture_group_members FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, next_retry_at)
    VALUES ('delete', 'fixture_group_members', OLD.id, CURRENT_TIMESTAMP);
END;
