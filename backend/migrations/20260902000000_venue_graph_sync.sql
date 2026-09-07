-- What the venue graph needed to become syncable.
--
-- `20260829000000_venue_graph.sql` built the four tables local-only, so only
-- `venue_nodes` has the column set the sync engine writes through
-- (`uid`/`version`/`synced_at`), and none of them has `origin`. The remote
-- counterpart is `supabase/migrations/20260902000000_venue_graph_sync_shape.sql`,
-- deployed before this lands; after it the four are in `sync::registry::TABLES`
-- and fixture placement travels between a user's machines again.
--
-- # Why the three dependent tables are rebuilt rather than altered
--
-- They need `uid`, and their existing rows have to get one — the owner of the
-- node they hang off. That value cannot be written with an `UPDATE`: every
-- venue table carries a `BEFORE UPDATE` admission trigger that aborts unless
-- the *currently* admitted principal owns the venue, and a migration runs
-- against whatever admission the last session left armed. A fresh table has no
-- triggers on it until this file creates them, so `INSERT ... SELECT` carries
-- the owner across where an `UPDATE` would abort. `venue_nodes` already has
-- `uid`, so it only needs `origin` added.
--
-- # Delivery columns, and which trigger owns which
--
-- `updated_at`/`version`/`synced_at` are the dirty flag the push sweep reads:
-- the `_updated_at` trigger bumps the row and clears `synced_at` on a local
-- edit, and skips itself when `version` already moved — which is how a pull
-- upsert (`version = version + 1` in its `ON CONFLICT`) writes a clean row
-- without re-dirtying it. `origin` is what the `sync_delete_*` triggers read:
-- a row that arrived from someone else must not push a tombstone back.
--
-- `venue_node_params` and `venue_constraints` have composite primary keys, so
-- their tombstones join the key values with `char(31)` — the encoding
-- `registry::RECORD_ID_SEPARATOR` names and `decode_record_id` splits. A
-- printable separator cannot serve: a venue's root node is named
-- `'<venue_id>:venue'`, so `node_id` already contains a colon.
--
-- Soft deletes do not cascade: a remote delete is a `deleted_at` PATCH, not a
-- `DELETE`, so the remote `ON DELETE CASCADE` never fires for one. Each of the
-- four therefore carries its own delete trigger, which is also why
-- `local::venue_graph::delete_nodes` deletes a node's belongings explicitly.

ALTER TABLE venue_nodes ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';

-- -----------------------------------------------------------------------------
-- venue_edges
-- -----------------------------------------------------------------------------

CREATE TABLE venue_edges_synced (
    child_id TEXT PRIMARY KEY,
    uid TEXT,
    parent_id TEXT NOT NULL,
    my_socket TEXT NOT NULL,
    their_socket TEXT NOT NULL,
    roll REAL NOT NULL DEFAULT 0.0,

    origin TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,

    CHECK (child_id <> parent_id),
    FOREIGN KEY (child_id) REFERENCES venue_nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES venue_nodes(id) ON DELETE CASCADE
);

INSERT INTO venue_edges_synced (child_id, uid, parent_id, my_socket, their_socket, roll)
SELECT edge.child_id, node.uid, edge.parent_id, edge.my_socket, edge.their_socket, edge.roll
FROM venue_edges AS edge
JOIN venue_nodes AS node ON node.id = edge.child_id;

DROP TABLE venue_edges;
ALTER TABLE venue_edges_synced RENAME TO venue_edges;

CREATE INDEX idx_venue_edges_parent ON venue_edges(parent_id);

CREATE TRIGGER venue_edges_updated_at AFTER UPDATE ON venue_edges FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venue_edges
        SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
            version = OLD.version + 1,
            synced_at = NULL
        WHERE child_id = OLD.child_id;
    END;

-- -----------------------------------------------------------------------------
-- venue_node_params
-- -----------------------------------------------------------------------------

