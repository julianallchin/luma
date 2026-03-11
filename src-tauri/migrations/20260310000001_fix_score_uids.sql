-- Fix any scores that were created with random UUIDs instead of inheriting the track UID.
-- These would have been created by the buggy ensure_score_id before the fix.
UPDATE scores
SET uid = (SELECT t.uid FROM tracks t WHERE t.id = scores.track_id)
WHERE uid != (SELECT t.uid FROM tracks t WHERE t.id = scores.track_id);
