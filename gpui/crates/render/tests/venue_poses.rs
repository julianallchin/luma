//! The venue graph's golden family: per-node resolved world poses.
//!
//! `docs/design/venue-graph.md`, Goldens — "per-node resolved world poses
//! captured at the end of phase 3 as the migration's proof". A render golden
//! answers "does the room look right" at a few hundred kilobytes a scene and a
//! GPU; this answers "is every piece where the graph says" in a diff a human
//! can read.
//!
//! It is captured **here** rather than in `luma-scene` because a resolved pose
//! is the resolver *and* the geometry: a truss end frame comes from the
//! generator and a deck's corners come from a measured GLB, so a golden that
//! stubbed either would pin half the answer. That makes this the one place the
//! whole chain — catalog, generator, sockets, solve — is pinned numerically.
//!
//! Run `cargo test -p luma-render --test venue_poses`. It rewrites the golden
//! and then fails if it changed, so a stale capture cannot be committed
//! silently.

use std::path::{Path, PathBuf};

use luma_render::catalog::{fixture_clamp, VenueSockets, FIXTURE_CLAMP_SOCKET};
use luma_scene::venue::{
    resolve, ConstraintStatus, Edge, Node, NodeKind, Params, VenueGraph, FLOOR_SOCKET, RIG_SOCKET,
};
use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn node(id: &str, kind: NodeKind, catalog_ref: &str, params: &[(&str, f64)]) -> Node {
    let mut p = Params::default();
    for (key, value) in params {
        p.set(*key, *value);
    }
    Node {
        id: id.into(),
        kind,
        catalog_ref: Some(catalog_ref.into()),
        label: None,
        params: p,
    }
}

/// One venue exercising every way a node can be placed.
///
/// Deliberately asymmetric and off-axis: a rig sitting at the origin with every
/// yaw at zero would let a sign error, an axis swap or a dropped `trim` through
/// unnoticed, which is the class of bug this family exists to catch.
fn venue() -> VenueGraph {
    let sockets = catalog();
    let mut graph = VenueGraph::new(Node {
        id: "venue".into(),
        kind: NodeKind::Venue,
        catalog_ref: None,
        label: Some("Golden room".into()),
        params: Params::default(),
    });

    let attach = |graph: &mut VenueGraph, node: Node, edge: Edge| {
        let id = node.id.clone();
        graph.insert(node);
        graph
            .attach(&id, edge, sockets)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
    };
    let on_floor = |my_socket: &str, yaw: f64| Edge {
        parent: "venue".into(),
        my_socket: my_socket.into(),
        their_socket: FLOOR_SOCKET.into(),
        roll: yaw,
    };

    // A deck on the floor, yawed off-axis, with a second deck butted onto its
    // right edge — the edge-mode joint, which is the one that used to tie an
    // upside-down pose with the correct one.
    attach(
        &mut graph,
        node(
            "deck_a",
            NodeKind::Stage,
            "stage_lab/stage_praticavel_2x1x1.glb",
            &[("u", 1.5), ("v", -2.25), ("trim", 0.0)],
        ),
        on_floor("bottom", 0.4),
    );
    attach(
        &mut graph,
        node(
            "deck_b",
            NodeKind::Stage,
            "stage_lab/stage_praticavel_1x1.glb",
            &[],
        ),
        Edge {
            parent: "deck_a".into(),
            my_socket: "edge_left".into(),
            their_socket: "edge_right".into(),
            roll: 0.0,
        },
    );

    // A generated truss standing on a deck corner, at a span the palette does
    // not default to — so a node whose params are ignored moves the golden.
    attach(
        &mut graph,
        node("tower", NodeKind::Tower, "truss/straight", &[("span", 4.5)]),
        Edge {
            parent: "deck_a".into(),
            my_socket: "end_a".into(),
            their_socket: "corner_fl".into(),
            roll: 0.0,
        },
    );
    // A corner block on top of it, then a run out of the corner.
    attach(
        &mut graph,
        node("corner", NodeKind::Piece, "truss/corner", &[]),
        Edge {
            parent: "tower".into(),
            my_socket: "face_-x".into(),
            their_socket: "end_b".into(),
            roll: 0.0,
        },
    );
    attach(
        &mut graph,
        node("run", NodeKind::Run, "truss/straight", &[("span", 6.0)]),
        Edge {
            parent: "corner".into(),
            my_socket: "end_a".into(),
            their_socket: "face_-z".into(),
            roll: 0.0,
        },
    );

    // A second stick standing on the far corner, bolted at one end and open at
    // the other. Every other truss end in this room is claimed by a joint or by
    // the far-end check below, so without it the golden pins no open
    // `TrussEnd` at all — the one thing `dangling()` exists to report.
    attach(
        &mut graph,
        node("post", NodeKind::Tower, "truss/straight", &[("span", 2.0)]),
        Edge {
            parent: "deck_a".into(),
            my_socket: "end_a".into(),
            their_socket: "corner_br".into(),
            roll: 0.0,
        },
    );

    // A fixture on the grid — flown, beam down — and one on the floor, beam up.
    attach(
        &mut graph,
        node(
            "flown",
            NodeKind::Fixture,
            "fixture:flown",
            &[("u", -1.0), ("v", 3.0), ("trim", 6.5)],
        ),
        Edge {
            parent: "venue".into(),
            my_socket: FIXTURE_CLAMP_SOCKET.into(),
            their_socket: RIG_SOCKET.into(),
            roll: 0.0,
        },
    );
    attach(
        &mut graph,
        node(
            "uplight",
            NodeKind::Fixture,
            "fixture:uplight",
            &[("u", 2.0), ("v", 0.5)],
        ),
        on_floor(FIXTURE_CLAMP_SOCKET, 1.1),
    );

    // An array of five speakers spread over four metres of the deck top.
    attach(
        &mut graph,
        node(
            "wall",
            NodeKind::Array,
            "stage_lab/speaker_dbr15.glb",
            &[("count", 5.0), ("span", 4.0), ("u", 0.0), ("v", 0.0)],
        ),
        on_floor("mount", 0.0),
    );

    // A speaker nobody has placed, with a second one stacked on it: rows in the
    // venue with no path to the root, which the solve reports rather than
    // drops. The pair is what pins that only the *root* of an unplaced branch
    // is listed, with the size of the branch alongside.
    graph.insert(node(
        "tray_speaker",
        NodeKind::Piece,
        "stage_lab/speaker_dbr15.glb",
        &[],
    ));
    graph.insert_edge(
        "tray_on_tray",
        Edge {
            parent: "tray_speaker".into(),
            my_socket: "mount".into(),
            their_socket: "mount".into(),
            roll: 0.0,
        },
    );
    graph.insert(node(
        "tray_on_tray",
        NodeKind::Piece,
        "stage_lab/speaker_dbr15.glb",
        &[],
    ));

    // A far end: the run's open end checked against the tower it started from.
    // Violated by construction — the run goes nowhere near it — which is the
    // status a golden most needs to pin, because "satisfied" is also what a
    // check that never ran looks like.
    graph.constrain(luma_scene::venue::Constraint {
        node: "run".into(),
        my_socket: "end_b".into(),
        target_node: "tower".into(),
        target_socket: "end_a".into(),
    });

    graph
}

