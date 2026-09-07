-- Per-venue preferred MIDI controller port name.
-- Local-only (not synced) — port names are machine-specific.
ALTER TABLE venues ADD COLUMN controller_port TEXT;
