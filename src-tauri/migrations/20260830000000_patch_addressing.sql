-- Addressing: the pin, the range invariant, and the output table.
--
-- `docs/design/venue-graph.md` ("Two pages"), `docs/specs/venue-builder-gauntlet.md` §3.
-- Addresses stop being typed and start being *derived* from where a fixture
-- hangs (`luma_scene::patch`). Three things have to be true in the database for
-- that to work, and none of them can live in Rust alone.

-- 1. Which addresses are a human's decision.
--
-- Auto-patch re-derives every fixture *except* the ones somebody set by hand.
-- A column on `fixtures` rather than a `venue_node_params` row: the design doc
-- splits the fixture row into patch and placement, and this is patch — a
-- fixture in the tray has an address and a pin but no node to hang a param on.
--
-- Deliberately absent from `src/sync/registry.rs`: the remote `fixtures` table
-- has no such column, and a pull that carried one would clear the local pin
-- every time somebody else pushed the row.
ALTER TABLE fixtures ADD COLUMN address_pinned INTEGER NOT NULL DEFAULT 0;

-- 2. An address in this table is always addressable.
--
-- `fixtures/engine.rs` used to drop any channel that landed past 512, so a
-- fixture patched at 500 with 16 channels went half-dark with no error
-- anywhere. Refusing the write is the only fix that makes the truncation
-- branch unreachable rather than merely rare — and it belongs here, not in the
-- handler, because the handler is not the only writer (sync pull is another).
--
-- Existing rows are repaired first: this is data written before anything
-- validated, and there is no address it could be moved to that is more right
-- than the start of its universe. Auto-patch is what puts it somewhere real.
UPDATE fixtures SET num_channels = 1 WHERE num_channels < 1;
UPDATE fixtures SET address = 1 WHERE address < 1 OR address + num_channels - 1 > 512;

CREATE TRIGGER fixtures_address_fits_universe_insert
BEFORE INSERT ON fixtures FOR EACH ROW
WHEN NEW.address < 1 OR NEW.num_channels < 1 OR NEW.address + NEW.num_channels - 1 > 512
BEGIN SELECT RAISE(ABORT, 'fixture footprint leaves its universe'); END;

CREATE TRIGGER fixtures_address_fits_universe_update
BEFORE UPDATE ON fixtures FOR EACH ROW
WHEN NEW.address < 1 OR NEW.num_channels < 1 OR NEW.address + NEW.num_channels - 1 > 512
BEGIN SELECT RAISE(ABORT, 'fixture footprint leaves its universe'); END;

-- 3. Where a universe goes on the wire.
--
-- Art-Net output used to derive its port address arithmetically —
-- `(net << 8) | (subnet << 4) | (universe & 0xF)` — which aliases universe 17
-- onto universe 1 and cannot express two nodes on one network at all. A
-- universe binds to a node instead: an address, a port, and the port address
-- that node announced for itself.
--
-- App-global, like the Art-Net settings it replaces half of, and local-only:
-- which box is plugged in where is a property of the room the app is running
-- in, not of the venue document.
CREATE TABLE universe_outputs (
    universe INTEGER PRIMARY KEY CHECK (universe >= 0),

    -- Where the packets go. The port is separate from the port address on
    -- purpose: one is UDP, the other is Art-Net's 15-bit Net/SubNet/Universe
    -- triple, and conflating them is the bug above.
    node_ip TEXT NOT NULL,
    node_port INTEGER NOT NULL DEFAULT 6454,
    port_address INTEGER NOT NULL CHECK (port_address BETWEEN 0 AND 32767),

    -- What the node called itself when it was discovered, so an unplugged
    -- binding can still be shown by name rather than by IP.
    node_name TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
