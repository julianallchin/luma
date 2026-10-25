-- Lasers (docs/design/lasers.md): a venue's zones, and each projector's
-- optional limit.
--
-- A zone is a named window of directions in the room that laser clips draw
-- in: `turn` 0° toward the house, positive toward stage right; `tilt` 0°
-- level, positive up. Every projector shares it, so the canvas's middle is
-- the same direction from each of them.
--
-- A limit is a polygon in one projector's own view, galvo coordinates −1 to
-- 1, outside which its light is blanked. `id` is the projector fixture's id.
--
-- Symmetric with supabase/migrations/20261024120000_lasers.sql.
CREATE TABLE laser_zones (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL,
    turn_min REAL NOT NULL,
    turn_max REAL NOT NULL,
    tilt_min REAL NOT NULL,
    tilt_max REAL NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_laser_zones_venue ON laser_zones(venue_id);
CREATE TRIGGER laser_zones_updated_at AFTER UPDATE ON laser_zones FOR EACH ROW
BEGIN UPDATE laser_zones SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TABLE laser_limits (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    points TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (id) REFERENCES fixtures(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_laser_limits_venue ON laser_limits(venue_id);
CREATE TRIGGER laser_limits_updated_at AFTER UPDATE ON laser_limits FOR EACH ROW
BEGIN UPDATE laser_limits SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
