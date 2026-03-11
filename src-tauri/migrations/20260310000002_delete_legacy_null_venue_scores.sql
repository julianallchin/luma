-- Remove any legacy scores that have no venue_id.
-- All scores must now be venue-scoped; the app no longer supports NULL venue_id.
-- Cascade will also delete related track_scores rows.
DELETE FROM scores WHERE venue_id IS NULL;
