-- The venue graph: a tree of relations, replacing the bag of world poses.
--
-- `docs/design/venue-graph.md`. Every node stores (parent, my_socket,
-- their_socket, roll, params) and *no* pose; poses are derived by walking the
-- tree through `luma_scene::venue::resolve` on load and after every edit.
--
-- The rows in `stage_pieces` and the pose columns in `fixtures` are converted
-- by one solve-and-invert pass in `crate::venue_graph::migrate`, which runs the
-- first time a venue is read and cannot live here: inverting a pose needs the
-- catalog, the GLB bounding boxes and the truss generator, none of which SQLite
-- has. Neither source is dropped by this migration for the same reason — the
-- pass has to still find them — and `fixtures.pos_*`/`rot_*` additionally sync
-- to Supabase, so dropping them is a remote schema change with its own deploy
-- ordering. They are left in place and **unread**; a later migration drops them
-- once every machine has run the pass.
--
-- Sync: local-only, exactly like `stage_pieces` was. The Supabase counterpart
-- is `supabase/migrations/20260829000000_venue_graph.sql`, which is NOT
-- deployed; registering these tables in `src/sync/registry.rs` before it is
-- would break every client's push.

CREATE TABLE venue_nodes (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,

    -- 'venue' (root) | 'stage' | 'run' | 'tower' | 'piece' | 'fixture' | 'array'.
    -- The closed alphabet of `luma_scene::venue::NodeKind`; a new set object is
    -- a 'piece' with sockets, never a new kind.
    kind TEXT NOT NULL CHECK (kind IN ('venue', 'stage', 'run', 'tower', 'piece', 'fixture', 'array')),

    -- What geometry this node has: a catalog piece id ('truss/straight',
    -- 'stage_lab/…​.glb') for structure, a `fixtures` row id for a fixture
    -- node. NULL on the root, which is the venue frame and has no geometry.
    catalog_ref TEXT,

    label TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,

    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

CREATE INDEX idx_venue_nodes_venue ON venue_nodes(venue_id);

-- Exactly one root per venue, as an index rather than a check: the resolver
-- starts from it, and two would make "the venue frame" ambiguous.
CREATE UNIQUE INDEX idx_venue_nodes_root ON venue_nodes(venue_id) WHERE kind = 'venue';

CREATE TRIGGER venue_nodes_updated_at AFTER UPDATE ON venue_nodes FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venue_nodes
        SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
            version = OLD.version + 1,
            synced_at = NULL
        WHERE id = OLD.id;
    END;

-- Keyed by `child_id`, so "exactly one parent per node" is a primary key
-- rather than a check nobody runs.
CREATE TABLE venue_edges (
    child_id TEXT PRIMARY KEY,
    parent_id TEXT NOT NULL,

    -- The socket on the child, and the one on the parent it mates. Names, not
    -- ids: a socket belongs to a catalog entry or a generator's face set, and
    -- both name them.
    my_socket TEXT NOT NULL,
    their_socket TEXT NOT NULL,

    -- The mate's turn about the shared normal, radians. On a surface placement
    -- this is what the stage vocabulary calls *yaw*. Clamped at solve by the
    -- host socket's roll freedom, so a bolted joint stores 0 whatever is
    -- written here.
    roll REAL NOT NULL DEFAULT 0.0,

    CHECK (child_id <> parent_id),
    FOREIGN KEY (child_id) REFERENCES venue_nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES venue_nodes(id) ON DELETE CASCADE
);

CREATE INDEX idx_venue_edges_parent ON venue_edges(parent_id);

-- u, v, trim on a surface placement; span, angle, faces on a generated piece;
-- count, span on an array; pan, tilt on a fixture. Untyped on purpose: a column
-- per key would be a second declaration of a list that already lives in
-- `luma_scene::venue`.
CREATE TABLE venue_node_params (
    node_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value REAL NOT NULL,

    PRIMARY KEY (node_id, key),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE
);

-- Far-end checks. A *separate* table on purpose: a bridging piece has one
-- parent and one far end, and the far end is evaluated after the solve —
-- satisfied / violated / dangling — never participating in it. Making it an
-- edge would give a node two parents.
CREATE TABLE venue_constraints (
    node_id TEXT NOT NULL,
    my_socket TEXT NOT NULL,
    target_node TEXT NOT NULL,
    target_socket TEXT NOT NULL,

    PRIMARY KEY (node_id, my_socket),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (target_node) REFERENCES venue_nodes(id) ON DELETE CASCADE
);

CREATE INDEX idx_venue_constraints_target ON venue_constraints(target_node);

-- Write admission. Same aggregate as every other venue-owned table: owners
-- write, joined members are read-only, remote pull and transaction-local
-- maintenance are the two explicit bypasses (`20260802600000`).
--
-- Only `venue_nodes` carries a `venue_id`; the other three reach it through the
-- node they belong to, which is also what makes a row that names a node in
-- another venue impossible.

CREATE TRIGGER auth_venue_node_insert
BEFORE INSERT ON venue_nodes FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.uid IS NOT (SELECT active_uid FROM auth_write_admission WHERE singleton = 1)
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
 )
