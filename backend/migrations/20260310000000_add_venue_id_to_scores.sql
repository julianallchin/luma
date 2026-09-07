-- Add venue_id to scores so annotations are venue-scoped.
-- Existing scores get venue_id = NULL (legacy unscoped).
ALTER TABLE scores ADD COLUMN venue_id INTEGER REFERENCES venues(id) ON DELETE CASCADE;

-- Index for venue-scoped lookups
CREATE INDEX idx_scores_venue ON scores(venue_id);

-- Unique constraint on (track_id, venue_id) pairs.
-- SQLite allows multiple NULLs in unique indexes, so legacy scores won't conflict.
CREATE UNIQUE INDEX idx_scores_track_venue ON scores(track_id, venue_id);
