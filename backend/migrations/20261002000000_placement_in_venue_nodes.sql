-- A node's placement moves onto its row, and `venue_edges` goes.
--
-- A light must never exist without a place in the room. With the placement in
-- a second table, "a node with no edge" was a state the schema allowed, and
-- the app grew a whole "unplaced" concept around it: a tray, a detach verb, a
-- group class, an address rule. Folding the four placement columns into
-- `venue_nodes` makes that state unrepresentable: every node but the root has
-- a parent, enforced by a CHECK, and deleting a parent deletes its subtree.
--
-- Data: every node the root cannot reach is deleted, with its subtree, its
-- params, its constraints and, for a light, its patch row and group
-- memberships. So is every patch row in a converted venue (one with a root)
-- that has no node at all. A venue with no root has not been converted off the
-- old schema yet; `venue_graph::migrate` places its fixtures when it opens.
--
-- Migrations run with foreign keys off, so nothing below cascades: every
-- dependant is deleted by name.
--
-- Symmetric with supabase/migrations/20261002000000_placement_in_venue_nodes.sql.

CREATE TEMP TABLE doomed_nodes AS
WITH RECURSIVE placed(id) AS (
    SELECT id FROM venue_nodes WHERE kind = 'venue'
    UNION
    SELECT edge.child_id
    FROM venue_edges edge
    JOIN placed ON edge.parent_id = placed.id
)
SELECT id FROM venue_nodes WHERE id NOT IN (SELECT id FROM placed);

CREATE TEMP TABLE doomed_fixtures AS
SELECT fixture.id FROM fixtures fixture
WHERE fixture.id IN (SELECT id FROM doomed_nodes)
   OR (NOT EXISTS (SELECT 1 FROM venue_nodes node WHERE node.id = fixture.id)
       AND EXISTS (SELECT 1 FROM venue_nodes root
                   WHERE root.venue_id = fixture.venue_id AND root.kind = 'venue'));

DELETE FROM fixture_group_members WHERE fixture_id IN (SELECT id FROM doomed_fixtures);
DELETE FROM fixtures WHERE id IN (SELECT id FROM doomed_fixtures);
DELETE FROM venue_node_params WHERE node_id IN (SELECT id FROM doomed_nodes);
DELETE FROM venue_constraints
WHERE node_id IN (SELECT id FROM doomed_nodes)
   OR target_node IN (SELECT id FROM doomed_nodes);
DELETE FROM venue_edges
WHERE child_id IN (SELECT id FROM doomed_nodes)
   OR parent_id IN (SELECT id FROM doomed_nodes);
DELETE FROM venue_nodes WHERE id IN (SELECT id FROM doomed_nodes);

DROP TABLE doomed_nodes;
DROP TABLE doomed_fixtures;

PRAGMA legacy_alter_table = ON;

CREATE TABLE "venue_nodes__placed" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,

    -- 'venue' (root) | 'stage' | 'run' | 'tower' | 'piece' | 'fixture' | 'array'.
    -- The closed alphabet of `luma_scene::venue::NodeKind`; a new set object is
    -- a 'piece' with sockets, never a new kind.
    kind TEXT NOT NULL CHECK (kind IN ('venue', 'stage', 'run', 'tower', 'piece', 'fixture', 'array')),

    -- What geometry this node has: a catalog piece id ('truss/straight',
    -- 'stage_lab/….glb') for structure, a fixture's bundle path for a light.
    -- NULL on the root, which is the venue frame and has no geometry.
    catalog_ref TEXT,

    label TEXT,

    -- Where it hangs: the parent's socket it mates, by which of its own, and
    -- its turn about the shared normal. NULL on the root and only there.
    parent_id TEXT,
    my_socket TEXT,
    their_socket TEXT,
    roll REAL,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),

    CHECK (CASE WHEN kind = 'venue'
                THEN parent_id IS NULL AND my_socket IS NULL
                     AND their_socket IS NULL AND roll IS NULL
                ELSE parent_id IS NOT NULL AND my_socket IS NOT NULL
                     AND their_socket IS NOT NULL AND roll IS NOT NULL
           END),
    CHECK (parent_id <> id),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (parent_id) REFERENCES venue_nodes(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);

INSERT INTO "venue_nodes__placed"
    (id, uid, venue_id, kind, catalog_ref, label,
     parent_id, my_socket, their_socket, roll, created_at, updated_at)
SELECT node.id, node.uid, node.venue_id, node.kind, node.catalog_ref, node.label,
       edge.parent_id, edge.my_socket, edge.their_socket, edge.roll,
       node.created_at, node.updated_at
FROM venue_nodes node
LEFT JOIN venue_edges edge ON edge.child_id = node.id;

DROP TABLE venue_edges;
DROP TABLE venue_nodes;
ALTER TABLE "venue_nodes__placed" RENAME TO venue_nodes;

PRAGMA legacy_alter_table = OFF;

CREATE INDEX idx_venue_nodes_venue ON venue_nodes(venue_id);
CREATE INDEX idx_venue_nodes_parent ON venue_nodes(parent_id);
CREATE UNIQUE INDEX idx_venue_nodes_root ON venue_nodes(venue_id) WHERE kind = 'venue';
CREATE TRIGGER venue_nodes_updated_at AFTER UPDATE ON venue_nodes FOR EACH ROW
BEGIN UPDATE venue_nodes SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
