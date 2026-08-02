-- Venue data is one authorization aggregate. Owners may read and write it;
-- joined members may read it but are deliberately read-only. Row `uid` is
-- provenance, not an independent access-control root. Guest access uses the
-- same owner predicate with an open guest admission (`active_uid IS NULL`) and
-- a guest-owned venue. Remote pull and transaction-local maintenance remain
-- explicit host bypasses.

CREATE VIEW auth_venue_access AS
SELECT
    venue.id AS venue_id,
    CASE
        WHEN admission.active_uid IS NULL
             AND venue.uid IS NULL
             AND venue.role != 'member'
            THEN 1
        WHEN admission.active_uid IS NOT NULL
             AND venue.uid IS admission.active_uid
            THEN 1
        ELSE 0
    END AS owner_access
FROM venues AS venue
CROSS JOIN auth_write_admission AS admission
WHERE admission.singleton = 1
  AND admission.armed = 1
  AND admission.accepting = 1
  AND admission.maintenance = 0
  AND admission.remote_writes = 0
  AND (
      (admission.active_uid IS NULL
       AND venue.uid IS NULL
       AND venue.role != 'member')
      OR
      (admission.active_uid IS NOT NULL AND (
          venue.uid IS admission.active_uid
          OR EXISTS (
              SELECT 1
              FROM venue_memberships AS membership
              WHERE membership.venue_id = venue.id
                AND membership.user_id = admission.active_uid
                AND membership.role = 'member'
          )
      ))
  );

-- The generic UID guard is deliberately replaced for the complete venue
-- aggregate. All replacement triggers below use auth_venue_access.
DROP TRIGGER auth_admit_venues_insert;
DROP TRIGGER auth_admit_venues_update;
DROP TRIGGER auth_admit_venues_delete;

DROP TRIGGER auth_admit_fixtures_insert;
DROP TRIGGER auth_admit_fixtures_update;
DROP TRIGGER auth_admit_fixtures_delete;

DROP TRIGGER auth_admit_fixture_groups_insert;
DROP TRIGGER auth_admit_fixture_groups_update;
DROP TRIGGER auth_admit_fixture_groups_delete;

DROP TRIGGER auth_admit_fixture_group_members_insert;
DROP TRIGGER auth_admit_fixture_group_members_update;
DROP TRIGGER auth_admit_fixture_group_members_delete;

DROP TRIGGER auth_admit_stage_pieces_insert;
DROP TRIGGER auth_admit_stage_pieces_update;
DROP TRIGGER auth_admit_stage_pieces_delete;

DROP TRIGGER auth_admit_cues_insert;
DROP TRIGGER auth_admit_cues_update;
DROP TRIGGER auth_admit_cues_delete;

DROP TRIGGER auth_admit_midi_modifiers_insert;
DROP TRIGGER auth_admit_midi_modifiers_update;
DROP TRIGGER auth_admit_midi_modifiers_delete;

DROP TRIGGER auth_admit_midi_bindings_insert;
DROP TRIGGER auth_admit_midi_bindings_update;
DROP TRIGGER auth_admit_midi_bindings_delete;

DROP TRIGGER auth_admit_scores_insert;
DROP TRIGGER auth_admit_scores_update;
DROP TRIGGER auth_admit_scores_delete;

DROP TRIGGER auth_admit_track_scores_insert;
DROP TRIGGER auth_admit_track_scores_update;
DROP TRIGGER auth_admit_track_scores_delete;

DROP TRIGGER auth_admit_venue_overrides_insert;
DROP TRIGGER auth_admit_venue_overrides_update;
DROP TRIGGER auth_admit_venue_overrides_delete;

-- Venue memberships are authority, not ordinary venue content. Only an
-- authenticated remote transaction may install them, and it may install only
-- the active principal's read-only membership. Ordinary SQL can remove only
-- that same principal's own membership. Transaction-local maintenance remains
-- available for an already-authorized owner cascade.
CREATE TRIGGER auth_venue_membership_insert
BEFORE INSERT ON venue_memberships FOR EACH ROW
WHEN NOT COALESCE((
    SELECT armed = 1 AND accepting = 1 AND maintenance = 0
           AND remote_writes = 1 AND active_uid IS NOT NULL
           AND NEW.user_id IS active_uid AND NEW.role = 'member'
           AND EXISTS (
               SELECT 1 FROM venues AS venue
               WHERE venue.id = NEW.venue_id
                 AND venue.uid IS NOT NEW.user_id
           )
    FROM auth_write_admission WHERE singleton = 1
), 0)
BEGIN SELECT RAISE(ABORT, 'venue membership grant is not authorized'); END;

CREATE TRIGGER auth_venue_membership_update
BEFORE UPDATE ON venue_memberships FOR EACH ROW
WHEN NEW.venue_id IS NOT OLD.venue_id
  OR NEW.user_id IS NOT OLD.user_id
  OR NEW.role IS NOT OLD.role
  OR NEW.role != 'member'
  OR NOT COALESCE((
      SELECT armed = 1 AND accepting = 1 AND maintenance = 0
             AND remote_writes = 1 AND active_uid IS NOT NULL
             AND OLD.user_id IS active_uid
      FROM auth_write_admission WHERE singleton = 1
  ), 0)
