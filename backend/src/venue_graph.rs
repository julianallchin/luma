//! Loading and solving a venue graph.
//!
//! [`luma_scene::venue`] owns the model and the maths;
//! [`crate::database::local::venue_graph`] owns the rows. This module is the
//! one place they meet, plus the one thing neither can do alone: supply the
//! geometry a socket resolves against.
//!
//! # Solve on every read
//!
//! There is no pose cache. A solve is a depth-first walk with a socket-frame
//! build and a 4x4 multiply per node, tens of microseconds against a 16 ms
//! frame, and a cache would be a second source of truth with no way to tell it
//! is stale (design doc, Performance).
//!
//! # A venue without its root
//!
//! Creating a venue writes its root node in the same transaction, so a venue
//! with no root is one whose rows are still arriving from sync. Every reader
//! sees it as empty; nothing here writes one in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use luma_scene::catalog::{FixtureDefinitions, VenueSockets};
use luma_scene::venue::{
    resolve as resolve_graph, Node, NodeKind, Params, ResolvedVenue, VenueGraph,
};
use luma_scene::venue_tiles::TileMap;

use crate::database::local;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::venue_graph::VenueGraphRows;

/// The catalog's geometry against `fixtures_root`, resolved once per root per
/// process.
///
/// Eager and shared because [`VenueSockets::load`] reads every GLB in the
/// palette, and a venue is solved on every read: paying that per solve would be
/// the one thing that makes the solve not free. Keyed by the root because the
/// root decides both the meshes and the fixture housings. The app sees one
/// root; the script-test runner sees one per test, all in one process, so a
/// load (a few hundred milliseconds) must not stall every other root's solve.
///
/// # Errors
/// Fails if a catalog piece's GLB is missing, unreadable, or measures empty.
pub fn sockets(fixtures_root: &Path) -> Result<&'static VenueSockets, String> {
    type Loaded = OnceLock<Result<VenueSockets, String>>;
    static SOCKETS: OnceLock<Mutex<HashMap<PathBuf, &'static Loaded>>> = OnceLock::new();
    // The map's lock only finds the root's cell; the load runs outside it, so
    // a root being loaded holds up only its own callers, never another root's.
    let cell: &'static Loaded = *SOCKETS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(fixtures_root.to_path_buf())
        .or_insert_with(|| Box::leak(Box::default()));
    cell.get_or_init(|| {
        VenueSockets::load(
            crate::resources::meshes_root(Some(fixtures_root)),
            std::sync::Arc::new(BundledFixtures(fixtures_root.to_path_buf())),
        )
        .map_err(|e| format!("the stage catalog could not be resolved: {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// The shipped QLC+ bundle, as the socket supply's answer to "how tall is this
/// light".
///
/// Keyed by `fixture_path` — which is what a fixture node's `catalog_ref`
/// carries, the same way a piece node's carries its mesh — so the supply needs
/// no patch, no venue and no database to place a light off its housing. The
/// parse is [`crate::fixtures::definition`]'s, read once per file and memoized
/// by the supply.
pub struct BundledFixtures(pub std::path::PathBuf);

impl FixtureDefinitions for BundledFixtures {
    fn definition(&self, fixture_path: &str) -> Option<luma_scene::definition::Definition> {
        crate::fixtures::definition(&self.0, fixture_path)
            .ok()
            .map(|def| crate::stage_render::definition(&def))
    }
}

/// Read a venue's graph and solve it.
///
/// # Errors
/// Fails if the rows cannot be read, if the catalog cannot be resolved, or if
/// the rows do not form one rooted graph.
pub async fn resolved(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &Path,
) -> Result<ResolvedVenue, String> {
    let graph = graph(access).await?;
    Ok(resolve_graph(&graph, sockets(fixtures_root)?))
}

/// The venue as a top-down text map — the "Gauntlet view".
///
/// Beside [`resolved`] because it is the same two inputs: the graph, and the
/// catalog geometry it resolves against. The map itself is
/// [`luma_scene::venue_tiles`], which is where a piece's measured extent is
/// already read.
///
/// # Errors
/// As [`resolved`].
pub async fn tiles(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &Path,
    options: TileMap,
) -> Result<String, String> {
    let venue = resolved(access, fixtures_root).await?;
    Ok(options.draw(&venue, sockets(fixtures_root)?.catalog()))
}

/// Commit a transaction that changed the graph.
///
/// One call rather than two, because the second half is what gets forgotten:
/// writing the rows tells the derived-group cache the rig moved, and only the
/// commit can tell it the rows are *visible* — a reader that got in between
/// the two refilled it from the graph the commit was about to replace. See
/// [`crate::database::local::venue_graph`] for why it takes both.
///
/// # Errors
/// Fails if the transaction cannot be committed.
pub async fn commit_graph(access: VenueAccess<'_, Write>) -> Result<(), String> {
    access.commit().await?;
    local::venue_graph::rig_changed();
    Ok(())
}

/// Read a venue's graph as the model, without solving it.
///
/// # Errors
/// As [`resolved`].
pub async fn graph(access: &mut impl AuthorizedVenue) -> Result<VenueGraph, String> {
    let venue_id = access.venue_id().to_string();
    graph_of(&venue_id, &local::venue_graph::get_graph(access).await?)
}

/// The rows as a graph; a venue whose root has not synced yet is empty.
fn graph_of(venue_id: &str, rows: &VenueGraphRows) -> Result<VenueGraph, String> {
    if !rows
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Venue.as_str())
    {
        return Ok(VenueGraph::new(Node {
            id: root_id(venue_id),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        }));
    }
    rows.to_graph()
        .ok_or_else(|| "this venue's graph does not have exactly one root".to_string())
}

/// The id of a venue's root node.
#[must_use]
pub fn root_id(venue_id: &str) -> String {
    format!("{venue_id}:venue")
}

#[cfg(test)]
mod tests {
    use super::*;
    use luma_scene::catalog::FIXTURE_CLAMP_SOCKET;
    use luma_scene::venue::{FLOOR_SOCKET, RIG_SOCKET};

    const FIXTURES_ROOT: &str = "resources/fixtures";

    // -----------------------------------------------------------------------
    // The venue as a document: the golden rig, as rows
    // -----------------------------------------------------------------------

    use crate::models::venue_graph::{NodePlacement, VenueConstraint, VenueGraphRows, VenueNode};
    use std::collections::BTreeMap;

    use crate::test_support::repo_root;

    /// The `venue-poses` rig, spelled as rows.
    ///
    /// The same room `luma-scene`'s `venue_poses` golden builds through
    /// `VenueGraph::attach` — deliberately asymmetric, one of every way a node
    /// can be placed and a violated far end. It is written
    /// here as rows because that is the shape a document stores, and
    /// [`rows_reproduce_the_venue_poses_golden`] holds the two
    /// spellings to the same solve, so the copy cannot drift in silence.
    /// The render crate's rig is built without a fixture bundle, so both
    /// copies of it answer every `fixture:*` ref with the same stated housing.
    /// Resolving this one through the shipped bundle instead would hang its
    /// lights by their pivots and disagree with the golden for that reason
    /// alone.
    fn golden_supply() -> VenueSockets {
        VenueSockets::load(
            crate::resources::meshes_root(Some(Path::new(FIXTURES_ROOT))),
            std::sync::Arc::new(luma_scene::catalog::StatedHousing::stock_head()),
        )
        .expect("the catalog resolves")
    }

    fn golden_rows() -> VenueGraphRows {
        let mut nodes = Vec::new();
        let mut params: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();

        // `(my socket, parent, their socket, roll)`.
        type At<'a> = (&'a str, &'a str, &'a str, f64);
        let mut node = |id: &str, kind: &str, catalog_ref: Option<&str>, at: Option<At<'_>>| {
            nodes.push(VenueNode {
                id: id.into(),
                venue_id: "golden".into(),
                kind: kind.into(),
                catalog_ref: catalog_ref.map(Into::into),
                label: (kind == "venue").then(|| "Golden room".into()),
                placement: at.map(|(my, parent, their, roll)| NodePlacement {
                    parent: parent.into(),
                    my_socket: my.into(),
                    their_socket: their.into(),
                    roll,
                }),
            });
        };
        node("venue", "venue", None, None);
        node(
            "deck_a",
            "stage",
            Some("stage_lab/stage_praticavel_2x1x1.glb"),
            Some(("bottom", "venue", FLOOR_SOCKET, 0.4)),
        );
        node(
            "deck_b",
            "stage",
            Some("stage_lab/stage_praticavel_1x1.glb"),
            Some(("edge_left", "deck_a", "edge_right", 0.0)),
        );
        node(
            "tower",
            "tower",
            Some("truss/straight"),
            Some(("end_a", "deck_a", "corner_fl", 0.0)),
        );
        node(
            "corner",
            "piece",
            Some("truss/corner"),
            Some(("face_-x", "tower", "end_b", 0.0)),
        );
        node(
            "run",
            "run",
            Some("truss/straight"),
            Some(("end_a", "corner", "face_-z", 0.0)),
        );
        node(
            "post",
            "tower",
            Some("truss/straight"),
            Some(("end_a", "deck_a", "corner_br", 0.0)),
        );
        node(
            "flown",
            "fixture",
            Some("fixture:flown"),
            Some((FIXTURE_CLAMP_SOCKET, "venue", RIG_SOCKET, 0.0)),
        );
        node(
            "uplight",
            "fixture",
            Some("fixture:uplight"),
            Some((FIXTURE_CLAMP_SOCKET, "venue", FLOOR_SOCKET, 1.1)),
        );
        node(
            "aimed",
            "fixture",
            Some("fixture:aimed"),
            Some((FIXTURE_CLAMP_SOCKET, "venue", RIG_SOCKET, 0.0)),
        );
        node(
            "flown_truss",
            "run",
            Some("truss/straight"),
            Some(("seat", "venue", RIG_SOCKET, 0.0)),
        );
        node(
            "under_truss",
            "fixture",
            Some("fixture:under"),
            Some((FIXTURE_CLAMP_SOCKET, "flown_truss", "face_-y", 0.0)),
        );
        node(
            "wall",
            "array",
            Some("stage_lab/speaker_dbr15.glb"),
            Some(("mount", "venue", FLOOR_SOCKET, 0.0)),
        );

        let mut param = |id: &str, entries: &[(&str, f64)]| {
            params.insert(
                id.into(),
                entries.iter().map(|(k, v)| ((*k).into(), *v)).collect(),
            );
        };
        param("deck_a", &[("u", 1.5), ("v", -2.25), ("trim", 0.0)]);
        param("tower", &[("span", 4.5)]);
        param("run", &[("span", 6.0)]);
        param("post", &[("span", 2.0)]);
        param("flown", &[("u", -1.0), ("v", 3.0), ("trim", 6.5)]);
        param("uplight", &[("u", 2.0), ("v", 0.5)]);
        param("flown_truss", &[("span", 3.0)]);
        param(
            "aimed",
            &[
                ("u", 1.0),
                ("v", -2.0),
                ("trim", 5.0),
                ("pan", 0.6),
                ("tilt", 0.9),
            ],
        );
        param(
            "wall",
            &[("count", 5.0), ("span", 4.0), ("u", 0.0), ("v", 0.0)],
        );

        VenueGraphRows {
            nodes,
            params,
            constraints: vec![VenueConstraint {
                node_id: "run".into(),
                my_socket: "end_b".into(),
                target_node: "tower".into(),
                target_socket: "end_a".into(),
            }],
        }
    }

    /// Six decimals — a micrometre, as `venue_poses` rounds.
    fn round(v: f64) -> f64 {
        (v * 1e6).round() / 1e6
    }

    /// Each fixtures root answers with its own housings: a second root is not
    /// served the first root's supply.
    #[test]
    fn each_fixtures_root_gets_its_own_sockets() {
        use luma_scene::venue::NodeSockets;
        let bundled = crate::resources::source_fixtures_root().expect("the shipped bundle");
        let empty = tempfile::tempdir().expect("a temp dir");
        let mover = Node {
            id: "mover".into(),
            kind: NodeKind::Fixture,
            catalog_ref: Some("Chauvet/Chauvet-Rogue-R2-Spot.qxf".into()),
            label: None,
            params: Params::default(),
        };
        let clamp =
            |root: &Path| sockets(root).expect("the catalog resolves").sockets(&mover)[0].position;

        let real = clamp(&bundled);
        let unknown = clamp(empty.path());
        assert!(
            real.length() > 0.01,
            "a known moving head hangs off its housing's top"
        );
        assert!(
            unknown.length() < 1e-9,
            "a root without the fixture hangs it by its origin, not by another root's housing"
        );
        assert!(std::ptr::eq(
            sockets(&bundled).unwrap(),
            sockets(&bundled).unwrap()
        ));
    }

    /// The rows and the `VenueGraph` the render crate builds by hand are the
    /// same room, so a change to either rig fails here rather than leaving two
    /// goldens describing different venues.
    #[test]
    fn rows_reproduce_the_venue_poses_golden() {
        let solved = crate::models::venue_graph::ResolvedVenue::from(&resolve_graph(
            &golden_rows().to_graph().expect("the rig has a root"),
            &golden_supply(),
        ));
        let golden: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(repo_root().join("harness/goldens/venue-poses.json"))
                .expect("the pose golden is committed"),
        )
        .expect("the pose golden is JSON");

        let poses: Vec<serde_json::Value> = solved
            .nodes
            .iter()
            .map(|node| {
                serde_json::json!({
                    "node": node.id,
                    "kind": node.kind,
                    "catalogRef": node.catalog_ref,
                    "parent": node.parent_id,
                    "arrayIndex": node.array_index,
                    "setPiece": node.set_piece,
                    "position": node.position.map(round),
                    "rotation": node.rotation.map(round),
                    "facing": node.facing.map(round),
                })
            })
            .collect();
        assert_eq!(
            serde_json::Value::Array(poses),
            golden["nodes"],
            "the rows resolve to the poses the render crate's rig pins"
        );
        assert_eq!(
            solved.dangling.len(),
            golden["dangling"].as_array().map_or(0, Vec::len)
        );
        assert_eq!(
            solved
                .constraints
                .iter()
                .map(|c| c.status.as_str())
                .collect::<Vec<_>>(),
            ["violated"],
            "the far end is violated by construction"
        );
    }
}
