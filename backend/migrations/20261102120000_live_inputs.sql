-- Live inputs: this machine's MIDI and OSC bindings to a venue's controls.
-- Local only, like the devices plugged into the machine.
CREATE TABLE control_bindings (
    id TEXT PRIMARY KEY,
    venue_id TEXT NOT NULL REFERENCES venues(id) ON DELETE CASCADE,
    control TEXT NOT NULL,
    address_json TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('fader', 'momentary', 'toggle')),
    invert INTEGER NOT NULL DEFAULT 0 CHECK (invert IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_control_bindings_venue ON control_bindings(venue_id);

-- The MIDI controller and DJ mixer mappings they replace. The synced tables
-- leave the server in supabase/migrations/20261102120000_drop_midi_mappings.sql.
DROP TABLE IF EXISTS midi_bindings;
DROP TABLE IF EXISTS midi_modifiers;
ALTER TABLE venues DROP COLUMN controller_port;
ALTER TABLE venues DROP COLUMN mixer_port;
ALTER TABLE venues DROP COLUMN mixer_mapping_json;