BEGIN SELECT RAISE(ABORT, 'venue membership update is not authorized'); END;

CREATE TRIGGER auth_venue_membership_delete
BEFORE DELETE ON venue_memberships FOR EACH ROW
WHEN NOT COALESCE((
    SELECT armed = 1 AND (
        maintenance = 1
        OR (
            accepting = 1 AND maintenance = 0
            AND active_uid IS NOT NULL AND OLD.user_id IS active_uid
        )
    )
    FROM auth_write_admission WHERE singleton = 1
), 0)
BEGIN SELECT RAISE(ABORT, 'venue membership deletion is not authorized'); END;

-- A root can be created only as the current principal's venue (or as a guest
-- venue in guest mode). Joined roots are installed by the authenticated remote
-- transaction, never by weakening this ordinary insert predicate.
CREATE TRIGGER auth_venue_insert
BEFORE INSERT ON venues FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT COALESCE((
        SELECT accepting = 1 AND NEW.uid IS active_uid
               AND ((active_uid IS NULL AND NEW.role != 'member')
                    OR active_uid IS NOT NULL)
        FROM auth_write_admission WHERE singleton = 1
     ), 0)
BEGIN
    SELECT RAISE(ABORT, 'venue write is not authorized');
END;

CREATE TRIGGER auth_venue_update
BEFORE UPDATE ON venues FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.id IS NOT OLD.id
      OR NEW.uid IS NOT OLD.uid
      OR NEW.role IS NOT OLD.role
      OR NOT EXISTS (
          SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.id AND owner_access = 1
      )
 )
BEGIN
    SELECT RAISE(ABORT, 'venue write is not authorized');
END;

CREATE TRIGGER auth_venue_delete
BEFORE DELETE ON venues FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
     SELECT 1 FROM auth_venue_access
     WHERE venue_id = OLD.id AND owner_access = 1
 )
BEGIN
    SELECT RAISE(ABORT, 'venue deletion requires owner access');
END;

-- Direct descendants. Inserts bind provenance to the current host principal;
-- updates cannot move a row between aggregates or rewrite its provenance.

CREATE TRIGGER auth_venue_fixture_insert
BEFORE INSERT ON fixtures FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
 )
BEGIN SELECT RAISE(ABORT, 'fixture write is not authorized'); END;

CREATE TRIGGER auth_venue_fixture_update
BEFORE UPDATE ON fixtures FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'fixture write is not authorized'); END;

CREATE TRIGGER auth_venue_fixture_delete
BEFORE DELETE ON fixtures FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'fixture write is not authorized'); END;

CREATE TRIGGER auth_venue_group_insert
BEFORE INSERT ON fixture_groups FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
 )
BEGIN SELECT RAISE(ABORT, 'fixture group write is not authorized'); END;

CREATE TRIGGER auth_venue_group_update
BEFORE UPDATE ON fixture_groups FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'fixture group write is not authorized'); END;

CREATE TRIGGER auth_venue_group_delete
BEFORE DELETE ON fixture_groups FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'fixture group write is not authorized'); END;

CREATE TRIGGER auth_venue_stage_piece_insert
BEFORE INSERT ON stage_pieces FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
      OR (NEW.parent_piece_id IS NOT NULL AND NOT EXISTS (
          SELECT 1 FROM stage_pieces AS parent
          WHERE parent.id = NEW.parent_piece_id AND parent.venue_id = NEW.venue_id
      ))
 )
BEGIN SELECT RAISE(ABORT, 'stage piece write is not authorized'); END;

CREATE TRIGGER auth_venue_stage_piece_update
BEFORE UPDATE ON stage_pieces FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
      OR (NEW.parent_piece_id IS NOT NULL AND NOT EXISTS (
          SELECT 1 FROM stage_pieces AS parent
          WHERE parent.id = NEW.parent_piece_id AND parent.venue_id = NEW.venue_id
      ))
 )
BEGIN SELECT RAISE(ABORT, 'stage piece write is not authorized'); END;

CREATE TRIGGER auth_venue_stage_piece_delete
BEFORE DELETE ON stage_pieces FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'stage piece write is not authorized'); END;

CREATE TRIGGER auth_venue_cue_insert
BEFORE INSERT ON cues FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'cue write is not authorized'); END;

CREATE TRIGGER auth_venue_cue_update
BEFORE UPDATE ON cues FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'cue write is not authorized'); END;

CREATE TRIGGER auth_venue_cue_delete
BEFORE DELETE ON cues FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'cue write is not authorized'); END;

CREATE TRIGGER auth_venue_midi_modifier_insert
BEFORE INSERT ON midi_modifiers FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'MIDI modifier write is not authorized'); END;

