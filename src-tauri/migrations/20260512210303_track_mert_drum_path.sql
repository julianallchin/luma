-- Add a second on-disk MERT cache path: the drum-stem cache used by the
-- n2n drum-onset preprocessor. The existing `file_path` column continues
-- to hold the full-mix cache (consumed by the bar classifier).
--
-- The MERT preprocessor's version is bumped to invalidate every row at
-- the same time, so reconcile-on-startup will re-run extraction and
-- populate both paths; existing rows can keep their empty `drum_path`
-- placeholder until they're rebuilt.

ALTER TABLE track_mert ADD COLUMN drum_path TEXT NOT NULL DEFAULT '';
