-- Per-track cached MERT-95M layer-7 features for the full-mix audio.
--
-- Stores a file path (no inline blob) — features for a 4-min track are
-- ~27 MB at fp16 / 75 Hz / 768-d, far too large for SQLite's row format.
-- The .npy lives under <app_config>/tracks/mert/<track_hash>.npy and is
-- regenerated on demand if the file is missing (verify_disk hook).
--
-- Local-only artifact: not in `sync::registry::TABLES`. We don't sync raw
-- features — they're cheap to recompute on the receiving device and a single
-- MERT extraction takes seconds. The corresponding tracks(id) FK keeps the
-- row tied to its track for cascade-delete on track removal.

CREATE TABLE track_mert (
    track_id          TEXT PRIMARY KEY,
    file_path         TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_mert_updated_at AFTER UPDATE ON track_mert FOR EACH ROW
    BEGIN
        UPDATE track_mert
            SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
        WHERE track_id = OLD.track_id;
    END;
