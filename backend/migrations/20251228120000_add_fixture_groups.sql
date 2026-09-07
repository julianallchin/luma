-- Fixture groups for organizing fixtures within a venue
-- Groups have axis positions for spatial selection queries
CREATE TABLE fixture_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    uid TEXT,
    venue_id INTEGER NOT NULL,
    name TEXT,

    -- Axis positions: -1.0 (negative) to +1.0 (positive), NULL = not positioned on axis
    axis_lr REAL,      -- Left (-1) to Right (+1)
    axis_fb REAL,      -- Front (-1) to Back (+1)
    axis_ab REAL,      -- Below (-1) to Above (+1)

    display_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

CREATE INDEX idx_fixture_groups_venue ON fixture_groups(venue_id);

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

-- Junction table: fixtures belong to groups (many-to-many)
CREATE TABLE fixture_group_members (
    fixture_id TEXT NOT NULL,
    group_id INTEGER NOT NULL,
    display_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (fixture_id, group_id),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES fixture_groups(id) ON DELETE CASCADE
);

CREATE INDEX idx_fixture_group_members_group ON fixture_group_members(group_id);
CREATE INDEX idx_fixture_group_members_fixture ON fixture_group_members(fixture_id);

-- Create default groups for existing venues and assign all fixtures to them
INSERT INTO fixture_groups (venue_id, name, axis_lr, axis_fb, axis_ab, display_order)
SELECT DISTINCT venue_id, 'Default', 0.0, 0.0, 0.0, 0
FROM fixtures;

-- Add existing fixtures to their venue's default group
INSERT INTO fixture_group_members (fixture_id, group_id, display_order)
SELECT f.id, fg.id, 0
FROM fixtures f
JOIN fixture_groups fg ON fg.venue_id = f.venue_id AND fg.name = 'Default';
