-- The Perform set: the patterns a venue plays live, and how its Perform tab
-- lays out the pads and faders that play them.
--
-- `perform_patterns` holds the same pattern a score holds (a graph, a target,
-- a blend mode, a seed) without a place on a timeline: each one plays from
-- the moment it is turned on, at its own level, for as long as it is on.
-- `selection_json` is a `Selection`, the group expression it targets.
-- `z_index` is its place in the set: where two patterns write the same
-- attribute of the same light, the higher one wins. `pad_json` is a
-- `luma_patterns::press::Press`: whether its pad toggles or flashes it, and
-- the envelope it fades in and out by.
--
-- `perform_layouts` is one row a venue, its `id` the venue's: the Perform
-- tab's named pages of cells, as a `PerformLayout` in JSON: a cell per
-- pattern, and the master, tap and faders of its own; rows that play one
-- pattern at a time. Nothing in it is tied to a controller; a controller
-- reaches it through bindings.
--
-- Symmetric with supabase/migrations/20261104120000_perform.sql.
CREATE TABLE perform_patterns (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    selection_json TEXT NOT NULL,
    z_index INTEGER NOT NULL DEFAULT 0,
    blend_mode TEXT NOT NULL,
    seed TEXT NOT NULL,
    graph_json TEXT NOT NULL,
    pad_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_perform_patterns_venue ON perform_patterns(venue_id);
CREATE TRIGGER perform_patterns_updated_at AFTER UPDATE ON perform_patterns FOR EACH ROW
BEGIN UPDATE perform_patterns SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TABLE perform_layouts (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    layout_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_perform_layouts_venue ON perform_layouts(venue_id);
CREATE TRIGGER perform_layouts_updated_at AFTER UPDATE ON perform_layouts FOR EACH ROW
BEGIN UPDATE perform_layouts SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
