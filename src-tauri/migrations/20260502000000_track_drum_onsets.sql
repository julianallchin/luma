-- Per-track drum-onset transcription from ADTOF-pytorch.
--
-- Stores a JSON blob of {midi_note: [timestamps_in_seconds, ...]} keyed by
-- track. Mirrors track_roots' column shape (origin, processor_version,
-- synced_at, version, updated_at) so a follow-up PR can flip on remote sync
-- by adding a Supabase migration + entry in `sync::registry::TABLES`. This
-- migration is local-only — no entry in TABLES, no sync_delete trigger.

CREATE TABLE track_drum_onsets (
    track_id          TEXT PRIMARY KEY,
    uid               TEXT,
    onsets_json       TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    origin            TEXT NOT NULL DEFAULT 'local',
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version           INTEGER NOT NULL DEFAULT 1,
    synced_at         TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_drum_onsets_updated_at AFTER UPDATE ON track_drum_onsets FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_drum_onsets
            SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;