CREATE TRIGGER auth_venue_midi_modifier_update
BEFORE UPDATE ON midi_modifiers FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'MIDI modifier write is not authorized'); END;

CREATE TRIGGER auth_venue_midi_modifier_delete
BEFORE DELETE ON midi_modifiers FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'MIDI modifier write is not authorized'); END;

CREATE TRIGGER auth_venue_midi_binding_insert
BEFORE INSERT ON midi_bindings FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'MIDI binding write is not authorized'); END;

CREATE TRIGGER auth_venue_midi_binding_update
BEFORE UPDATE ON midi_bindings FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'MIDI binding write is not authorized'); END;

CREATE TRIGGER auth_venue_midi_binding_delete
BEFORE DELETE ON midi_bindings FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'MIDI binding write is not authorized'); END;

CREATE TRIGGER auth_venue_score_insert
BEFORE INSERT ON scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'score write is not authorized'); END;

CREATE TRIGGER auth_venue_score_update
BEFORE UPDATE ON scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid
      OR NEW.venue_id IS NOT OLD.venue_id OR NEW.track_id IS NOT OLD.track_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'score write is not authorized'); END;

CREATE TRIGGER auth_venue_score_delete
BEFORE DELETE ON scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'score write is not authorized'); END;

CREATE TRIGGER auth_venue_override_insert
BEFORE INSERT ON venue_implementation_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'venue override write is not authorized'); END;

CREATE TRIGGER auth_venue_override_update
BEFORE UPDATE ON venue_implementation_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (NEW.venue_id IS NOT OLD.venue_id OR NEW.pattern_id IS NOT OLD.pattern_id
      OR NEW.uid IS NOT OLD.uid
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1))
BEGIN SELECT RAISE(ABORT, 'venue override write is not authorized'); END;

CREATE TRIGGER auth_venue_override_delete
BEFORE DELETE ON venue_implementation_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'venue override write is not authorized'); END;

-- Relationship descendants resolve through their parent aggregate and reject
-- cross-venue edges even when both IDs are individually known.

CREATE TRIGGER auth_venue_group_member_insert
BEFORE INSERT ON fixture_group_members FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (
          SELECT 1
          FROM fixture_groups AS group_row
          JOIN fixtures AS fixture ON fixture.id = NEW.fixture_id
          JOIN auth_venue_access AS access ON access.venue_id = group_row.venue_id
          WHERE group_row.id = NEW.group_id
            AND fixture.venue_id = group_row.venue_id
            AND access.owner_access = 1
      )
 )
BEGIN SELECT RAISE(ABORT, 'fixture group membership is not authorized'); END;

CREATE TRIGGER auth_venue_group_member_update
BEFORE UPDATE ON fixture_group_members FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid
      OR NEW.fixture_id IS NOT OLD.fixture_id OR NEW.group_id IS NOT OLD.group_id
      OR NOT EXISTS (
          SELECT 1
          FROM fixture_groups AS group_row
          JOIN fixtures AS fixture ON fixture.id = OLD.fixture_id
          JOIN auth_venue_access AS access ON access.venue_id = group_row.venue_id
          WHERE group_row.id = OLD.group_id
            AND fixture.venue_id = group_row.venue_id
            AND access.owner_access = 1
      )
 )
BEGIN SELECT RAISE(ABORT, 'fixture group membership is not authorized'); END;

CREATE TRIGGER auth_venue_group_member_delete
BEFORE DELETE ON fixture_group_members FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
     SELECT 1
     FROM fixture_groups AS group_row
     JOIN fixtures AS fixture ON fixture.id = OLD.fixture_id
     JOIN auth_venue_access AS access ON access.venue_id = group_row.venue_id
     WHERE group_row.id = OLD.group_id
       AND fixture.venue_id = group_row.venue_id
       AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'fixture group membership is not authorized'); END;

CREATE TRIGGER auth_venue_track_score_insert
BEFORE INSERT ON track_scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (
          SELECT 1
          FROM scores AS score
          JOIN auth_venue_access AS access ON access.venue_id = score.venue_id
          WHERE score.id = NEW.score_id
            AND access.owner_access = 1
      )
 )
BEGIN SELECT RAISE(ABORT, 'track score write is not authorized'); END;

CREATE TRIGGER auth_venue_track_score_update
BEFORE UPDATE ON track_scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.score_id IS NOT OLD.score_id
      OR NOT EXISTS (
          SELECT 1
          FROM scores AS score
          JOIN auth_venue_access AS access ON access.venue_id = score.venue_id
          WHERE score.id = OLD.score_id
            AND access.owner_access = 1
      )
 )
BEGIN SELECT RAISE(ABORT, 'track score write is not authorized'); END;

CREATE TRIGGER auth_venue_track_score_delete
BEFORE DELETE ON track_scores FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
     SELECT 1
     FROM scores AS score
     JOIN auth_venue_access AS access ON access.venue_id = score.venue_id
     WHERE score.id = OLD.score_id
       AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'track score write is not authorized'); END;
