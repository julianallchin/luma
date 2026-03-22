-- Drop UNIQUE constraint on fixtures.remote_id and fixture_groups.remote_id
-- to allow the same cloud fixtures/groups to exist under multiple local venues
-- (e.g., owner and member of the same venue on one machine).
-- SQLite doesn't support DROP CONSTRAINT, so we recreate the tables.

-- ============================================================================
-- fixtures
-- ============================================================================

CREATE TABLE fixtures_new (
    id TEXT PRIMARY KEY,
    remote_id TEXT,
    uid TEXT,
    venue_id INTEGER NOT NULL,
    universe INTEGER NOT NULL DEFAULT 1,
    address INTEGER NOT NULL,
    num_channels INTEGER NOT NULL,
    manufacturer TEXT NOT NULL,
    model TEXT NOT NULL,
    mode_name TEXT NOT NULL,
    fixture_path TEXT NOT NULL,
    label TEXT,
    pos_x REAL DEFAULT 0.0,
    pos_y REAL DEFAULT 0.0,
    pos_z REAL DEFAULT 0.0,
    rot_x REAL DEFAULT 0.0,
    rot_y REAL DEFAULT 0.0,
    rot_z REAL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

INSERT INTO fixtures_new SELECT * FROM fixtures;
DROP TABLE fixtures;
ALTER TABLE fixtures_new RENAME TO fixtures;

CREATE INDEX idx_fixtures_venue ON fixtures(venue_id);
CREATE INDEX idx_fixtures_universe ON fixtures(universe);
CREATE INDEX idx_fixtures_remote_id ON fixtures(remote_id);

CREATE TRIGGER fixtures_updated_at
    AFTER UPDATE ON fixtures
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE fixtures SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

-- ============================================================================
-- fixture_groups
-- ============================================================================

CREATE TABLE fixture_groups_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT,
    uid TEXT,
    venue_id INTEGER NOT NULL,
    name TEXT,
    axis_lr REAL,
    axis_fb REAL,
    axis_ab REAL,
    display_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    tags TEXT NOT NULL DEFAULT '[]',
    movement_config TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

INSERT INTO fixture_groups_new SELECT * FROM fixture_groups;
DROP TABLE fixture_groups;
ALTER TABLE fixture_groups_new RENAME TO fixture_groups;

CREATE INDEX idx_fixture_groups_venue ON fixture_groups(venue_id);
CREATE INDEX idx_fixture_groups_remote_id ON fixture_groups(remote_id);
CREATE UNIQUE INDEX idx_fixture_groups_venue_name ON fixture_groups(venue_id, name);

CREATE TRIGGER fixture_groups_updated_at
    AFTER UPDATE ON fixture_groups
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE fixture_groups SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
