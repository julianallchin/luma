CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

-- Default settings
INSERT INTO settings (key, value) VALUES ('artnet_enabled', 'false');
INSERT INTO settings (key, value) VALUES ('artnet_interface', '0.0.0.0');
INSERT INTO settings (key, value) VALUES ('artnet_broadcast', 'true');
INSERT INTO settings (key, value) VALUES ('artnet_unicast_ip', '');
INSERT INTO settings (key, value) VALUES ('artnet_net', '0');
INSERT INTO settings (key, value) VALUES ('artnet_subnet', '0');
