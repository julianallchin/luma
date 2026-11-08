-- A venue member could not open a song another member owns: computing its
-- waveform aborted with "signed-in write admission is closed or
-- principal-mismatched".
--
-- `track_waveforms` is this device's cache, never synced. Its row carries the
-- track's owner, like every child of a track, but admission demanded that the
-- owner be the signed-in principal, which a shared track's owner is not. What
-- the cache has to guarantee is narrower: it is written only for a track the
-- signed-in principal can see, under that track's own uid. A delete is refused
-- only for a track that is still here and not visible, so the cascade from a
-- shared track that sync removes goes through.

DROP TRIGGER auth_admit_track_waveforms_insert;
DROP TRIGGER auth_admit_track_waveforms_update;
DROP TRIGGER auth_admit_track_waveforms_delete;

CREATE TRIGGER auth_admit_track_waveforms_insert BEFORE INSERT ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NOT EXISTS (SELECT 1 FROM auth_visible_tracks WHERE track_id = NEW.track_id)
      OR NEW.uid IS NOT (SELECT uid FROM tracks WHERE id = NEW.track_id))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_waveforms_update BEFORE UPDATE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR NEW.track_id IS NOT OLD.track_id
      OR NOT EXISTS (SELECT 1 FROM auth_visible_tracks WHERE track_id = NEW.track_id)
      OR NEW.uid IS NOT (SELECT uid FROM tracks WHERE id = NEW.track_id))
BEGIN SELECT RAISE(ABORT, 'signed-in write admission is closed or principal-mismatched'); END;

CREATE TRIGGER auth_admit_track_waveforms_delete BEFORE DELETE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR (EXISTS (SELECT 1 FROM tracks WHERE id = OLD.track_id)
          AND NOT EXISTS (SELECT 1 FROM auth_visible_tracks WHERE track_id = OLD.track_id)))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;
