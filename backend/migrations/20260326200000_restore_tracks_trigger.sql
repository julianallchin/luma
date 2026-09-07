-- Restore the tracks_updated_at trigger that was destroyed by the
-- tracks_drop_hash_unique migration (DROP TABLE wipes triggers).
-- This is a no-op if the trigger already exists (e.g. fresh installs
-- that ran the fixed migration).

CREATE TRIGGER IF NOT EXISTS tracks_updated_at AFTER UPDATE ON tracks FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE tracks SET updated_at = CURRENT_TIMESTAMP, version = OLD.version + 1 WHERE id = OLD.id; END;
