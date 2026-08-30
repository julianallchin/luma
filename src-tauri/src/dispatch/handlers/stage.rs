//! The venue graph across the host boundary.
//!
//! Six verbs and two reads. Every verb writes rows and then returns the whole
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
use crate::services::fixture_create;
use crate::venue_graph;
use luma_render::venue_tiles::TileMap;
use luma_scene::venue::{Constraint, Edge, NodeKind, FLOOR_SOCKET};

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

/// The venue as a top-down text map — the "Gauntlet view".
///
/// The same solve [`get_resolved_venue`] returns, quantized into something a
/// human or an agent reads at a glance and diffs line by line. `cell_m` is
/// metres per character; the drawer clamps it, so no value refuses.
///
/// # Errors
/// As [`get_resolved_venue`].
pub async fn venue_tiles(
    services: &AppServices,
    venue_id: String,
    cell_m: Option<f64>,
) -> Result<String, CommandError> {
    let mut access = admit(services, &venue_id).await?;
    let options = TileMap {
        cell_m: cell_m.unwrap_or(TileMap::default().cell_m),
        ..TileMap::default()
    };
    Ok(venue_graph::tiles(&mut access, &services.fixtures_root, options).await?)
}

/// Place a new node by mating two sockets.
///
/// # Errors
/// Refuses a socket pair that does not exist or whose polarity forbids the
/// joint, and a parent that cannot host: one that would close a cycle, or an
/// array, whose members are derived and have no row to bolt to. Those are the
/// hard errors the design admits; everything else comes back as a warning on
/// the report.
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

