-- A score repository is permanently named by its score, owner, track, and
-- venue identities. Generic sync upserts may refresh mutable catalog fields,
-- but no local, maintenance, or remote-write path may rebind an existing row
-- to a different authored repository. Replacement is an explicit
-- archive/create lifecycle operation.
CREATE TRIGGER authored_score_identity_immutable
BEFORE UPDATE ON scores FOR EACH ROW
WHEN NEW.id IS NOT OLD.id
  OR NEW.uid IS NOT OLD.uid
  OR NEW.track_id IS NOT OLD.track_id
  OR NEW.venue_id IS NOT OLD.venue_id
BEGIN
    SELECT RAISE(ABORT, 'score authored identity is immutable');
END;
