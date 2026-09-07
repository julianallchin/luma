-- Add sync columns to fixture_group_members so it syncs like every other table.

-- SQLite doesn't support ADD COLUMN with defaults on existing tables well,
-- so recreate the table.
CREATE TABLE fixture_group_members_new (
    fixture_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    display_order INTEGER NOT NULL DEFAULT 0,
    uid TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    PRIMARY KEY (fixture_id, group_id),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE
);

INSERT INTO fixture_group_members_new (fixture_id, group_id, display_order, uid, created_at)
SELECT fgm.fixture_id, fgm.group_id, fgm.display_order,
       fg.uid,
       fgm.created_at
FROM fixture_group_members fgm
JOIN fixture_groups fg ON fg.id = fgm.group_id;

DROP TABLE fixture_group_members;
ALTER TABLE fixture_group_members_new RENAME TO fixture_group_members;

CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);

CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixture_group_members SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1
    WHERE fixture_id = OLD.fixture_id AND group_id = OLD.group_id; END;
