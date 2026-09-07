-- Local venue_memberships table: tracks which venues the current user has joined.
-- The venue row itself always stores the OWNER's uid; memberships record the joiner.
CREATE TABLE venue_memberships (
    venue_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (venue_id, user_id),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

-- Backfill from existing member venue rows:
-- These rows have uid = joiner's uid and role = 'member'.
INSERT OR IGNORE INTO venue_memberships (venue_id, user_id, role)
SELECT id, uid, 'member' FROM venues WHERE role = 'member' AND uid IS NOT NULL;

-- Clear joiner uid on member venues.
-- On next pull, the owner's uid will be fetched from the cloud and set here.
UPDATE venues SET uid = NULL WHERE role = 'member';