/// Write down a far end: this socket meets that one.
///
/// A **check**, not an edge — it is evaluated after the solve and never takes
/// part in it, which is how a bridging piece has one parent and still says
/// where its other end belongs. A socket carries one far end or none, so
/// naming the same socket again replaces the check it had.
///
/// # Errors
/// Refuses a socket pair that does not exist or whose polarity forbids the
/// joint, and an array at either end: an array's ends belong to its derived
/// members, so one check would name one socket where the room has `count` of
/// them. Whether the ends actually *meet* is not an error — that is the
/// satisfied / violated / dangling the report carries.
pub async fn constrain(
    services: &AppServices,
    venue_id: String,
    node_id: String,
    my_socket: String,
    target_node: String,
    target_socket: String,
) -> Result<PlacementReport, CommandError> {
    let mut access = write(services, &venue_id).await?;
    require_in_venue(&mut access, &[node_id.clone(), target_node.clone()]).await?;

    let mut graph = venue_graph::graph(&mut access).await?;
    let sockets = venue_graph::sockets(&services.fixtures_root)?;
    graph
        .constrain(
            Constraint {
                node: node_id.clone(),
                my_socket: my_socket.clone(),
                target_node: target_node.clone(),
                target_socket: target_socket.clone(),
            },
            sockets,
        )
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    venue_graph_db::upsert_constraint(
        &mut access,
        &node_id,
        &my_socket,
        &target_node,
        &target_socket,
    )
    .await?;
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

/// Delete a node and everything structural hanging off it.
///
/// The subtree is computed from the graph rather than left to a foreign-key
/// cascade: which nodes those are is the graph's question, and the answer has
/// to be the same one the builder just previewed.
///
/// # A fixture is inventory, not structure
///
/// Pulling a truss down loses the rig its shape, not its lights: every fixture
/// under the deleted node is **trayed** — its edge cascades away with its
/// parent, so the solve reports it unplaced and the tray can hang it
/// somewhere else. Only a fixture the caller names *directly* is deleted, and
/// then through [`fixture_create::delete`], which is the one door that takes
/// the patch row with the node.
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
    let (fixtures, structure): (Vec<String>, Vec<String>) = graph
        .subtree(&node_id)
        .into_iter()
        .partition(|id| graph.node(id).is_some_and(|n| n.kind == NodeKind::Fixture));
    venue_graph_db::delete_nodes(&mut access, &structure).await?;
    if fixtures.contains(&node_id) {
        fixture_create::delete(&mut access, &node_id).await?;
    }
    let solved = venue_graph::resolved(&mut access, &services.fixtures_root).await?;
    let out = ResolvedVenue::from(&solved);
    access.commit().await?;
    venue_graph_db::graph_committed();
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
    venue_graph_db::graph_committed();
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices, CommandError};

    /// A deck: `bottom` mates the floor, `top` is a surface, `edge_*` are the
    /// self-mating sides. Real geometry, because the socket supply is a
    /// measured GLB and a stub would pin half the answer.
    const DECK: &str = "stage_lab/stage_praticavel_2x1x1.glb";
    const SPEAKER: &str = "stage_lab/speaker_dbr15.glb";
    /// The generated stick. Its two `TrussEnd`s are the only self-mating
    /// sockets in this catalog, so it is what "open end" is measured on.
    const TRUSS: &str = "truss/straight";

    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_socket_the_geometry_does_not_have_is_refused_by_name() {
        let (_dir, services, venue) = room().await;
        let error = place(&services, &venue, "stage", DECK, "nope", 0.0, 0.0)
            .await
            .expect_err("a deck has no socket called `nope`");
        assert_invalid(&error, "has no socket `nope`");
    }

    /// Two receptacles do not make a joint: a deck's `top` is a host, never a
    /// thing that is held.
    #[tokio::test]
    async fn two_female_sockets_are_refused() {
        let (_dir, services, venue) = room().await;
        let error = place(&services, &venue, "stage", DECK, "top", 0.0, 0.0)
            .await
            .expect_err("`top` cannot be carried onto the floor");
        assert_invalid(&error, "does not mate");
    }

    /// A refused verb must leave nothing behind. `attach` inserts the node row
    /// and *then* checks the edge, so the guard is the write transaction: if it
    /// ever committed early, a rejected placement would litter the venue with
    /// parentless nodes.
    #[tokio::test]
    async fn a_refused_placement_writes_no_rows() {
        let (_dir, services, venue) = room().await;
        let before = node_count(&services, &venue).await;
        place(&services, &venue, "stage", DECK, "nope", 0.0, 0.0)
            .await
            .expect_err("the placement was accepted");
        assert_eq!(before, node_count(&services, &venue).await);
    }

    #[tokio::test]
    async fn a_node_cannot_be_its_own_parent() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let error = reattach(&services, &venue, &deck, &deck, "edge_left", "edge_right")
            .await
            .expect_err("a deck was bolted to itself");
        assert_invalid(&error, "so attaching would loop");
    }

    #[tokio::test]
    async fn a_cycle_is_refused() {
        let (_dir, services, venue) = room().await;
        let a = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let b = attach(
            &services,
            &venue,
            "stage",
            DECK,
            &a,
            "edge_left",
            "edge_right",
            None,
        )
        .await
        .unwrap();
        let error = reattach(&services, &venue, &a, &b, "edge_right", "edge_left")
            .await
            .expect_err("a is inside b is inside a");
        assert_invalid(&error, "so attaching would loop");
    }

    /// The root is the venue frame, not a piece in the room.
    #[tokio::test]
    async fn the_root_cannot_be_reattached() {
        let (_dir, services, venue) = room().await;
        let root = root_id(&services, &venue).await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let error = reattach(&services, &venue, &root, &deck, "bottom", "top")
            .await
            .expect_err("the room was hung off a deck");
        assert_invalid(&error, "the venue root cannot be attached");
    }

    /// A node id is not authorization. Naming another venue's deck as a parent
    /// is refused before anything is written, not resolved into it.
    #[tokio::test]
    async fn a_parent_in_another_venue_is_refused() {
        let (_dir, services, venue) = room().await;
        let other = venue_named(&services, "Other room").await;
        let theirs = place(&services, &other, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let error = attach(
            &services,
            &venue,
            "stage",
            DECK,
            &theirs,
            "edge_left",
            "edge_right",
            None,
        )
        .await
        .expect_err("a deck reached across venues");
        assert_invalid(&error, "not in this venue");
    }

    /// The solve is depth-first with children in id order, so two reads of an
    /// unchanged venue are the same bytes. The golden capture depends on it and
    /// so does every diff a builder shows.
    #[tokio::test]
    async fn resolving_twice_gives_the_same_venue() {
        let (_dir, services, venue) = room().await;
        let a = place(&services, &venue, "stage", DECK, "bottom", 1.5, -2.25)
            .await
            .unwrap();
        attach(
            &services,
            &venue,
            "stage",
            DECK,
            &a,
            "edge_left",
            "edge_right",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            resolved(&services, &venue).await,
            resolved(&services, &venue).await
        );
    }

    /// A joint accounts for **both** of its halves, the held one and the host
    /// one. Two decks butted edge to edge have eight edges between them and
    /// six of them open.
    #[tokio::test]
    async fn a_mated_edge_is_not_reported_dangling() {
        let (_dir, services, venue) = room().await;
        let a = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let b = attach(
            &services,
            &venue,
            "stage",
            DECK,
            &a,
            "edge_left",
            "edge_right",
            None,
        )
        .await
        .unwrap();

        let venue_json = resolved(&services, &venue).await;
        let open: Vec<(&str, &str)> = venue_json["dangling"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["nodeId"].as_str().unwrap(), d["socket"].as_str().unwrap()))
            .collect();
        assert!(
            !open.contains(&(a.as_str(), "edge_right")),
            "the host half of the joint is open: {open:?}"
        );
        assert!(
            !open.contains(&(b.as_str(), "edge_left")),
            "the held half of the joint is open: {open:?}"
        );
        assert_eq!(open.len(), 6, "six outer edges stay open: {open:?}");
    }

    /// An array is one row and reports a placement like any other node: the
    /// anchor gets a pose the span is centred on, and each of `count` members
    /// gets one of its own.
    #[tokio::test]
    async fn an_array_is_one_row_and_reports_a_placement() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let report = dispatch(
            &services,
            "attach",
            &json!({
                "venueId": venue,
                "kind": "array",
                "catalogRef": SPEAKER,
                "label": null,
                "parentId": deck,
                "mySocket": "mount",
                "theirSocket": "top",
                "yaw": null,
                "params": { "count": 4.0, "span": 3.0 },
            }),
        )
        .await
        .expect("the array was refused");

        let id = report["nodeId"].as_str().unwrap().to_string();
        assert_eq!(
            report["outcome"],
            json!("placed"),
            "a placed array is not reported placed"
        );
        assert_eq!(report["parentId"], json!(deck));

        // One row in the graph, `count` derived members plus the anchor in the
        // solve: members are derived, never stored.
        let rows = dispatch(&services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap();
        assert_eq!(
            rows["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|n| n["id"] == json!(id))
                .count(),
            1
        );
        let placed: Vec<&str> = report["venue"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .filter(|node| node.starts_with(&id))
            .collect();
        assert_eq!(
            placed,
            [
                id.as_str(),
                &format!("{id}#0"),
                &format!("{id}#1"),
                &format!("{id}#2"),
                &format!("{id}#3"),
            ]
        );
        // Members hang off the generator, which now has a frame to hang from.
        for member in report["venue"]["nodes"].as_array().unwrap() {
            if member["arrayIndex"].is_number() {
                assert_eq!(member["parentId"], json!(id));
            }
        }
    }

    /// A far end is what the builder writes down instead of a second parent,
    /// so it accounts for both sockets it names — whether or not they meet.
    /// A violated check has *measured* those ends; the gap is reported as
    /// itself.
    #[tokio::test]
    async fn a_far_end_check_accounts_for_both_its_ends() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let left = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();
        let right = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fr",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();
        let before = open_ends(&services, &venue).await;
        assert!(before.contains(&(left.clone(), "end_b".into())));
        assert!(before.contains(&(right.clone(), "end_b".into())));

        let report = constrain(&services, &venue, &left, "end_b", &right, "end_b")
            .await
            .expect("two truss ends can be checked against each other");
        assert_eq!(
            report["venue"]["constraints"][0]["status"],
            json!("violated"),
            "the towers stand apart, so the check resolved and failed"
        );

        let after = open_ends(&services, &venue).await;
        assert!(
            !after.contains(&(left.clone(), "end_b".into()))
                && !after.contains(&(right.clone(), "end_b".into())),
            "a resolved check accounts for both ends it names: {after:?}"
        );
    }

    /// A check whose target is gone claims nothing: the node is not in the
    /// room, so the socket it named is still standing open. Closing it on the
    /// strength of the paperwork would hide the very end the paperwork was
    /// meant to explain.
    #[tokio::test]
    async fn a_dangling_constraint_leaves_its_end_open() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let left = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();
        let right = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fr",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();
        constrain(&services, &venue, &left, "end_b", &right, "end_b")
            .await
            .unwrap();

        // Delete the target out from under the check. The row goes with it
        // (`ON DELETE CASCADE` names `target_node`), so this is the general
        // case: unplaced, gone, or a socket the geometry lost.
        dispatch(
            &services,
            "detach",
            &json!({ "venueId": venue, "nodeId": right }),
        )
        .await
        .unwrap();

        let venue_json = resolved(&services, &venue).await;
        assert_eq!(
            venue_json["constraints"][0]["status"],
            json!("dangling"),
            "the target has no pose to measure against"
        );
        let open = open_ends(&services, &venue).await;
        assert!(
            open.contains(&(left.clone(), "end_b".into())),
            "the checked end is still standing open: {open:?}"
        );
    }

    /// An array's ends belong to its derived members, which hold no rows — so
    /// one check naming the anchor would name one socket where the room has
    /// `count` of them. Refused at either end.
    #[tokio::test]
    async fn a_constraint_cannot_name_an_array() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let array = attach(
            &services,
            &venue,
            "array",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "count": 3.0, "span": 4.0 })),
        )
        .await
        .unwrap();
        let stick = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fr",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();
        let before = open_ends(&services, &venue).await;

        let error = constrain(&services, &venue, &stick, "end_b", &array, "end_b")
            .await
            .expect_err("a far end was pointed at an array");
        assert_invalid(&error, "is an array");

        let error = constrain(&services, &venue, &array, "end_b", &stick, "end_b")
            .await
            .expect_err("an array end was checked against a truss");
        assert_invalid(&error, "is an array");

        assert_eq!(
            constraint_count(&services, &venue).await,
            0,
            "refused before any write"
        );
        assert_eq!(
            open_ends(&services, &venue).await,
            before,
            "and nothing was accounted for"
        );
    }

    /// A check names two sockets that exist and could meet. One that no
    /// placement could ever satisfy is a typo, and reporting it every solve as
    /// a gap would say the rig is wrong where the paperwork is.
    #[tokio::test]
    async fn a_constraint_needs_two_sockets_that_could_mate() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let tower = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();

        let error = constrain(&services, &venue, &tower, "nope", &deck, "top")
            .await
            .expect_err("a truss has no socket called `nope`");
        assert_invalid(&error, "has no socket `nope`");

        // Two receptacles do not make a joint, here as at an edge: a deck's
        // `top` is a host, never a thing that is held.
        let error = constrain(&services, &venue, &deck, "top", &tower, "end_b")
            .await
            .expect_err("a floor top was checked against a truss end");
        assert_invalid(&error, "does not mate");

        assert_eq!(constraint_count(&services, &venue).await, 0);
    }

    /// An array's members are derived at solve time, so the anchor is the only
    /// row an edge could name — and one edge on it would seat the child on
    /// every copy at once, through a seat with no geometry.
    #[tokio::test]
    async fn an_array_cannot_be_a_parent() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let array = attach(
            &services,
            &venue,
            "array",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "count": 3.0, "span": 4.0 })),
        )
        .await
        .unwrap();

        let before = node_count(&services, &venue).await;
        let error = attach(
            &services,
            &venue,
            "run",
            TRUSS,
            &array,
            "end_a",
            "end_b",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .expect_err("a stick was bolted to an array");
        assert_invalid(&error, "is an array");
        assert_eq!(
            before,
            node_count(&services, &venue).await,
            "a refused placement writes no rows"
        );

        // Every member's every end is still open: one edge on the anchor would
        // have closed one on all three.
        let open: Vec<(String, String)> = resolved(&services, &venue).await["dangling"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["nodeId"].as_str().unwrap().to_string(),
                    d["socket"].as_str().unwrap().to_string(),
                )
            })
            .filter(|(node, _)| node.starts_with(&array))
            .collect();
        assert_eq!(
            open,
            [
                (format!("{array}#0"), "end_b".to_string()),
                (format!("{array}#1"), "end_b".to_string()),
                (format!("{array}#2"), "end_b".to_string()),
            ],
            "three sticks, three free ends; the bolted end is spent on each"
        );
    }

    /// A stick bolted to a deck corner at one end has one open end, and the
    /// bolted one is accounted for.
    #[tokio::test]
    async fn an_open_truss_end_is_dangling() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let post = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();

        let open: Vec<String> = resolved(&services, &venue).await["dangling"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["nodeId"] == json!(post))
            .map(|d| d["socket"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(open, ["end_b"], "the bolted end is not open: {open:?}");
    }

    /// A branch with no edge is reported, never dropped: `detach` names the
    /// subtree's root and how many nodes hang off it. Silence is what makes
    /// "unplaced" and "deleted" look identical to whoever just dragged a wing
    /// off.
    #[tokio::test]
    async fn a_detached_subtree_is_reported_unplaced() {
        let (_dir, services, venue) = room().await;
        let deck = place(&services, &venue, "stage", DECK, "bottom", 0.0, 0.0)
            .await
            .unwrap();
        let post = attach(
            &services,
            &venue,
            "tower",
            TRUSS,
            &deck,
            "end_a",
            "corner_fl",
            Some(json!({ "span": 2.0 })),
        )
        .await
        .unwrap();
        let head = attach(
            &services,
            &venue,
            "run",
            TRUSS,
            &post,
            "end_a",
            "end_b",
            Some(json!({ "span": 1.5 })),
        )
        .await
        .unwrap();
        assert!(resolved(&services, &venue).await["unplaced"]
            .as_array()
            .unwrap()
            .is_empty());

        let report = dispatch(
            &services,
            "detach",
            &json!({ "venueId": venue, "nodeId": post }),
        )
        .await
        .expect("the detach was refused");

        // `unplaced`, and not a refusal: the call did exactly what it was
        // asked to do, and the outcome is a fact about the node.
        assert_eq!(
            report["outcome"],
            json!("unplaced"),
            "a detached node was not reported unplaced"
        );
        let unplaced = report["venue"]["unplaced"].as_array().unwrap();
        assert_eq!(
            unplaced
                .iter()
                .map(|u| u["nodeId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [post.as_str()],
            "the root of the branch, listed once"
        );
        assert_eq!(
            unplaced[0]["descendants"],
            json!(1),
            "the speaker on it came along"
        );
        // The rows are still there — detach unplaces, it does not delete.
        let placed: Vec<&str> = report["venue"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert!(!placed.contains(&post.as_str()));
        assert!(!placed.contains(&head.as_str()));
        assert_eq!(
            node_count(&services, &venue).await,
            4,
            "root, deck, post, head: unplacing deletes nothing"
        );
    }

    // -----------------------------------------------------------------------
    // Plumbing
    // -----------------------------------------------------------------------

    fn assert_invalid(error: &CommandError, needle: &str) {
        let CommandError::Invalid(message) = error else {
            panic!("expected a refusal, got {} ({error})", error.kind());
        };
        assert!(
            message.contains(needle),
            "`{message}` does not mention `{needle}`"
        );
    }

    async fn room() -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        let venue = venue_named(&services, "Golden room").await;
        (directory, services, venue)
    }

    async fn venue_named(services: &AppServices, name: &str) -> String {
        dispatch(
            services,
            "create_venue",
            &json!({ "name": name, "description": null }),
        )
        .await
        .expect("the venue was not created")["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn place(
        services: &AppServices,
        venue: &str,
        kind: &str,
        catalog_ref: &str,
        my_socket: &str,
        u: f64,
        v: f64,
    ) -> Result<String, CommandError> {
        let report = dispatch(
            services,
            "place_free",
            &json!({
                "venueId": venue,
                "kind": kind,
                "catalogRef": catalog_ref,
                "label": null,
                "surfaceNodeId": null,
                "surfaceSocket": null,
                "mySocket": my_socket,
                "u": u,
                "v": v,
                "yaw": null,
                "trim": null,
            }),
        )
        .await?;
        Ok(report["nodeId"].as_str().unwrap().to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach(
        services: &AppServices,
        venue: &str,
        kind: &str,
        catalog_ref: &str,
        parent: &str,
        my_socket: &str,
        their_socket: &str,
        params: Option<Value>,
    ) -> Result<String, CommandError> {
        let report = dispatch(
            services,
            "attach",
            &json!({
                "venueId": venue,
                "kind": kind,
                "catalogRef": catalog_ref,
                "label": null,
                "parentId": parent,
                "mySocket": my_socket,
                "theirSocket": their_socket,
                "yaw": null,
                "params": params,
            }),
        )
        .await?;
        Ok(report["nodeId"].as_str().unwrap().to_string())
    }

    async fn reattach(
        services: &AppServices,
        venue: &str,
        node: &str,
        parent: &str,
        my_socket: &str,
        their_socket: &str,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "reattach",
            &json!({
                "venueId": venue,
                "nodeId": node,
                "parentId": parent,
                "mySocket": my_socket,
                "theirSocket": their_socket,
                "yaw": null,
            }),
        )
        .await
    }

    async fn constrain(
        services: &AppServices,
        venue: &str,
        node: &str,
        my_socket: &str,
        target_node: &str,
        target_socket: &str,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "constrain",
            &json!({
                "venueId": venue,
                "nodeId": node,
                "mySocket": my_socket,
                "targetNode": target_node,
                "targetSocket": target_socket,
            }),
        )
        .await
    }

    /// Every open end in the solved venue, as `(node, socket)`.
    async fn open_ends(services: &AppServices, venue: &str) -> Vec<(String, String)> {
        resolved(services, venue).await["dangling"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["nodeId"].as_str().unwrap().to_string(),
                    d["socket"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    async fn constraint_count(services: &AppServices, venue: &str) -> usize {
        dispatch(services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap()["constraints"]
            .as_array()
            .unwrap()
            .len()
    }

    async fn resolved(services: &AppServices, venue: &str) -> Value {
        dispatch(services, "get_resolved_venue", &json!({ "venueId": venue }))
            .await
            .expect("the venue did not resolve")
    }

    async fn root_id(services: &AppServices, venue: &str) -> String {
        dispatch(services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["kind"] == json!("venue"))
            .expect("every venue has a root")["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn node_count(services: &AppServices, venue: &str) -> usize {
        dispatch(services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap()["nodes"]
            .as_array()
            .unwrap()
            .len()
    }

    /// A headless host over a temporary database. `fixtures_root` is the temp
    /// directory: the socket supply resolves against the repo's shipped meshes
    /// either way (`stage_render::meshes_root`), and the catalog is what these
    /// tests are about.
    async fn seed(directory: &Path) -> AppServices {
        let db = database::init_app_db_at(directory).await.unwrap();
        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::bootstrap_headless_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        AppServices::headless(db, state_db, storage, PathBuf::from(directory), workspaces)
    }
}
