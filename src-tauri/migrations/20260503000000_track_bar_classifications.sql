-- Per-track bar classifier output (intensity + multi-label tag probabilities).
--
-- Stores the worker's full per-bar JSON list verbatim — `predictions` is
-- the schema, not flattened columns, so future schema swaps (legacy 9-tag →
-- new 7-head) are pure preprocessor-version bumps with no migration.
--
-- Mirrors track_roots' column shape (origin, processor_version, synced_at,
-- version, updated_at). Local-only — a follow-up PR can flip on remote sync
-- by adding a Supabase migration + entry in `sync::registry::TABLES`.

CREATE TABLE track_bar_classifications (
    track_id           TEXT PRIMARY KEY,
    uid                TEXT,
    classifications_json TEXT NOT NULL,
    tag_order_json     TEXT NOT NULL,
    processor_version  INTEGER NOT NULL DEFAULT 1,
    origin             TEXT NOT NULL DEFAULT 'local',
    created_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version            INTEGER NOT NULL DEFAULT 1,
    synced_at          TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_bar_classifications_updated_at AFTER UPDATE ON track_bar_classifications FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_bar_classifications
            SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;
