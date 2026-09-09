-- Preserve old undone approvals as unreviewed, never as negative labels.
CREATE TABLE track_beat_validations_next (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    uid TEXT,
    track_hash TEXT NOT NULL,
    grid_json TEXT NOT NULL,
    processor_version INTEGER NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('unreviewed', 'correct', 'incorrect')),
    reason TEXT CHECK (reason IS NULL OR (verdict = 'incorrect' AND reason IN ('tempo', 'offset', 'drift', 'bar_phase'))),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    origin TEXT NOT NULL DEFAULT 'local'
);
INSERT INTO track_beat_validations_next
(track_id, uid, track_hash, grid_json, processor_version, verdict, created_at, updated_at, version, synced_at, origin)
SELECT track_id, uid, track_hash, grid_json, processor_version,
CASE approved WHEN 1 THEN 'correct' ELSE 'unreviewed' END,
created_at, updated_at, version, synced_at, origin FROM track_beat_validations;
DROP TABLE track_beat_validations;
ALTER TABLE track_beat_validations_next RENAME TO track_beat_validations;
CREATE TRIGGER track_beat_validations_updated_at AFTER UPDATE ON track_beat_validations
FOR EACH ROW WHEN OLD.version = NEW.version
BEGIN
    UPDATE track_beat_validations
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1
    WHERE track_id = OLD.track_id;
END;
