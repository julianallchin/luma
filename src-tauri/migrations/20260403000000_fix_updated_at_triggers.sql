-- Fix updated_at triggers to use ISO 8601 UTC format (consistent with Rust/chrono).
-- CURRENT_TIMESTAMP produces "YYYY-MM-DD HH:MM:SS" (space separator) while Rust
-- inserts "YYYY-MM-DDTHH:MM:SS+00:00" (T separator). SQLite string comparison
-- treats T > space, so synced_at (T-format) always appeared > updated_at (space-format),
-- silently marking dirty rows as clean and breaking the sync dirty check.

DROP TRIGGER IF EXISTS venues_updated_at;
DROP TRIGGER IF EXISTS fixtures_updated_at;
DROP TRIGGER IF EXISTS pattern_categories_updated_at;
DROP TRIGGER IF EXISTS patterns_updated_at;
DROP TRIGGER IF EXISTS implementations_updated_at;
DROP TRIGGER IF EXISTS venue_implementation_overrides_updated_at;
DROP TRIGGER IF EXISTS tracks_updated_at;
DROP TRIGGER IF EXISTS track_beats_updated_at;
DROP TRIGGER IF EXISTS track_roots_updated_at;
DROP TRIGGER IF EXISTS track_waveforms_updated_at;
DROP TRIGGER IF EXISTS track_stems_updated_at;
DROP TRIGGER IF EXISTS scores_updated_at;
DROP TRIGGER IF EXISTS track_scores_updated_at;
DROP TRIGGER IF EXISTS fixture_groups_updated_at;
DROP TRIGGER IF EXISTS fixture_group_members_updated_at;

CREATE TRIGGER venues_updated_at AFTER UPDATE ON venues FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE venues SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER fixtures_updated_at AFTER UPDATE ON fixtures FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixtures SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER pattern_categories_updated_at AFTER UPDATE ON pattern_categories FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE pattern_categories SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER patterns_updated_at AFTER UPDATE ON patterns FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE patterns SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER implementations_updated_at AFTER UPDATE ON implementations FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE implementations SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER venue_implementation_overrides_updated_at AFTER UPDATE ON venue_implementation_overrides FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE venue_implementation_overrides SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE venue_id = OLD.venue_id AND pattern_id = OLD.pattern_id; END;

CREATE TRIGGER tracks_updated_at AFTER UPDATE ON tracks FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE tracks SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER track_beats_updated_at AFTER UPDATE ON track_beats FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_beats SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_roots_updated_at AFTER UPDATE ON track_roots FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_roots SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_waveforms_updated_at AFTER UPDATE ON track_waveforms FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_waveforms SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE track_id = OLD.track_id; END;

CREATE TRIGGER track_stems_updated_at AFTER UPDATE ON track_stems FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_stems SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name; END;

CREATE TRIGGER scores_updated_at AFTER UPDATE ON scores FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER track_scores_updated_at AFTER UPDATE ON track_scores FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE track_scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER fixture_groups_updated_at AFTER UPDATE ON fixture_groups FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixture_groups SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

CREATE TRIGGER fixture_group_members_updated_at AFTER UPDATE ON fixture_group_members FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE fixture_group_members SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE fixture_id = OLD.fixture_id AND group_id = OLD.group_id; END;

-- Null out synced_at for any rows where the format mismatch caused them to be
-- incorrectly treated as clean. datetime() normalises both formats for comparison.
UPDATE venues              SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE fixtures            SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE pattern_categories  SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE patterns            SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE implementations     SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE tracks              SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE track_beats         SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE track_roots         SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE track_stems         SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE scores              SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE track_scores        SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE fixture_groups      SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
UPDATE fixture_group_members SET synced_at = NULL WHERE synced_at IS NOT NULL AND datetime(updated_at) > datetime(synced_at);
