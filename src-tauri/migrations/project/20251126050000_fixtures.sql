CREATE TABLE IF NOT EXISTS fixtures (
    id TEXT PRIMARY KEY,
    -- DMX Patching
    universe INTEGER NOT NULL DEFAULT 1,
    address INTEGER NOT NULL,
    num_channels INTEGER NOT NULL,
    -- Identity
    manufacturer TEXT NOT NULL,
    model TEXT NOT NULL,
    mode_name TEXT NOT NULL,
    fixture_path TEXT NOT NULL,
    -- Configuration (config_json removed as requested)
    label TEXT,
    -- Spatial
    pos_x REAL DEFAULT 0.0,
    pos_y REAL DEFAULT 0.0,
    pos_z REAL DEFAULT 0.0,
    rot_x REAL DEFAULT 0.0,
    rot_y REAL DEFAULT 0.0,
    rot_z REAL DEFAULT 0.0,
    -- Metadata
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_fixtures_universe ON fixtures(universe);

CREATE TRIGGER IF NOT EXISTS fixtures_updated_at
AFTER UPDATE ON fixtures
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE fixtures
    SET updated_at = CURRENT_TIMESTAMP
    WHERE id = OLD.id;
END;
