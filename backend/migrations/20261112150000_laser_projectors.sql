-- Laser projectors as what they are (docs/design/lasers.md). The rows are
-- converted on the server (supabase/migrations/20261112150000_laser_projectors.sql,
-- data format 3); this is the schema that holds them.

-- A projector is driven through a DAC, not over DMX: it has no channels and
-- takes no address. Its row's address is 1 and means nothing.
DROP TRIGGER fixtures_address_fits_universe_insert;
DROP TRIGGER fixtures_address_fits_universe_update;

CREATE TRIGGER fixtures_address_fits_universe_insert
BEFORE INSERT ON fixtures FOR EACH ROW
WHEN NEW.address < 1 OR NEW.num_channels < 0 OR NEW.address + NEW.num_channels - 1 > 512
BEGIN SELECT RAISE(ABORT, 'fixture footprint leaves its universe'); END;

CREATE TRIGGER fixtures_address_fits_universe_update
BEFORE UPDATE ON fixtures FOR EACH ROW
WHEN NEW.address < 1 OR NEW.num_channels < 0 OR NEW.address + NEW.num_channels - 1 > 512
BEGIN SELECT RAISE(ABORT, 'fixture footprint leaves its universe'); END;

-- A venue has one laser window, held as parameters of its root node.
DROP TABLE laser_zones;

-- Which DAC plays each projector. This machine's wiring, like `dmx_routes`:
-- never synced.
CREATE TABLE laser_routes (
    fixture_id TEXT PRIMARY KEY NOT NULL,
    device_kind TEXT NOT NULL CHECK (device_kind IN ('ether-dream')),
    -- Ether Dream: the MAC address it broadcasts.
    device_id TEXT NOT NULL,
    device_name TEXT,
    -- The `ip:port` to connect to until the DAC is heard on the network.
    address TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
