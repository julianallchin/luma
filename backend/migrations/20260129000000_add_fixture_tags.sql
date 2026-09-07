-- Tags table: flat list of tags that can be assigned to fixtures
CREATE TABLE fixture_tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    uid TEXT,
    venue_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'purpose',
    is_auto_generated INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE,
    UNIQUE (venue_id, name)
);

CREATE INDEX idx_fixture_tags_venue ON fixture_tags(venue_id);
CREATE INDEX idx_fixture_tags_category ON fixture_tags(category);

CREATE TRIGGER fixture_tags_updated_at
    AFTER UPDATE ON fixture_tags
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE fixture_tags SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

-- Junction table: fixtures can have multiple tags (many-to-many)
CREATE TABLE fixture_tag_assignments (
    fixture_id TEXT NOT NULL,
    tag_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (fixture_id, tag_id),
    FOREIGN KEY (fixture_id) REFERENCES fixtures(id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES fixture_tags(id) ON DELETE CASCADE
);

CREATE INDEX idx_fixture_tag_assignments_tag ON fixture_tag_assignments(tag_id);
CREATE INDEX idx_fixture_tag_assignments_fixture ON fixture_tag_assignments(fixture_id);
