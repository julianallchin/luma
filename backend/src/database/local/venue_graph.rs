//! The three venue-graph tables.
//!
//! Rows in, rows out. Nothing here knows what a socket is or where a piece
//! ends up — that is [`crate::venue_graph`]'s business, and the split is what
//! keeps the resolver testable without a database and this module testable
//! without a GLB.
//!
//! One exception, deliberate: every write here tells the derived-group cache
//! that the rig moved. The group tree is *derived from this graph*, and the
//! cache that holds it is a read cache over these rows. Making each of the
//! eight callers remember to invalidate is how six of them came to forget —
//! moving a truss left every selection expression naming a derived group
//! answering with the old split. The one place that can promise the cache is
//! not stale is the place the rows change.
//!
//! Twice, though: see [`graph_changed`] and [`graph_committed`].

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::database::local::deletes;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::venue_graph::{NodePlacement, VenueConstraint, VenueGraphRows, VenueNode};

/// Tell the derived-group cache the rig moved.
///
/// Called from every write in this module. Early — before the transaction
/// commits — because the cache is a pure read cache: dropping it when a write
/// might still roll back costs one reload, and keeping it when a write lands
/// costs a wrong answer.
///
/// Early is not enough on its own; see [`graph_committed`].
fn graph_changed() {
    crate::services::groups::invalidate_venue_fixture_cache();
}

/// Tell it again, once the rows are visible.
///
/// Dropping the cache inside the transaction closes the window *before* the
/// write and opens one after it: a reader arriving between the last write and
/// the commit sees the old rows, cannot see the new ones, and repopulates the
/// cache with an answer that is already wrong by the time it lands. The second
/// drop is the one that makes "the tree is derived from these rows" true again.
///
/// Called by whoever owns the transaction, because that is who knows when it
/// committed — this module never commits.
pub fn graph_committed() {
    crate::services::groups::invalidate_venue_fixture_cache();
}

/// The three tables this module owns.
///
/// Sync is the one writer that does not come through the functions below: it
/// upserts and deletes graph rows straight from the registry, by name. This is
/// how it says the same thing they say.
pub const TABLES: &[&str] = &["venue_nodes", "venue_node_params", "venue_constraints"];

/// One `venue_nodes` row as SQLite hands it over: the placement as four
/// nullable columns, which the schema's CHECK keeps all-or-nothing.
#[derive(sqlx::FromRow)]
struct NodeRow {
    id: String,
    venue_id: String,
    kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
    parent_id: Option<String>,
    my_socket: Option<String>,
    their_socket: Option<String>,
    roll: Option<f64>,
}

impl From<NodeRow> for VenueNode {
    fn from(row: NodeRow) -> Self {
        let placement = match (row.parent_id, row.my_socket, row.their_socket, row.roll) {
            (Some(parent), Some(my_socket), Some(their_socket), Some(roll)) => {
                Some(NodePlacement {
                    parent,
                    my_socket,
                    their_socket,
                    roll,
                })
            }
            _ => None,
        };
        VenueNode {
            id: row.id,
            venue_id: row.venue_id,
            kind: row.kind,
            catalog_ref: row.catalog_ref,
            label: row.label,
            placement,
        }
    }
}

// -----------------------------------------------------------------------------
// Reads
// -----------------------------------------------------------------------------

/// Every row of one venue's graph, in id order.
///
/// # Errors
/// Fails if any of the three tables cannot be read.
pub async fn get_graph(access: &mut impl AuthorizedVenue) -> Result<VenueGraphRows, String> {
    let venue_id = access.venue_id().to_string();

    // `catalog_ref` names a node's geometry, and for a light that is its
    // bundle path. Venues written before that was true stored the patch-row id
    // — which the node id already is — so the path is read back off the patch
    // here rather than repaired in place: `venue_nodes` is a table whose
    // triggers refuse a migration-time write, and a row that is rewritten the
    // next time it is saved needs no repair anyway.
    let nodes = sqlx::query_as::<_, NodeRow>(
        "SELECT node.id, node.venue_id, node.kind,
                COALESCE(fixture.fixture_path, node.catalog_ref) AS catalog_ref,
                node.label, node.parent_id, node.my_socket, node.their_socket, node.roll
         FROM venue_nodes node
         LEFT JOIN fixtures fixture
                ON fixture.id = node.id AND node.kind = 'fixture'
         WHERE node.venue_id = ? ORDER BY node.id ASC",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to read venue nodes: {e}"))?
    .into_iter()
    .map(VenueNode::from)
    .collect();

    let param_rows: Vec<(String, String, f64)> = sqlx::query_as(
        "SELECT param.node_id, param.key, param.value
         FROM venue_node_params param
         JOIN venue_nodes node ON node.id = param.node_id
         WHERE node.venue_id = ? ORDER BY param.node_id ASC, param.key ASC",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to read venue node params: {e}"))?;

    let constraints = sqlx::query_as::<_, VenueConstraint>(
        "SELECT c.node_id, c.my_socket, c.target_node, c.target_socket
         FROM venue_constraints c
         JOIN venue_nodes node ON node.id = c.node_id
         WHERE node.venue_id = ? ORDER BY c.node_id ASC, c.my_socket ASC",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to read venue constraints: {e}"))?;

    let mut params: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for (node_id, key, value) in param_rows {
        params.entry(node_id).or_default().insert(key, value);
    }

    Ok(VenueGraphRows {
        nodes,
        params,
        constraints,
    })
}

