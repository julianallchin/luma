-- Stage pieces: placeable 3D set-design objects (floors, trusses, speakers, ...)
-- Co-exist with fixtures in a venue. Phase 1 is flat (no parent hierarchy);
-- Phase 2 will add fixtures.parent_stage_piece_id for proximity-attach.
--
-- Coordinate convention matches fixtures: data is Z-up. The renderer swaps
-- Y <-> Z when mapping to three.js (which is Y-up).
--
-- Sync: not yet registered in src/sync/registry.rs — local-only until the
-- matching Supabase table is created. To enable, add a TableMeta entry with
-- parents = &["venues"] and the columns list below (excluding local-only).

CREATE TABLE stage_pieces (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,

    -- Path to GLB asset, relative to resources/meshes/ (e.g. "stage_lab/truss_q30_box.glb")
    mesh_path TEXT NOT NULL,

    -- Taxonomy used for snap rules and palette grouping
    -- ('floor' | 'truss' | 'speaker' | 'cdj' | 'mixer' | 'guardrail' | 'stand' | 'cable_cover')
    kind TEXT NOT NULL,

    label TEXT,

    pos_x REAL NOT NULL DEFAULT 0.0,
    pos_y REAL NOT NULL DEFAULT 0.0,
    pos_z REAL NOT NULL DEFAULT 0.0,
    rot_x REAL NOT NULL DEFAULT 0.0,
    rot_y REAL NOT NULL DEFAULT 0.0,
    rot_z REAL NOT NULL DEFAULT 0.0,
    scale REAL NOT NULL DEFAULT 1.0,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,

    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

CREATE INDEX idx_stage_pieces_venue ON stage_pieces(venue_id);

CREATE TRIGGER stage_pieces_updated_at AFTER UPDATE ON stage_pieces FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE stage_pieces
        SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