CREATE TABLE venue_node_params_synced (
    node_id TEXT NOT NULL,
    uid TEXT,
    key TEXT NOT NULL,
    value REAL NOT NULL,

    origin TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,

    PRIMARY KEY (node_id, key),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE
);

INSERT INTO venue_node_params_synced (node_id, uid, key, value)
SELECT param.node_id, node.uid, param.key, param.value
FROM venue_node_params AS param
JOIN venue_nodes AS node ON node.id = param.node_id;

DROP TABLE venue_node_params;
ALTER TABLE venue_node_params_synced RENAME TO venue_node_params;

CREATE TRIGGER venue_node_params_updated_at AFTER UPDATE ON venue_node_params FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venue_node_params
        SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
            version = OLD.version + 1,
            synced_at = NULL
        WHERE node_id = OLD.node_id AND key = OLD.key;
    END;

-- -----------------------------------------------------------------------------
-- venue_constraints
-- -----------------------------------------------------------------------------

CREATE TABLE venue_constraints_synced (
    node_id TEXT NOT NULL,
    uid TEXT,
    my_socket TEXT NOT NULL,
    target_node TEXT NOT NULL,
    target_socket TEXT NOT NULL,

    origin TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,

    PRIMARY KEY (node_id, my_socket),
    FOREIGN KEY (node_id) REFERENCES venue_nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (target_node) REFERENCES venue_nodes(id) ON DELETE CASCADE
);

INSERT INTO venue_constraints_synced (node_id, uid, my_socket, target_node, target_socket)
SELECT c.node_id, node.uid, c.my_socket, c.target_node, c.target_socket
FROM venue_constraints AS c
JOIN venue_nodes AS node ON node.id = c.node_id;

DROP TABLE venue_constraints;
ALTER TABLE venue_constraints_synced RENAME TO venue_constraints;

CREATE INDEX idx_venue_constraints_target ON venue_constraints(target_node);

CREATE TRIGGER venue_constraints_updated_at AFTER UPDATE ON venue_constraints FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venue_constraints
        SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
            version = OLD.version + 1,
            synced_at = NULL
        WHERE node_id = OLD.node_id AND my_socket = OLD.my_socket;
    END;

-- -----------------------------------------------------------------------------
-- Write admission, rebuilt verbatim from `20260829000000_venue_graph.sql`
--
-- `DROP TABLE` takes a table's triggers with it. These three tables are the
-- same aggregate they always were: owners write, joined members are read-only,
-- remote pull and transaction-local maintenance are the two explicit bypasses.
-- -----------------------------------------------------------------------------

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

-- -----------------------------------------------------------------------------
-- Outgoing tombstones
-- -----------------------------------------------------------------------------

CREATE TRIGGER sync_delete_venue_nodes AFTER DELETE ON venue_nodes FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, conflict_key, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'venue_nodes', OLD.id, 'id', CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

CREATE TRIGGER sync_delete_venue_edges AFTER DELETE ON venue_edges FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, conflict_key, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'venue_edges', OLD.child_id, 'child_id', CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

CREATE TRIGGER sync_delete_venue_node_params AFTER DELETE ON venue_node_params FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, conflict_key, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'venue_node_params', OLD.node_id || char(31) || OLD.key, 'node_id,key', CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

CREATE TRIGGER sync_delete_venue_constraints AFTER DELETE ON venue_constraints FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, conflict_key, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'venue_constraints', OLD.node_id || char(31) || OLD.my_socket, 'node_id,my_socket', CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

-- Composite record ids were joined with ':' before `registry::
-- RECORD_ID_SEPARATOR`, and a stale tombstone under the old encoding now
-- decodes to the wrong arity and can never be delivered. Upserts do not need
-- the same treatment: the dirty sweep re-enqueues them under the new encoding.
DELETE FROM pending_ops
WHERE op_type = 'delete'
  AND instr(record_id, char(31)) = 0
  AND table_name IN (
      'track_stems', 'authored_revision_files', 'authored_revision_parents',
      'authored_operation_outcomes', 'agent_thread_message_appends',
      'authored_turn_preparations', 'authored_turn_outcomes'
  );
