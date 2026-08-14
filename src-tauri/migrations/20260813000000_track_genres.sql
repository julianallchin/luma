-- Per-track, per-bar Discogs style activations (Discogs-EffNet, 400 styles).
--
-- `genres_json` holds `{"bars": [{bar_idx, start, end, top: [[label_idx, p], ...]}, ...],
--  "track_top": [[label_idx, p], ...]}` — the per-bar lists are sparse top-K,
-- and every `label_idx` indexes into `labels_json`, the compacted list of style
-- names this track actually uses. Keeping the taxonomy per row (rather than a
-- shared 400-name table) means a model or taxonomy swap is a pure
-- preprocessor-version bump with no migration, exactly like
-- track_bar_classifications' tag_order_json.
--
-- Mirrors track_bar_classifications' column shape (uid, processor_version,
-- origin, created_at, updated_at, version, synced_at) and is registered in
-- `sync::registry::TABLES`. Like track_beats/roots/stems/bar_classifications it
-- has NO sync_delete trigger — the parent track's soft-delete cascades through
-- Supabase, and sync never issues cascade deletes of its own.

CREATE TABLE track_genres (
    track_id          TEXT PRIMARY KEY,
    uid               TEXT,
    genres_json       TEXT NOT NULL,
    labels_json       TEXT NOT NULL,
    processor_version INTEGER NOT NULL DEFAULT 1,
    origin            TEXT NOT NULL DEFAULT 'local',
    created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version           INTEGER NOT NULL DEFAULT 1,
    synced_at         TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_genres_updated_at AFTER UPDATE ON track_genres FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_genres
            SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;
