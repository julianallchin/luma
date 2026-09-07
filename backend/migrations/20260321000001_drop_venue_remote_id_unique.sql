-- Drop UNIQUE constraint on venues.remote_id to allow multiple local users
-- to reference the same cloud venue (owner + member rows).
-- SQLite doesn't support DROP CONSTRAINT, so we recreate the table.

CREATE TABLE venues_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT,
    uid TEXT,
    name TEXT NOT NULL,
    description TEXT,
    share_code TEXT,
    role TEXT NOT NULL DEFAULT 'owner',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

INSERT INTO venues_new SELECT id, remote_id, uid, name, description, share_code, role, created_at, updated_at, version, synced_at FROM venues;

DROP TABLE venues;
ALTER TABLE venues_new RENAME TO venues;

-- Recreate the share_code unique index
CREATE UNIQUE INDEX idx_venues_share_code ON venues(share_code) WHERE share_code IS NOT NULL;

-- Recreate the updated_at trigger
CREATE TRIGGER venues_updated_at
    AFTER UPDATE ON venues
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venues SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