BEGIN SELECT RAISE(ABORT, 'venue node write is not authorized'); END;

CREATE TRIGGER auth_venue_node_update
BEFORE UPDATE ON venue_nodes FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.id IS NOT OLD.id OR NEW.uid IS NOT OLD.uid OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
 )
BEGIN SELECT RAISE(ABORT, 'venue node write is not authorized'); END;

CREATE TRIGGER auth_venue_node_delete
BEFORE DELETE ON venue_nodes FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'venue node write is not authorized'); END;

CREATE TRIGGER auth_venue_edge_insert
BEFORE INSERT ON venue_edges FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS child
      JOIN venue_nodes AS parent ON parent.id = NEW.parent_id
      JOIN auth_venue_access AS access ON access.venue_id = child.venue_id
      WHERE child.id = NEW.child_id
        AND parent.venue_id = child.venue_id
        AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue edge write is not authorized'); END;

CREATE TRIGGER auth_venue_edge_update
BEFORE UPDATE ON venue_edges FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS child
      JOIN venue_nodes AS parent ON parent.id = NEW.parent_id
      JOIN auth_venue_access AS access ON access.venue_id = child.venue_id
      WHERE child.id = NEW.child_id
        AND parent.venue_id = child.venue_id
        AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue edge write is not authorized'); END;

CREATE TRIGGER auth_venue_edge_delete
BEFORE DELETE ON venue_edges FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS child
      JOIN auth_venue_access AS access ON access.venue_id = child.venue_id
      WHERE child.id = OLD.child_id AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue edge write is not authorized'); END;

CREATE TRIGGER auth_venue_param_insert
BEFORE INSERT ON venue_node_params FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS node
      JOIN auth_venue_access AS access ON access.venue_id = node.venue_id
      WHERE node.id = NEW.node_id AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue node param write is not authorized'); END;

CREATE TRIGGER auth_venue_param_update
BEFORE UPDATE ON venue_node_params FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS node
      JOIN auth_venue_access AS access ON access.venue_id = node.venue_id
      WHERE node.id = NEW.node_id AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue node param write is not authorized'); END;

CREATE TRIGGER auth_venue_param_delete
BEFORE DELETE ON venue_node_params FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS node
      JOIN auth_venue_access AS access ON access.venue_id = node.venue_id
      WHERE node.id = OLD.node_id AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue node param write is not authorized'); END;

CREATE TRIGGER auth_venue_constraint_insert
BEFORE INSERT ON venue_constraints FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS node
      JOIN venue_nodes AS target ON target.id = NEW.target_node
      JOIN auth_venue_access AS access ON access.venue_id = node.venue_id
      WHERE node.id = NEW.node_id
        AND target.venue_id = node.venue_id
        AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue constraint write is not authorized'); END;

CREATE TRIGGER auth_venue_constraint_update
BEFORE UPDATE ON venue_constraints FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS node
      JOIN venue_nodes AS target ON target.id = NEW.target_node
      JOIN auth_venue_access AS access ON access.venue_id = node.venue_id
      WHERE node.id = NEW.node_id
        AND target.venue_id = node.venue_id
        AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue constraint write is not authorized'); END;

CREATE TRIGGER auth_venue_constraint_delete
BEFORE DELETE ON venue_constraints FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (
      SELECT 1
      FROM venue_nodes AS node
      JOIN auth_venue_access AS access ON access.venue_id = node.venue_id
      WHERE node.id = OLD.node_id AND access.owner_access = 1
 )
BEGIN SELECT RAISE(ABORT, 'venue constraint write is not authorized'); END;