/// The venue's root node id, or `None` if the graph has not been built yet.
///
/// Its absence is what [`crate::venue_graph::migrate`] uses as the "this venue
/// has not been converted" marker — a marker column would be a second fact
/// about the same thing.
///
/// # Errors
/// Fails if `venue_nodes` cannot be read.
pub async fn root_id(access: &mut impl AuthorizedVenue) -> Result<Option<String>, String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query_scalar("SELECT id FROM venue_nodes WHERE venue_id = ? AND kind = 'venue'")
        .bind(venue_id)
        .fetch_optional(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to read the venue root: {e}"))
}

// -----------------------------------------------------------------------------
// Writes
// -----------------------------------------------------------------------------

/// Insert a node where it hangs, and return its generated id.
///
/// The placement is part of the row: a node is never written without one,
/// and the schema refuses one that tries.
///
/// # Errors
/// Fails if the insert is refused.
pub async fn insert_node(
    access: &mut VenueAccess<'_, Write>,
    kind: &str,
    catalog_ref: Option<&str>,
    label: Option<&str>,
    placement: &NodePlacement,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();
    insert_node_with_id(access, &id, kind, catalog_ref, label, Some(placement)).await?;
    Ok(id)
}

/// Insert a node under an id the caller chose: the root, a fixture (whose
/// node id is its patch-row id), a restored row, or the conversion pass,
/// which reuses the old row's id so that a group membership or a saved
/// selection naming a stage piece still names the same thing.
///
/// `placement` is `None` for the root and only for the root.
///
/// # Errors
/// As [`insert_node`].
pub async fn insert_node_with_id(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    kind: &str,
    catalog_ref: Option<&str>,
    label: Option<&str>,
    placement: Option<&NodePlacement>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    let principal = access.principal().map(str::to_owned);
    sqlx::query(
        "INSERT INTO venue_nodes
             (id, uid, venue_id, kind, catalog_ref, label,
              parent_id, my_socket, their_socket, roll)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(principal)
    .bind(venue_id)
    .bind(kind)
    .bind(catalog_ref)
    .bind(label)
    .bind(placement.map(|p| p.parent.as_str()))
    .bind(placement.map(|p| p.my_socket.as_str()))
    .bind(placement.map(|p| p.their_socket.as_str()))
    .bind(placement.map(|p| p.roll))
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to insert venue node: {e}"))?;
    graph_changed();
    Ok(())
}

/// Restore the metadata and placement of an existing node without deleting
/// its relations.
pub async fn update_node(
    access: &mut VenueAccess<'_, Write>,
    node: &VenueNode,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    let placement = node.placement.as_ref();
    sqlx::query(
        "UPDATE venue_nodes
         SET kind = ?, catalog_ref = ?, label = ?,
             parent_id = ?, my_socket = ?, their_socket = ?, roll = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(&node.kind)
    .bind(&node.catalog_ref)
    .bind(&node.label)
    .bind(placement.map(|p| p.parent.as_str()))
    .bind(placement.map(|p| p.my_socket.as_str()))
    .bind(placement.map(|p| p.their_socket.as_str()))
    .bind(placement.map(|p| p.roll))
    .bind(&node.id)
    .bind(venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to restore venue node: {e}"))?;
    graph_changed();
    Ok(())
}

/// Remove the far-end check on one socket.
pub async fn delete_constraint(
    access: &mut VenueAccess<'_, Write>,
    node_id: &str,
    my_socket: &str,
) -> Result<(), String> {
    deletes::delete_where(
        access.connection(),
        "venue_constraints",
        "node_id = ? AND my_socket = ?",
        &[node_id, my_socket],
    )
    .await
    .map_err(|e| format!("Failed to remove venue constraint: {e}"))?;
    graph_changed();
    Ok(())
}

/// Move a node to a new place. One statement: a node has exactly one
/// placement, and there is no moment between two writes where it has none.
///
/// # Errors
/// Fails if the update is refused.
pub async fn set_placement(
    access: &mut VenueAccess<'_, Write>,
    node_id: &str,
    placement: &NodePlacement,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_owned();
    sqlx::query(
        "UPDATE venue_nodes SET parent_id = ?, my_socket = ?, their_socket = ?, roll = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(&placement.parent)
    .bind(&placement.my_socket)
    .bind(&placement.their_socket)
    .bind(placement.roll)
    .bind(node_id)
    .bind(venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to place venue node: {e}"))?;
    graph_changed();
    Ok(())
}

/// Record a far end, replacing whatever check that socket already carried.
///
/// One statement, because `(node_id, my_socket)` is the primary key: a socket
/// has one far end or none, and an insert-then-delete would be two states that
/// is false in.
///
/// Admission is [`luma_scene::venue::VenueGraph::constrain`]'s, upstream of
/// here, exactly as it is for [`set_placement`]: this layer writes rows the
/// caller has already had admitted against the solved graph.
///
/// # Errors
/// Fails if the upsert is refused.
pub async fn upsert_constraint(
    access: &mut VenueAccess<'_, Write>,
    node_id: &str,
    my_socket: &str,
    target_node: &str,
    target_socket: &str,
) -> Result<(), String> {
    let principal = access.principal().map(str::to_owned);
    let venue_id = access.venue_id().to_owned();
    sqlx::query(
        "INSERT INTO venue_constraints
             (node_id, uid, venue_id, my_socket, target_node, target_socket)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(node_id, my_socket) DO UPDATE SET
             uid = excluded.uid,
             venue_id = excluded.venue_id,
             target_node = excluded.target_node,
             target_socket = excluded.target_socket",
    )
    .bind(node_id)
    .bind(principal)
    .bind(venue_id)
    .bind(my_socket)
    .bind(target_node)
    .bind(target_socket)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to record venue constraint: {e}"))?;
    graph_changed();
    Ok(())
}

/// Merge parameters into a node. A `None` value clears the key, so "unset the
/// trim" and "set the trim" are the same call rather than two.
///
/// # Errors
/// Fails if any write is refused.
pub async fn set_params(
    access: &mut VenueAccess<'_, Write>,
    node_id: &str,
    params: &BTreeMap<String, Option<f64>>,
) -> Result<(), String> {
    let principal = access.principal().map(str::to_owned);
    let venue_id = access.venue_id().to_owned();
    for (key, value) in params {
        match value {
            Some(value) if value.is_finite() => {
                sqlx::query(
                    "INSERT INTO venue_node_params (node_id, uid, venue_id, key, value)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT(node_id, key) DO UPDATE SET
                         uid = excluded.uid,
                         venue_id = excluded.venue_id,
                         value = excluded.value",
                )
                .bind(node_id)
                .bind(principal.clone())
                .bind(venue_id.clone())
                .bind(key)
                .bind(value)
                .execute(&mut *access.connection())
                .await
                .map_err(|e| format!("Failed to set venue node param: {e}"))?;
            }
            // A non-finite value is a cleared key, not a stored NaN: NaN in a
            // transform poisons every descendant's pose.
            _ => {
                deletes::delete_where(
                    access.connection(),
                    "venue_node_params",
                    "node_id = ? AND key = ?",
                    &[node_id, key.as_str()],
                )
                .await
                .map_err(|e| format!("Failed to clear venue node param: {e}"))?;
            }
        }
    }
    graph_changed();
    Ok(())
}

/// Rename a node. `None` clears the label back to the default the UI derives.
///
/// # Errors
/// Fails if the update is refused.
pub async fn set_label(
    access: &mut VenueAccess<'_, Write>,
    node_id: &str,
    label: Option<&str>,
) -> Result<(), String> {
    sqlx::query("UPDATE venue_nodes SET label = ? WHERE id = ?")
        .bind(label)
        .bind(node_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to rename venue node: {e}"))?;
    graph_changed();
    Ok(())
}

/// Delete the named nodes, and everything hanging off each row.
///
/// The caller passes the whole subtree it means to remove, because which nodes
/// those are is the graph's question, not SQL's — and because a light's patch
/// row has to go with its node, which only the caller can do
/// ([`crate::services::fixture_create::delete`]). Children go before their
/// parent, so each delete is its own row in the upload queue rather than a
/// cascade. A node's params and constraints go with it ([`deletes`]).
///
/// # Errors
/// Fails if any delete is refused.
pub async fn delete_nodes(
    access: &mut VenueAccess<'_, Write>,
    ids: &[String],
) -> Result<(), String> {
    for id in ids.iter().rev() {
        deletes::delete_where(access.connection(), "venue_nodes", "id = ?", &[id.as_str()])
            .await
            .map_err(|e| format!("Failed to delete venue node: {e}"))?;
    }
    graph_changed();
    Ok(())
}

/// Which of `ids` belong to this venue. The guard on every id a caller hands
/// in: `VenueAccess` admits one venue, and a node id is not proof of anything.
///
/// # Errors
/// Fails if `venue_nodes` cannot be read.
pub async fn nodes_in_venue(
    access: &mut impl AuthorizedVenue,
    ids: &[String],
) -> Result<Vec<String>, String> {
    let venue_id = access.venue_id().to_string();
    let mut found = Vec::new();
    for id in ids {
        let exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM venue_nodes WHERE id = ? AND venue_id = ?")
                .bind(id)
                .bind(&venue_id)
                .fetch_optional(&mut *access.connection())
                .await
                .map_err(|e| format!("Failed to check venue node: {e}"))?;
        if let Some(id) = exists {
            found.push(id);
        }
    }
    Ok(found)
}
