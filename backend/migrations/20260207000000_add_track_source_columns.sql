-- Add source tracking columns for DJ library imports (Engine DJ, Rekordbox, etc.)
ALTER TABLE tracks ADD COLUMN source_type TEXT;      -- "engine_dj", "rekordbox", "file", null
ALTER TABLE tracks ADD COLUMN source_id TEXT;         -- opaque stable ID, e.g. "uuid:trackId"
ALTER TABLE tracks ADD COLUMN source_filename TEXT;   -- bare filename for live matching

CREATE INDEX idx_tracks_source ON tracks(source_type, source_id);
CREATE INDEX idx_tracks_source_filename ON tracks(source_filename);
