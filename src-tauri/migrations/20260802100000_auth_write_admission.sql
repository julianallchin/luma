-- Signed-in writes are admitted by the app database, not by a caller's stale
-- notion of the current session. `armed = 0` is a bootstrap state used only
-- until the app/harness has loaded state.db; no command surface is exposed
-- before the host arms this row. Once armed, guest writes (`uid IS NULL`) are
-- always legal, while signed writes require an open admission for the active
-- principal. A pull transaction must explicitly enter `remote_writes`; row
-- provenance such as `origin = 'remote'` is data and grants no authority.
CREATE TABLE auth_write_admission (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    armed      INTEGER NOT NULL DEFAULT 0 CHECK (armed IN (0, 1)),
    accepting  INTEGER NOT NULL DEFAULT 0 CHECK (accepting IN (0, 1)),
    maintenance INTEGER NOT NULL DEFAULT 0 CHECK (maintenance IN (0, 1)),
    remote_writes INTEGER NOT NULL DEFAULT 0 CHECK (remote_writes IN (0, 1)),
    active_uid TEXT,
    generation INTEGER NOT NULL DEFAULT 0,
    CHECK (maintenance = 0 OR (accepting = 0 AND remote_writes = 0)),
    CHECK (remote_writes = 0 OR (accepting = 1 AND maintenance = 0 AND active_uid IS NOT NULL))
);

INSERT INTO auth_write_admission (singleton) VALUES (1);

-- Tables carrying `origin` share the same guard. SQLite has no table-generic
-- trigger, so these intentionally repeat the invariant at each storage owner.

CREATE TRIGGER auth_admit_venues_insert BEFORE INSERT ON venues FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_venues_update BEFORE UPDATE ON venues FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_tracks_insert BEFORE INSERT ON tracks FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_tracks_update BEFORE UPDATE ON tracks FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_fixtures_insert BEFORE INSERT ON fixtures FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_fixtures_update BEFORE UPDATE ON fixtures FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_patterns_insert BEFORE INSERT ON patterns FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_patterns_update BEFORE UPDATE ON patterns FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_fixture_groups_insert BEFORE INSERT ON fixture_groups FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_fixture_groups_update BEFORE UPDATE ON fixture_groups FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_midi_modifiers_insert BEFORE INSERT ON midi_modifiers FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_midi_modifiers_update BEFORE UPDATE ON midi_modifiers FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_scores_insert BEFORE INSERT ON scores FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_scores_update BEFORE UPDATE ON scores FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_beats_insert BEFORE INSERT ON track_beats FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_beats_update BEFORE UPDATE ON track_beats FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_roots_insert BEFORE INSERT ON track_roots FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_roots_update BEFORE UPDATE ON track_roots FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_stems_insert BEFORE INSERT ON track_stems FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_stems_update BEFORE UPDATE ON track_stems FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_drum_onsets_insert BEFORE INSERT ON track_drum_onsets FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_drum_onsets_update BEFORE UPDATE ON track_drum_onsets FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_bar_classifications_insert BEFORE INSERT ON track_bar_classifications FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_bar_classifications_update BEFORE UPDATE ON track_bar_classifications FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_fixture_group_members_insert BEFORE INSERT ON fixture_group_members FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_fixture_group_members_update BEFORE UPDATE ON fixture_group_members FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_cues_insert BEFORE INSERT ON cues FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_cues_update BEFORE UPDATE ON cues FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_midi_bindings_insert BEFORE INSERT ON midi_bindings FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_midi_bindings_update BEFORE UPDATE ON midi_bindings FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

-- Authored projection tables still carry `origin` even though their document
-- bytes no longer participate in generic relational sync.
CREATE TRIGGER auth_admit_implementations_insert BEFORE INSERT ON implementations FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_implementations_update BEFORE UPDATE ON implementations FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_scores_insert BEFORE INSERT ON track_scores FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_track_scores_update BEFORE UPDATE ON track_scores FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR ((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0 AND (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

-- These legacy routing/catalog tables predate provenance. They only admit the
-- active principal; remote sync no longer writes venue overrides.
CREATE TRIGGER auth_admit_pattern_categories_insert BEFORE INSERT ON pattern_categories FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_pattern_categories_update BEFORE UPDATE ON pattern_categories FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_venue_overrides_insert BEFORE INSERT ON venue_implementation_overrides FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
CREATE TRIGGER auth_admit_venue_overrides_update BEFORE UPDATE ON venue_implementation_overrides FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1) OR NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;
