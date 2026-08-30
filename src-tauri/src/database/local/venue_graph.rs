//! The four venue-graph tables.
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

use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::venue_graph::{VenueConstraint, VenueEdge, VenueGraphRows, VenueNode};

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

// -----------------------------------------------------------------------------
// Reads
// -----------------------------------------------------------------------------

/// Every row of one venue's graph, in id order.
///
/// # Errors
/// Fails if any of the four tables cannot be read.
pub async fn get_graph(access: &mut impl AuthorizedVenue) -> Result<VenueGraphRows, String> {
    let venue_id = access.venue_id().to_string();

    let nodes = sqlx::query_as::<_, VenueNode>(
        "SELECT id, venue_id, kind, catalog_ref, label
         FROM venue_nodes WHERE venue_id = ? ORDER BY id ASC",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to read venue nodes: {e}"))?;

    let edges = sqlx::query_as::<_, VenueEdge>(
        "SELECT edge.child_id, edge.parent_id, edge.my_socket, edge.their_socket, edge.roll
         FROM venue_edges edge
         JOIN venue_nodes node ON node.id = edge.child_id
         WHERE node.venue_id = ? ORDER BY edge.child_id ASC",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to read venue edges: {e}"))?;

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
        edges,
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

/// Insert a node and return its generated id.
///
/// # Errors
/// Fails if the insert is refused — most often by write admission.
pub async fn insert_node(
    access: &mut VenueAccess<'_, Write>,
    kind: &str,
    catalog_ref: Option<&str>,
    label: Option<&str>,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();
    insert_node_with_id(access, &id, kind, catalog_ref, label).await?;
    Ok(id)
}

/// Insert a node under an id the caller chose — the migration pass, which
/// reuses the old row's id so that a group membership or a saved selection
/// naming a stage piece still names the same thing.
///
/// # Errors
/// As [`insert_node`].
pub async fn insert_node_with_id(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    kind: &str,
    catalog_ref: Option<&str>,
    label: Option<&str>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    let principal = access.principal().map(str::to_owned);
    sqlx::query(
        "INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(principal)
    .bind(venue_id)
    .bind(kind)
    .bind(catalog_ref)
    .bind(label)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to insert venue node: {e}"))?;
    graph_changed();
    Ok(())
}

/// Place a node, replacing whatever edge it had. One statement, because
/// "exactly one parent" is the primary key and an insert-then-delete would be
/// two states the invariant is false in.
///
/// # Errors
/// Fails if the upsert is refused.
pub async fn upsert_edge(
    access: &mut VenueAccess<'_, Write>,
    child_id: &str,
    parent_id: &str,
    my_socket: &str,
    their_socket: &str,
    roll: f64,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO venue_edges (child_id, parent_id, my_socket, their_socket, roll)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(child_id) DO UPDATE SET
             parent_id = excluded.parent_id,
             my_socket = excluded.my_socket,
             their_socket = excluded.their_socket,
             roll = excluded.roll",
    )
    .bind(child_id)
    .bind(parent_id)
    .bind(my_socket)
    .bind(their_socket)
    .bind(roll)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to attach venue node: {e}"))?;
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
/// here, exactly as it is for [`upsert_edge`]: this layer writes rows the
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
    sqlx::query(
        "INSERT INTO venue_constraints (node_id, my_socket, target_node, target_socket)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(node_id, my_socket) DO UPDATE SET
             target_node = excluded.target_node,
             target_socket = excluded.target_socket",
    )
    .bind(node_id)
    .bind(my_socket)
    .bind(target_node)
    .bind(target_socket)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to record venue constraint: {e}"))?;
    graph_changed();
    Ok(())
}

/// Unplace a node, leaving it and its subtree out of the solve.
///
/// # Errors
/// Fails if the delete is refused.
pub async fn delete_edge(
    access: &mut VenueAccess<'_, Write>,
    child_id: &str,
) -> Result<(), String> {
    sqlx::query("DELETE FROM venue_edges WHERE child_id = ?")
        .bind(child_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to detach venue node: {e}"))?;
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
    for (key, value) in params {
        match value {
            Some(value) if value.is_finite() => {
                sqlx::query(
                    "INSERT INTO venue_node_params (node_id, key, value) VALUES (?, ?, ?)
                     ON CONFLICT(node_id, key) DO UPDATE SET value = excluded.value",
                )
                .bind(node_id)
                .bind(key)
                .bind(value)
                .execute(&mut *access.connection())
                .await
                .map_err(|e| format!("Failed to set venue node param: {e}"))?;
            }
            // A non-finite value is a cleared key, not a stored NaN: NaN in a
            // transform poisons every descendant's pose.
            _ => {
                sqlx::query("DELETE FROM venue_node_params WHERE node_id = ? AND key = ?")
                    .bind(node_id)
                    .bind(key)
                    .execute(&mut *access.connection())
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
/// those are is the graph's question, not SQL's.
///
/// # Why the belongings go first, rather than by cascade
///
/// The three dependent tables declare `ON DELETE CASCADE`, but their write
/// admission (`20260802600000`) authorizes a param or a constraint *through the
/// node that owns it* — and during a cascade that node is already gone, so the
/// `BEFORE DELETE` guard finds no owner and aborts the whole statement. Deleting
/// them while the node still stands is the same set of rows, in the order the
/// admission can actually check.
///
/// Dropping the edges a node is the **parent** of is what leaves its children
/// unplaced rather than dangling at a parent that no longer exists — which is
/// how `delete_subtree` trays the lights off a truss it takes down.
///
/// # Errors
/// Fails if any delete is refused.
pub async fn delete_nodes(
    access: &mut VenueAccess<'_, Write>,
    ids: &[String],
) -> Result<(), String> {
    // Everything that hangs off a node goes first, in one pass over the whole
    // batch, and only then the nodes themselves.
    //
    // Not for tidiness — `ON DELETE CASCADE` would do it — but because the
    // cascade cannot be authorized. Each of these tables has a delete trigger
    // that admits the write by joining the row back to its `venue_nodes` row,
    // and inside a cascade that row is already gone: the join finds nothing,
    // the trigger aborts, and the whole delete fails with the *dependant's*
    // error rather than the node's. Deleting them while their node still
    // stands is what lets the trigger see what it is checking. The batch is
    // swept table by table because a subtree's edges name its siblings.
    for id in ids {
        for statement in [
            "DELETE FROM venue_constraints WHERE node_id = ? OR target_node = ?",
            "DELETE FROM venue_edges WHERE child_id = ? OR parent_id = ?",
        ] {
            sqlx::query(statement)
                .bind(id)
                .bind(id)
                .execute(&mut *access.connection())
                .await
                .map_err(|e| format!("Failed to detach venue node: {e}"))?;
        }
        sqlx::query("DELETE FROM venue_node_params WHERE node_id = ?")
            .bind(id)
            .execute(&mut *access.connection())
            .await
            .map_err(|e| format!("Failed to clear venue node params: {e}"))?;
    }
    for id in ids {
        sqlx::query("DELETE FROM venue_nodes WHERE id = ?")
            .bind(id)
            .execute(&mut *access.connection())
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
