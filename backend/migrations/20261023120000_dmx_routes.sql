-- Output routing stops being Art-Net's table.
--
-- `universe_outputs` named a node by IP and held one destination per
-- universe. `dmx_routes` names any device — an Art-Net node, a USB dongle — by
-- a stable id, and a universe may go to several. A universe with no row is
-- sent nowhere: the net/subnet fallback that aliased universe 17 onto 1 is
-- gone, and with it the settings that only fed it.
--
-- Local-only, like the table it replaces: which box is plugged in where is a
-- property of the room, not of the venue document.
CREATE TABLE dmx_routes (
    universe INTEGER NOT NULL CHECK (universe >= 0),
    device_kind TEXT NOT NULL CHECK (device_kind IN ('art-net', 'usb')),
    device_id TEXT NOT NULL,
    -- Art-Net: the node's own 15-bit port address. USB: the output's index.
    port INTEGER NOT NULL CHECK (port BETWEEN 0 AND 32767),
    device_name TEXT,
    -- Art-Net: the `ip:port` to send to until the node answers a poll.
    address TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (universe, device_kind, device_id, port)
);

-- An old binding named its node by IP, so the IP becomes the id. The driver
-- matches a node by MAC or IP, and the next bind from the page stores the MAC.
INSERT INTO dmx_routes (universe, device_kind, device_id, port, device_name, address, created_at, updated_at)
SELECT universe, 'art-net', node_ip, port_address, node_name,
       node_ip || ':' || node_port, created_at, updated_at
FROM universe_outputs;

DROP TABLE universe_outputs;

UPDATE settings SET key = 'dmx_output_enabled' WHERE key = 'artnet_enabled';
DELETE FROM settings
WHERE key IN ('artnet_broadcast', 'artnet_unicast_ip', 'artnet_net', 'artnet_subnet');
