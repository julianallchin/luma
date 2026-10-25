-- The client reads the current format only. Everything below was read by the
-- runtime conversion off the pre-graph venue schema, which is gone:
--
-- - `stage_pieces` and `fixtures.pos_*`/`rot_*`: the old poses. A venue's
--   pieces and lights are `venue_nodes` rows.
-- - `fixture_group_overrides`: edits on top of live-derived groups. Groups are
--   saved `fixture_groups` rows.
-- - `venues.groups_initialized`: whether a venue's derived groups had been
--   saved yet.
--
-- `track_waveforms` is this device's cache and nothing syncs it. It is rebuilt
-- with its band gains NOT NULL, so a cached row always carries the units its
-- envelopes are in; any track opened afterwards computes its row again.
--
-- From here on a local migration is DDL only. The local tables are a
-- projection of the server: a format change is a server migration, and a
-- client that can no longer read what it downloaded bumps
-- `database::PROJECTION_VERSION`, which re-downloads. `user_version` holds the
-- projection version this file's schema reads.
--
-- The matching Postgres change is
-- `supabase/migrations/20261024000200_drop_venue_conversion.sql`.

-- The indexes and triggers on these two go with them.
DROP TABLE stage_pieces;
DROP TABLE fixture_group_overrides;

ALTER TABLE venues DROP COLUMN groups_initialized;

ALTER TABLE fixtures DROP COLUMN pos_x;
ALTER TABLE fixtures DROP COLUMN pos_y;
ALTER TABLE fixtures DROP COLUMN pos_z;
ALTER TABLE fixtures DROP COLUMN rot_x;
ALTER TABLE fixtures DROP COLUMN rot_y;
ALTER TABLE fixtures DROP COLUMN rot_z;

DROP TABLE track_waveforms;
CREATE TABLE track_waveforms (
    track_id TEXT PRIMARY KEY,
    uid TEXT,
    preview_samples_blob BLOB NOT NULL,
    full_samples_blob BLOB,
    bands_blob BLOB,
    preview_bands_blob BLOB,
    decoded_duration REAL,
    band_gain_low REAL NOT NULL,
    band_gain_mid REAL NOT NULL,
    band_gain_high REAL NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);
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
CREATE TRIGGER auth_admit_track_waveforms_delete BEFORE DELETE ON track_waveforms FOR EACH ROW
WHEN (SELECT armed FROM auth_write_admission WHERE singleton = 1) = 1
 AND (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND ((SELECT accepting FROM auth_write_admission WHERE singleton = 1) = 0
      OR OLD.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1))
BEGIN SELECT RAISE(ABORT, 'signed-in delete admission is closed or principal-mismatched'); END;
CREATE TRIGGER track_waveforms_updated_at AFTER UPDATE ON track_waveforms FOR EACH ROW
BEGIN UPDATE track_waveforms SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE track_id = OLD.track_id; END;

PRAGMA user_version = 1;