/// A fixture's `catalog_ref` is a patch-row id, not a catalog piece, so the
/// socket supply has to answer for it. In the app that is `VenueSockets`; here
/// the two golden fixtures get the same one clamp it would give them.
struct Sockets(VenueSockets);

impl luma_scene::venue::NodeSockets for Sockets {
    fn is_known(&self, node: &Node) -> bool {
        luma_scene::venue::NodeSockets::is_known(&self.0, node)
    }

    fn sockets(&self, node: &Node) -> Vec<luma_scene::sockets::ResolvedSocket> {
        if node.kind == NodeKind::Fixture {
            return vec![fixture_clamp()];
        }
        self.0.sockets(node)
    }
}

fn catalog() -> &'static Sockets {
    static SOCKETS: std::sync::OnceLock<Sockets> = std::sync::OnceLock::new();
    SOCKETS.get_or_init(|| {
        Sockets(
            VenueSockets::load(repo_root().join("resources/meshes"))
                .expect("the catalog resolves against the shipped meshes"),
        )
    })
}

/// Six decimals — a micrometre. The solve is `f64` and deterministic, so this
/// is not a tolerance, it is how many digits are worth reading in a diff.
fn round(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn golden() -> String {
    let graph = venue();
    let solved = resolve(&graph, catalog());

    let nodes: Vec<Value> = solved
        .poses()
        .map(|pose| {
            let (position, rotation) = pose.data_pose();
            let (_, basis) = pose.data_basis();
            let facing = basis * glam::DVec3::NEG_Z;
            json!({
                "node": pose.node,
                "kind": pose.kind.as_str(),
                "catalogRef": pose.catalog_ref,
                "parent": pose.parent,
                "arrayIndex": pose.array_index,
                // The one answer three consumers draw from: the room, the
                // lights and the array's anchor are frames, not objects.
                "setPiece": pose.is_set_piece(),
                // Data space, Z-up: the convention every consumer reads.
                "position": position.map(round),
                "rotation": rotation.map(round),
                "facing": [round(facing.x), round(facing.y), round(facing.z)],
            })
        })
        .collect();

    let constraints: Vec<Value> = solved
        .constraints()
        .iter()
        .map(|c| {
            json!({
                "node": c.node,
                "mySocket": c.my_socket,
                "targetNode": c.target_node,
                "targetSocket": c.target_socket,
                "status": match c.status {
                    ConstraintStatus::Satisfied => "satisfied",
                    ConstraintStatus::Violated { .. } => "violated",
                    ConstraintStatus::Dangling => "dangling",
                },
                "gapM": match c.status {
                    ConstraintStatus::Violated { gap_m } => Some(round(gap_m)),
                    _ => None,
                },
            })
        })
        .collect();

    let dangling: Vec<Value> = solved
        .dangling()
        .iter()
        .map(|d| json!({ "node": d.node, "socket": d.socket, "type": d.socket_type.as_str() }))
        .collect();

    let unplaced: Vec<Value> = solved
        .unplaced()
        .iter()
        .map(|u| {
            json!({
                "node": u.node,
                "kind": u.kind.as_str(),
                "descendants": u.descendants,
            })
        })
        .collect();

    let mut out = serde_json::to_string_pretty(&json!({
        "nodes": nodes,
        "constraints": constraints,
        "dangling": dangling,
        "unplaced": unplaced,
        "warnings": solved.warnings().len(),
    }))
    .expect("the capture serializes");
    out.push('\n');
    out
}

fn write_if_changed(path: PathBuf, contents: &str) -> bool {
    let same = std::fs::read_to_string(&path).is_ok_and(|old| old == contents);
    if !same {
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
    !same
}

#[test]
fn venue_poses_golden_is_current() {
    let path = repo_root().join("harness/goldens/venue-poses.json");
    assert!(
        !write_if_changed(path, &golden()),
        "the venue-pose golden was stale and has been rewritten — review and commit it"
    );
}

/// The determinism the golden depends on, asserted separately so a flaky solve
/// reads as "the solve is not deterministic" rather than as a stale capture.
#[test]
fn two_solves_are_byte_identical() {
    assert_eq!(golden(), golden());
}
