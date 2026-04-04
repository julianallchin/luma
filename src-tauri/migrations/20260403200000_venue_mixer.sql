-- Per-venue MIDI mixer configuration (local-only, not synced).
-- Port names are machine-specific; mapping is user-configured via learn flow.
ALTER TABLE venues ADD COLUMN mixer_port TEXT;
ALTER TABLE venues ADD COLUMN mixer_mapping_json TEXT;
