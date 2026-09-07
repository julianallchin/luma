-- preprocessing_runs: single source of truth for "has preprocessor X run for
-- track Y at version V". Replaces ad-hoc track_has_beats / track_has_stems /
-- track_has_roots existence checks. Bumping a preprocessor's version
-- invalidates older rows so app updates auto-trigger backfills.

CREATE TABLE preprocessing_runs (
    track_id TEXT NOT NULL,
    preprocessor TEXT NOT NULL,
    version INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running','completed','failed')),
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    completed_at TEXT,
    error TEXT,
    PRIMARY KEY (track_id, preprocessor),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE INDEX idx_preprocessing_runs_status ON preprocessing_runs(status);

-- Backfill: tracks that already have beats/stems/roots are recorded as
-- completed at version 1. Without this, every existing track would be
-- reprocessed on first launch after this migration ships.
INSERT OR IGNORE INTO preprocessing_runs (track_id, preprocessor, version, status, completed_at)
SELECT track_id, 'beat_grid', 1, 'completed', strftime('%Y-%m-%dT%H:%M:%SZ','now')
FROM track_beats;

INSERT OR IGNORE INTO preprocessing_runs (track_id, preprocessor, version, status, completed_at)
SELECT DISTINCT track_id, 'stems', 1, 'completed', strftime('%Y-%m-%dT%H:%M:%SZ','now')
FROM track_stems;

INSERT OR IGNORE INTO preprocessing_runs (track_id, preprocessor, version, status, completed_at)
SELECT track_id, 'roots', 1, 'completed', strftime('%Y-%m-%dT%H:%M:%SZ','now')
FROM track_roots;
