-- Allow multiple scores per (track, venue) pair — one per user.
-- The old unique index enforced exactly one score per track/venue.
DROP INDEX IF EXISTS idx_scores_track_venue;
CREATE INDEX idx_scores_track_venue ON scores(track_id, venue_id);
