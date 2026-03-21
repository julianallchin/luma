-- Add share_code for venue sharing and role for distinguishing owned vs joined venues.
ALTER TABLE venues ADD COLUMN share_code TEXT;
ALTER TABLE venues ADD COLUMN role TEXT NOT NULL DEFAULT 'owner';

-- SQLite doesn't support UNIQUE in ALTER TABLE, so create a unique index separately.
CREATE UNIQUE INDEX idx_venues_share_code ON venues(share_code) WHERE share_code IS NOT NULL;
