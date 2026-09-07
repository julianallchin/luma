-- Retained relational closure is durable state, not ambient read authority.
-- These views are the single database-level roots for track and pattern
-- discovery by the currently admitted host principal. Every known-ID reader
-- joins the same roots, so signing out or switching accounts immediately
-- changes visibility without deleting authored dependencies.

CREATE VIEW auth_visible_tracks AS
SELECT DISTINCT track.id AS track_id
FROM tracks AS track
CROSS JOIN auth_write_admission AS admission
WHERE admission.singleton = 1
  AND admission.armed = 1
  AND admission.accepting = 1
  AND admission.maintenance = 0
  AND admission.remote_writes = 0
  AND (
      track.uid IS admission.active_uid
      OR EXISTS (
          SELECT 1
          FROM scores AS score
          JOIN auth_venue_access AS access ON access.venue_id = score.venue_id
          WHERE score.track_id = track.id
      )
  );

CREATE VIEW auth_visible_patterns AS
SELECT DISTINCT pattern.id AS pattern_id
FROM patterns AS pattern
CROSS JOIN auth_write_admission AS admission
WHERE admission.singleton = 1
  AND admission.armed = 1
  AND admission.accepting = 1
  AND admission.maintenance = 0
  AND admission.remote_writes = 0
  AND (
      pattern.uid IS admission.active_uid
      OR pattern.is_verified = 1
      OR EXISTS (
          SELECT 1
          FROM track_scores AS clip
          JOIN scores AS score ON score.id = clip.score_id
          JOIN auth_venue_access AS access ON access.venue_id = score.venue_id
          WHERE clip.pattern_id = pattern.id
      )
      OR EXISTS (
          SELECT 1
          FROM cues AS cue
          JOIN auth_venue_access AS access ON access.venue_id = cue.venue_id
          WHERE cue.pattern_id = pattern.id
      )
      OR EXISTS (
          SELECT 1
          FROM venue_implementation_overrides AS override
          JOIN auth_venue_access AS access ON access.venue_id = override.venue_id
          WHERE override.pattern_id = pattern.id
      )
  );

-- Git repository descriptors bind these identities permanently. No ordinary,
-- remote, or maintenance update may move a live row into a different authored
-- repository or principal namespace; replacement is delete/create through the
-- owning lifecycle protocol.
CREATE TRIGGER authored_track_identity_immutable
BEFORE UPDATE ON tracks FOR EACH ROW
WHEN NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid
BEGIN SELECT RAISE(ABORT, 'track authored identity is immutable'); END;

CREATE TRIGGER authored_pattern_identity_immutable
BEFORE UPDATE ON patterns FOR EACH ROW
WHEN NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid
BEGIN SELECT RAISE(ABORT, 'pattern authored identity is immutable'); END;

CREATE TRIGGER authored_implementation_identity_immutable
BEFORE UPDATE ON implementations FOR EACH ROW
WHEN NEW.id IS NOT OLD.id
  OR NEW.uid IS NOT OLD.uid
  OR NEW.pattern_id IS NOT OLD.pattern_id
BEGIN SELECT RAISE(ABORT, 'implementation authored identity is immutable'); END;
