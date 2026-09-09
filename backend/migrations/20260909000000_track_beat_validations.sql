-- Keep the reviewed grid even when track_beats is regenerated.
CREATE TABLE track_beat_validations (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    uid TEXT,
    track_hash TEXT NOT NULL,
    grid_json TEXT NOT NULL,
    processor_version INTEGER NOT NULL,
    approved INTEGER NOT NULL CHECK (approved IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    origin TEXT NOT NULL DEFAULT 'local'
);
CREATE TRIGGER track_beat_validations_updated_at AFTER UPDATE ON track_beat_validations
FOR EACH ROW WHEN OLD.version = NEW.version
BEGIN
    UPDATE track_beat_validations
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1
    WHERE track_id = OLD.track_id;
END;
