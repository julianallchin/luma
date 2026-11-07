-- Controllers, synced: a venue has any number, each with the MIDI port it
-- matches and a profile (`inputs::profile::Profile` as JSON: where its pads
-- and faders are and how it shows a light). `port` is empty for one not yet
-- heard; its bindings then hear every input.
--
-- `control_bindings` becomes synced and belongs to a controller: one
-- binding per controller, control and input. The bindings this machine
-- kept before move to `control_bindings_unsynced`, a local table: the app
-- moves each venue's into a controller of its own the next time the venue's
-- inputs open (`inputs::store::adopt`), as an ordinary write, so it
-- uploads. A migration's own writes are never uploaded.
--
-- Symmetric with supabase/migrations/20261107120000_perform_controllers.sql.
CREATE TABLE perform_controllers (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    port TEXT NOT NULL DEFAULT '',
    profile_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_perform_controllers_venue ON perform_controllers(venue_id);
CREATE TRIGGER perform_controllers_updated_at AFTER UPDATE ON perform_controllers FOR EACH ROW
BEGIN UPDATE perform_controllers SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

ALTER TABLE control_bindings RENAME TO control_bindings_unsynced;
DROP INDEX idx_control_bindings_venue;
DROP INDEX idx_control_bindings_input;

CREATE TABLE control_bindings (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    controller_id TEXT NOT NULL,
    control TEXT NOT NULL,
    address_json TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('fader', 'momentary', 'toggle')),
    invert INTEGER NOT NULL DEFAULT 0 CHECK (invert IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (controller_id) REFERENCES perform_controllers(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_control_bindings_venue ON control_bindings(venue_id);
CREATE UNIQUE INDEX idx_control_bindings_input
    ON control_bindings(controller_id, control, address_json);
CREATE TRIGGER control_bindings_updated_at AFTER UPDATE ON control_bindings FOR EACH ROW
BEGIN UPDATE control_bindings SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
