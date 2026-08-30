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
use luma_render::venue_tiles::TileMap;
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
    // A third, hung off the same grid but aimed off its rest direction. Both
    // angles are non-zero and neither is a right angle, so a dropped `pan`, a
    // dropped `tilt` and the two swapped all move `facing` here — which is the
    // only line in this golden that says a rest aim reached the pose at all.
    attach(
        &mut graph,
        node(
            "aimed",
            NodeKind::Fixture,
            "fixture:aimed",
            &[
                ("u", 1.0),
                ("v", -2.0),
                ("trim", 5.0),
                ("pan", 0.6),
                ("tilt", 0.9),
            ],
        ),
        Edge {
            parent: "venue".into(),
            my_socket: FIXTURE_CLAMP_SOCKET.into(),
            their_socket: RIG_SOCKET.into(),
            roll: 0.0,
        },
    );

    // A stick flown from the rig plane, with a head on its underside.
    //
    // The rig faces *down*, and a face mate opposes the two normals, so seating
    // a piece on it used to turn the piece over — which quietly made `face_-y`
    // the top of every flown truss and pointed a whole rig at the roof. A piece
    // hangs under a down-facing host instead, so the two lines below are the
    // pin: `flown_truss` keeps its own up, and `under_truss` beams straight
    // down. `beam_of` asserts that second half in world terms, because a golden
    // of nine digits is exactly the thing a reader cannot check by eye.
    attach(
        &mut graph,
        node(
            "flown_truss",
            NodeKind::Run,
            "truss/straight",
            &[("span", 3.0)],
        ),
        Edge {
            parent: "venue".into(),
            my_socket: "seat".into(),
            their_socket: RIG_SOCKET.into(),
            roll: 0.0,
        },
    );
    attach(
        &mut graph,
        node("under_truss", NodeKind::Fixture, "fixture:under", &[]),
        Edge {
            parent: "flown_truss".into(),
            my_socket: FIXTURE_CLAMP_SOCKET.into(),
            their_socket: "face_-y".into(),
            roll: 0.0,
        },
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
    graph.insert_placed(
        node(
            "tray_on_tray",
            NodeKind::Piece,
            "stage_lab/speaker_dbr15.glb",
            &[],
        ),
        Edge {
            parent: "tray_speaker".into(),
            my_socket: "mount".into(),
            their_socket: "mount".into(),
            roll: 0.0,
        },
    );

    // A far end: the run's open end checked against the tower it started from.
    // Violated by construction — the run goes nowhere near it — which is the
    // status a golden most needs to pin, because "satisfied" is also what a
    // check that never ran looks like.
    graph
        .constrain(
            luma_scene::venue::Constraint {
                node: "run".into(),
                my_socket: "end_b".into(),
                target_node: "tower".into(),
                target_socket: "end_a".into(),
            },
            catalog(),
        )
        .expect("both truss ends exist and mate");

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

/// A head on the underside of a flown stick points at the floor.
///
/// The claim the golden's nine digits make but cannot state: `face_-y` is the
/// piece's underside wherever the piece ended up, so a row hung there beams
/// straight down whether the stick stands on a deck or hangs from the grid.
/// Read in world terms — data space `-Z` — rather than as a face name, because
/// naming the face is the very thing that was wrong when a flown piece came out
/// upside down.
#[test]
fn a_head_under_a_flown_stick_beams_down() {
    let solved = resolve(&venue(), catalog());
    let beam = |id: &str| {
        let (_, basis) = solved
            .pose(id)
            .unwrap_or_else(|| panic!("{id} is in the room"))
            .data_basis();
        basis * glam::DVec3::NEG_Z
    };
    assert!(
        beam("under_truss").abs_diff_eq(glam::DVec3::NEG_Z, 1e-9),
        "a head on the underside of a flown stick beams {:?}, not straight down",
        beam("under_truss")
    );
    // And the flown stick itself kept its own up: not asserted through the
    // fixture, because a beam that happened to be right off an upside-down
    // truss would be two errors cancelling.
    let (_, truss) = solved.pose("flown_truss").unwrap().data_basis();
    assert!(
        (truss * glam::DVec3::Z).dot(glam::DVec3::Z) > 0.999,
        "the flown stick was turned over: up is {:?}",
        truss * glam::DVec3::Z
    );
}

// ---------------------------------------------------------------------------
// the Gauntlet view of the same room
// ---------------------------------------------------------------------------

/// The tile map of this rig, at the default cell and at a coarse one.
///
/// Captured from **this** graph rather than a second hand-built one: the map is
/// a projection of the solve, so a rig that already pins every way a node can
/// be placed pins every way one can be drawn. Lines rather than one string
/// because a JSON string with escaped newlines is not a diff anybody can read,
/// and localising a one-piece move to one row is the whole claim.
fn tiles_golden() -> String {
    let solved = resolve(&venue(), catalog());
    let bounds = catalog().0.catalog();
    let map = |cell_m: f64| {
        let text = TileMap {
            cell_m,
            ..TileMap::default()
        }
        .draw(&solved, bounds);
        json!({
            "cellM": cell_m,
            "map": text.lines().collect::<Vec<_>>(),
        })
    };
    let mut out = serde_json::to_string_pretty(&json!({
        "venue-poses": map(0.5),
        "venue-poses-coarse": map(1.0),
    }))
    .expect("the capture serializes");
    out.push('\n');
    out
}

#[test]
fn venue_tiles_golden_is_current() {
    let path = repo_root().join("harness/goldens/venue-tiles.json");
    assert!(
        !write_if_changed(path, &tiles_golden()),
        "the tile-map golden was stale and has been rewritten — review and commit it"
    );
}

/// The map is a pure function of the solve, so two draws are one string.
#[test]
fn the_tile_map_is_byte_stable() {
    assert_eq!(tiles_golden(), tiles_golden());
}

/// Half-metre cells means a metre is two cells — the property that makes a
/// diff localisable, measured by moving a piece rather than restated as a
/// constant.
///
/// Measured *against a fixture that does not move*, not against the left edge
/// of the map: the map is sized to its contents, so a piece that moves also
/// moves the frame around it, and only the separation between two things in
/// the room is the rig's own geometry.
#[test]
fn a_metre_of_truss_is_two_cells() {
    let separation = |u: f64| {
        let mut graph = VenueGraph::new(Node {
            id: "venue".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        });
        let mut place = |node: Node, socket: &str| {
            let id = node.id.clone();
            graph.insert(node);
            graph
                .attach(
                    &id,
                    Edge {
                        parent: "venue".into(),
                        my_socket: socket.into(),
                        their_socket: FLOOR_SOCKET.into(),
                        roll: 0.0,
                    },
                    catalog(),
                )
                .unwrap_or_else(|e| panic!("{id}: {e}"));
        };
        place(
            node(
                "mark",
                NodeKind::Fixture,
                "fixture:mark",
                &[("u", -6.0), ("v", 0.0)],
            ),
            FIXTURE_CLAMP_SOCKET,
        );
        place(
            node(
                "stick",
                NodeKind::Run,
                "truss/straight",
                &[("u", u), ("v", 0.0), ("span", 2.0)],
            ),
            "seat",
        );
        let text = TileMap::default().draw(&resolve(&graph, catalog()), catalog().0.catalog());
        // The legend names every glyph, so the search starts below it: three
        // header lines, a blank, and the two ruler lines.
        // Columns are a grid, so two glyphs are comparable whatever rows they
        // are on. The search starts below the legend, which names them both.
        let column = |glyph: char| {
            text.lines()
                .skip(6)
                .find_map(|line| line.find(glyph))
                .unwrap_or_else(|| panic!("{glyph} is not on the map:\n{text}"))
        };
        column('\u{b7}') - column('\u{2550}')
    };

    // Columns run +x to -x, so moving the stick +1 m along x walks it two cells
    // *away* from a mark that is further -x than it is.
    assert_eq!(
        separation(1.0) - separation(0.0),
        2,
        "a metre is two half-metre cells"
    );
}
