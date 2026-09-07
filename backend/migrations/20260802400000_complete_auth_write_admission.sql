-- Complete the signed-write admission invariant for stale deletes and for
-- principal-bearing local/cache tables outside generic sync. Trusted logout
-- deletion sets maintenance=1 only inside its IMMEDIATE transaction, then
-- restores maintenance=0 before commit.

CREATE TRIGGER auth_admit_stage_pieces_insert BEFORE INSERT ON stage_pieces FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_stage_pieces_update BEFORE UPDATE ON stage_pieces FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_waveforms_insert BEFORE INSERT ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_waveforms_update BEFORE UPDATE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_sync_state_insert BEFORE INSERT ON sync_state FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_sync_state_update BEFORE UPDATE ON sync_state FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_venues_delete BEFORE DELETE ON venues FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_tracks_delete BEFORE DELETE ON tracks FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_fixtures_delete BEFORE DELETE ON fixtures FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_patterns_delete BEFORE DELETE ON patterns FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_fixture_groups_delete BEFORE DELETE ON fixture_groups FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_midi_modifiers_delete BEFORE DELETE ON midi_modifiers FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_scores_delete BEFORE DELETE ON scores FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_beats_delete BEFORE DELETE ON track_beats FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_roots_delete BEFORE DELETE ON track_roots FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_stems_delete BEFORE DELETE ON track_stems FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_drum_onsets_delete BEFORE DELETE ON track_drum_onsets FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_bar_classifications_delete BEFORE DELETE ON track_bar_classifications FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_fixture_group_members_delete BEFORE DELETE ON fixture_group_members FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_cues_delete BEFORE DELETE ON cues FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_midi_bindings_delete BEFORE DELETE ON midi_bindings FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_implementations_delete BEFORE DELETE ON implementations FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_scores_delete BEFORE DELETE ON track_scores FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_pattern_categories_delete BEFORE DELETE ON pattern_categories FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_venue_overrides_delete BEFORE DELETE ON venue_implementation_overrides FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_stage_pieces_delete BEFORE DELETE ON stage_pieces FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_waveforms_delete BEFORE DELETE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_sync_state_delete BEFORE DELETE ON sync_state FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;


