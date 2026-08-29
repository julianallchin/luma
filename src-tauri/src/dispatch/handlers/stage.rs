//! The venue graph across the host boundary.
//!
//! Five verbs and two reads. Every verb writes rows and then returns the whole
//! solved venue ([`PlacementReport`]), because a graph edit moves everything
//! bolted to what it touched: handing back the one row that changed would make
//! every caller re-fetch, and a second fetch is a second solve of the same
//! graph.
//!
//! Authorization is [`VenueAccess`] as everywhere else. Node ids are *not*
//! authorization: every id a caller hands in is checked against the admitted
//! venue by [`venue_graph_db::nodes_in_venue`] before it is written.

use std::collections::BTreeMap;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venue_graph as venue_graph_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::venue_graph::{PlacementReport, ResolvedVenue, VenueGraphRows};
use crate::venue_graph;
use luma_scene::venue::{Edge, NodeKind, FLOOR_SOCKET};

/// The rows themselves — what the builder edits.
///
/// # Errors
/// Fails if the venue is not readable, or if converting the old schema fails.
pub async fn get_venue_graph(
    services: &AppServices,
    venue_id: String,
) -> Result<VenueGraphRows, CommandError> {
    let mut access = admit(services, &venue_id).await?;
    Ok(venue_graph_db::get_graph(&mut access).await?)
}

/// The venue solved — what every consumer draws.
///
/// # Errors
/// As [`get_venue_graph`], plus a catalog that will not resolve.
pub async fn get_resolved_venue(
    services: &AppServices,
    venue_id: String,
) -> Result<ResolvedVenue, CommandError> {
    let mut access = admit(services, &venue_id).await?;
    let solved = venue_graph::resolved(&mut access, &services.fixtures_root).await?;
    Ok(ResolvedVenue::from(&solved))
}

/// Place a new node by mating two sockets.
///
/// # Errors
/// Refuses a socket pair that does not exist or whose polarity forbids the
/// joint, and a parent that would close a cycle. Those are the only two hard
/// errors the design admits; everything else comes back as a warning on the
/// report.
#[allow(clippy::too_many_arguments)]
pub async fn attach(
    services: &AppServices,
    venue_id: String,
    kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
    parent_id: String,
    my_socket: String,
    their_socket: String,
    yaw: Option<f64>,
    params: Option<BTreeMap<String, f64>>,
) -> Result<PlacementReport, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_kind(&kind)?;
    require_in_venue(&mut access, std::slice::from_ref(&parent_id)).await?;

    let node_id =
        venue_graph_db::insert_node(&mut access, &kind, catalog_ref.as_deref(), label.as_deref())
            .await?;
    if let Some(params) = params {
        venue_graph_db::set_params(&mut access, &node_id, &keep(params)).await?;
    }
    let edge = Edge {
        parent: parent_id.clone(),
        my_socket,
        their_socket,
        roll: yaw.unwrap_or(0.0),
    };
    check_and_write(&mut access, services, &node_id, edge).await?;
    report(access, services, &node_id).await
}

/// Place a node that already exists somewhere else — a re-attach, or a fixture
/// dragged out of the patch tray.
///
/// # Errors
/// As [`attach`].
pub async fn reattach(
    services: &AppServices,
    venue_id: String,
    node_id: String,
    parent_id: String,
    my_socket: String,
    their_socket: String,
    yaw: Option<f64>,
) -> Result<PlacementReport, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_in_venue(&mut access, &[node_id.clone(), parent_id.clone()]).await?;
    let edge = Edge {
        parent: parent_id,
        my_socket,
        their_socket,
        roll: yaw.unwrap_or(0.0),
    };
    check_and_write(&mut access, services, &node_id, edge).await?;
    report(access, services, &node_id).await
}

