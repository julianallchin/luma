-- Timestamp ordering cannot represent sync dirtiness: a local edit can land in
-- the same SQLite second as `synced_at`. Make dirtiness an explicit bit instead.
-- Local update triggers atomically clear synced_at; pull and mark-synced both
-- increment version, deliberately bypassing these triggers.

DROP TRIGGER IF EXISTS venues_updated_at;
DROP TRIGGER IF EXISTS tracks_updated_at;
DROP TRIGGER IF EXISTS fixtures_updated_at;
DROP TRIGGER IF EXISTS pattern_categories_updated_at;
DROP TRIGGER IF EXISTS patterns_updated_at;
DROP TRIGGER IF EXISTS fixture_groups_updated_at;
DROP TRIGGER IF EXISTS scores_updated_at;
DROP TRIGGER IF EXISTS track_beats_updated_at;
DROP TRIGGER IF EXISTS track_roots_updated_at;
DROP TRIGGER IF EXISTS track_stems_updated_at;
DROP TRIGGER IF EXISTS track_drum_onsets_updated_at;
DROP TRIGGER IF EXISTS track_bar_classifications_updated_at;
DROP TRIGGER IF EXISTS fixture_group_members_updated_at;
DROP TRIGGER IF EXISTS cues_updated_at;
DROP TRIGGER IF EXISTS midi_modifiers_updated_at;
DROP TRIGGER IF EXISTS midi_bindings_updated_at;

CREATE TRIGGER venues_updated_at AFTER UPDATE ON venues FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE venues SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER tracks_updated_at AFTER UPDATE ON tracks FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE tracks SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER fixtures_updated_at AFTER UPDATE ON fixtures FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE fixtures SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER pattern_categories_updated_at AFTER UPDATE ON pattern_categories FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE pattern_categories SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER patterns_updated_at AFTER UPDATE ON patterns FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE patterns SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER fixture_groups_updated_at AFTER UPDATE ON fixture_groups FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE fixture_groups SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER scores_updated_at AFTER UPDATE ON scores FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER track_beats_updated_at AFTER UPDATE ON track_beats FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE track_beats SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE track_id = OLD.track_id;
END;
CREATE TRIGGER track_roots_updated_at AFTER UPDATE ON track_roots FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE track_roots SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE track_id = OLD.track_id;
END;
CREATE TRIGGER track_stems_updated_at AFTER UPDATE ON track_stems FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE track_stems SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name;
END;
CREATE TRIGGER track_drum_onsets_updated_at AFTER UPDATE ON track_drum_onsets FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE track_drum_onsets SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE track_id = OLD.track_id;
END;
CREATE TRIGGER track_bar_classifications_updated_at AFTER UPDATE ON track_bar_classifications FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE track_bar_classifications SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE track_id = OLD.track_id;
END;
CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE fixture_group_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER cues_updated_at AFTER UPDATE ON cues FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE cues SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER midi_modifiers_updated_at AFTER UPDATE ON midi_modifiers FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE midi_modifiers SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;
CREATE TRIGGER midi_bindings_updated_at AFTER UPDATE ON midi_bindings FOR EACH ROW
WHEN OLD.version = NEW.version BEGIN
  UPDATE midi_bindings SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), version = OLD.version + 1, synced_at = NULL WHERE id = OLD.id;
END;

-- Force one safe replay of every locally-owned signed row. This also repairs
-- any same-second edit that the previous timestamp heuristic already missed.
UPDATE venues SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE tracks SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE fixtures SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE pattern_categories SET synced_at = NULL WHERE uid IS NOT NULL;
UPDATE patterns SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE fixture_groups SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE scores SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE track_beats SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE track_roots SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE track_stems SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE track_drum_onsets SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE track_bar_classifications SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE fixture_group_members SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE cues SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE midi_modifiers SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
UPDATE midi_bindings SET synced_at = NULL WHERE uid IS NOT NULL AND origin = 'local';
