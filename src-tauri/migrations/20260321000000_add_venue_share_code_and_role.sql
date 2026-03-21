-- Add share_code for venue sharing and role for distinguishing owned vs joined venues.
ALTER TABLE venues ADD COLUMN share_code TEXT UNIQUE;
ALTER TABLE venues ADD COLUMN role TEXT NOT NULL DEFAULT 'owner';