/// Free placement: put a node on a surface at `(u, v, yaw, trim)`.
///
/// The floor and the grid are the venue root's own two surfaces, so a piece on
/// the deck and a light in the air take the same path as a piece on a stage —
/// there is no "unparented" branch to get wrong.
///
/// # Errors
/// As [`attach`].
#[allow(clippy::too_many_arguments)]
pub async fn place_free(
    services: &AppServices,
    venue_id: String,
    kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
    surface_node_id: Option<String>,
    surface_socket: Option<String>,
    my_socket: String,
    u: f64,
    v: f64,
    yaw: Option<f64>,
    trim: Option<f64>,
) -> Result<PlacementReport, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_kind(&kind)?;
    let parent_id = match surface_node_id {
        Some(id) => {
            require_in_venue(&mut access, std::slice::from_ref(&id)).await?;
            id
        }
        None => venue_graph_db::root_id(&mut access)
            .await?
            .ok_or_else(|| CommandError::Invalid("this venue has no graph root".into()))?,
    };

    let node_id =
        venue_graph_db::insert_node(&mut access, &kind, catalog_ref.as_deref(), label.as_deref())
            .await?;
    let params = BTreeMap::from([
        ("u".to_string(), u),
        ("v".to_string(), v),
        ("trim".to_string(), trim.unwrap_or(0.0)),
    ]);
    venue_graph_db::set_params(&mut access, &node_id, &keep(params)).await?;
    let edge = Edge {
        parent: parent_id,
        my_socket,
        their_socket: surface_socket.unwrap_or_else(|| FLOOR_SOCKET.to_string()),
        roll: yaw.unwrap_or(0.0),
    };
    check_and_write(&mut access, services, &node_id, edge).await?;
    report(access, services, &node_id).await
}

/// Unplace a node. It and its subtree drop out of the solve; the rows stay, so
/// re-attaching restores the whole branch.
///
/// # Errors
/// Fails if the venue is not writable or the node is not in it.
pub async fn detach(
    services: &AppServices,
    venue_id: String,
    node_id: String,
) -> Result<PlacementReport, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_in_venue(&mut access, std::slice::from_ref(&node_id)).await?;
    venue_graph_db::delete_edge(&mut access, &node_id).await?;
    report(access, services, &node_id).await
}

/// Merge parameters into a node, and optionally rename it.
///
/// `yaw` is spelled as itself and lands on the edge, which is where the
/// mate's turn about the shared normal lives; every other key is a param.
///
/// # Errors
/// Fails if the venue is not writable or the node is not in it.
pub async fn set_params(
    services: &AppServices,
    venue_id: String,
    node_id: String,
    params: BTreeMap<String, f64>,
    label: Option<String>,
) -> Result<PlacementReport, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_in_venue(&mut access, std::slice::from_ref(&node_id)).await?;

    let mut params = params;
    if let Some(yaw) = params.remove("yaw") {
        let graph = venue_graph::graph(&mut access).await?;
        let Some(edge) = graph.edge(&node_id).cloned() else {
            return Err(CommandError::Invalid(
                "an unplaced node has no yaw to set".into(),
            ));
        };
        venue_graph_db::upsert_edge(
            &mut access,
            &node_id,
            &edge.parent,
            &edge.my_socket,
            &edge.their_socket,
            yaw,
        )
        .await?;
    }
    venue_graph_db::set_params(&mut access, &node_id, &keep(params)).await?;
    if label.is_some() {
        venue_graph_db::set_label(&mut access, &node_id, label.as_deref()).await?;
    }
    report(access, services, &node_id).await
}

/// Delete a node and everything hanging off it.
///
/// The subtree is computed from the graph rather than left to a foreign-key
/// cascade: which nodes those are is the graph's question, and the answer has
/// to be the same one the builder just previewed.
///
/// # Errors
/// Fails if the venue is not writable or the node is not in it.
pub async fn delete_subtree(
    services: &AppServices,
    venue_id: String,
    node_id: String,
) -> Result<ResolvedVenue, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_in_venue(&mut access, std::slice::from_ref(&node_id)).await?;
    let graph = venue_graph::graph(&mut access).await?;
    if node_id == graph.root() {
        return Err(CommandError::Invalid(
            "the venue root is the room itself and cannot be deleted".into(),
        ));
    }
    let subtree = graph.subtree(&node_id);
    venue_graph_db::delete_nodes(&mut access, &subtree).await?;
    let solved = venue_graph::resolved(&mut access, &services.fixtures_root).await?;
    let out = ResolvedVenue::from(&solved);
    access.commit().await?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// A read snapshot, with the old schema converted first if it has not been.
///
/// The conversion needs a write, so a read that finds an unconverted venue
/// takes one, commits it, and then opens the snapshot. Ordinary reads — every
/// one after the first — never take a write lock.
async fn admit<'a>(
    services: &'a AppServices,
    venue_id: &str,
) -> Result<VenueAccess<'a, Read>, CommandError> {
    ensure_migrated(services, venue_id).await?;
    Ok(VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(venue_id)).await?)
}

async fn write<'a>(
    services: &'a AppServices,
    venue_id: &str,
) -> Result<VenueAccess<'a, Write>, CommandError> {
    ensure_migrated(services, venue_id).await?;
    Ok(VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(venue_id)).await?)
}

async fn ensure_migrated(services: &AppServices, venue_id: &str) -> Result<(), CommandError> {
    Ok(venue_graph::ensure_migrated(&services.db.0, venue_id, &services.fixtures_root).await?)
}

/// Check the edge against the graph's invariants, then write it.
///
/// The check is `luma_scene`'s, not a copy: acyclic, both sockets present on
/// their catalog entries, polarities compatible.
async fn check_and_write(
    access: &mut VenueAccess<'_, Write>,
    services: &AppServices,
    node_id: &str,
    edge: Edge,
) -> Result<(), CommandError> {
    let mut graph = venue_graph::graph(access).await?;
    let sockets = venue_graph::sockets(&services.fixtures_root)?;
    graph
        .attach(node_id, edge.clone(), sockets)
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    venue_graph_db::upsert_edge(
        access,
        node_id,
        &edge.parent,
        &edge.my_socket,
        &edge.their_socket,
        edge.roll,
    )
    .await?;
    Ok(())
}

/// Solve, commit, and report — the tail of every verb.
async fn report(
    mut access: VenueAccess<'_, Write>,
    services: &AppServices,
    node_id: &str,
) -> Result<PlacementReport, CommandError> {
    let solved = venue_graph::resolved(&mut access, &services.fixtures_root).await?;
    let report = PlacementReport::of(node_id, &solved);
    access.commit().await?;
    Ok(report)
}

fn require_kind(kind: &str) -> Result<(), CommandError> {
    match NodeKind::from_name(kind) {
        Some(NodeKind::Venue) => Err(CommandError::Invalid(
            "a venue has exactly one root, and it is made with the venue".into(),
        )),
        Some(_) => Ok(()),
        None => Err(CommandError::Invalid(format!(
            "`{kind}` is not a node kind"
        ))),
    }
}

/// Every id a caller names must belong to the venue it was admitted to.
async fn require_in_venue(
    access: &mut VenueAccess<'_, Write>,
    ids: &[String],
) -> Result<(), CommandError> {
    let found = venue_graph_db::nodes_in_venue(access, ids).await?;
    if found.len() == ids.len() {
        return Ok(());
    }
    Err(CommandError::Invalid(
        "that node is not in this venue".into(),
    ))
}

/// Every key present, every value kept. The `Option` in the DB layer is for
/// clearing a key, which the wire has no spelling for yet.
fn keep(params: BTreeMap<String, f64>) -> BTreeMap<String, Option<f64>> {
    params.into_iter().map(|(k, v)| (k, Some(v))).collect()
}
