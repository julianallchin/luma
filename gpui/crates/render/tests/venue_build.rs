//! The authoring layer against real geometry.
//!
//! [`luma_scene::build`] compiles intent — a direction, a length, a hinge axis
//! — into the relations the resolver already admits. Its own unit tests pin the
//! arithmetic; this pins the part that only exists once a catalog is loaded: a
//! truss end frame comes from the generator and a deck's corners come from a
//! measured GLB, so "does `direction=(0, 0, 1)` build a tower" cannot be
//! answered without both.
//!
//! Run `cargo test -p luma-render --test venue_build`.

use std::path::{Path, PathBuf};

use glam::DVec3;
use luma_render::catalog::VenueSockets;
use luma_scene::build::{compile, Refusal, Request, Scene, Tip};
use luma_scene::venue::{resolve, Node, NodeKind, Params, ResolvedVenue, VenueGraph};

fn meshes_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes")
}

fn supply() -> VenueSockets {
    VenueSockets::load(meshes_root()).expect("the catalog resolves")
}

/// A venue holding nothing but its root, plus a counter for minted ids.
struct Room {
    graph: VenueGraph,
    solved: ResolvedVenue,
    sockets: VenueSockets,
    next: usize,
}

impl Room {
    fn new() -> Room {
        let sockets = supply();
        let graph = VenueGraph::new(Node {
            id: "venue".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        });
        let solved = resolve(&graph, &sockets);
        Room {
            graph,
            solved,
            sockets,
            next: 0,
        }
    }

    fn scene(&self) -> Scene<'_, VenueSockets> {
        Scene::new(&self.graph, &self.solved, &self.sockets)
    }

    /// Compile and write one request, answering with the node id and its tip.
    fn build(&mut self, request: Request) -> Result<(String, Option<Tip>), Refusal> {
        let plan = compile(&self.scene(), &request)?;
        self.next += 1;
        let id = format!("n{}", self.next);
        plan.apply(&mut self.graph, &id, &self.sockets)
            .expect("a plan compiled against this graph is admissible");
        self.solved = resolve(&self.graph, &self.sockets);
        let tip = plan.tip_at(&id);
        Ok((id, tip))
    }

    fn expect(&mut self, request: Request) -> (String, Option<Tip>) {
        match self.build(request) {
            Ok(out) => out,
            Err(refusal) => panic!("refused: {refusal}"),
        }
    }
}

fn truss(length: f64, direction: [f64; 3]) -> Request {
    Request {
        piece: "truss".into(),
        length: Some(length),
        direction: Some(direction),
        ..Request::default()
    }
}

fn close(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() < 0.02, "{what}: got {got}, want {want}");
}

// ---------------------------------------------------------------------------

/// The claim contract sentence 1 makes: a free `place` anchors by the piece's
/// **footprint centre**, not by whatever socket it happens to stand on.
#[test]
fn a_free_place_anchors_by_the_footprint_centre() {
    let mut room = Room::new();
    let mut request = truss(8.0, [0.0, 0.0, 1.0]);
    request.at = Some([-5.5, 5.0]);
    let (id, _) = room.expect(request);
    let print = room
        .scene()
        .footprint(&id)
        .expect("a placed truss has a box");
    close(print.at[0], -5.5, "u");
    close(print.at[1], 5.0, "v");
    // Standing on end, so it is eight metres tall and its centre is at four.
    close(print.size[2], 8.0, "height");
    close(print.z, 4.0, "centre height");
    assert!(print.size[0] < 0.5 && print.size[1] < 0.5, "{print:?}");
}

/// The same piece, two verbs: laid flat it runs along the axis it was pointed
/// down, and the node is filed as a run rather than a tower.
#[test]
fn direction_chooses_the_axis_a_free_stick_runs_along() {
    let mut room = Room::new();
    let mut request = truss(6.0, [1.0, 0.0, 0.0]);
    request.at = Some([0.0, -2.0]);
    let (id, _) = room.expect(request);
    let print = room.scene().footprint(&id).expect("placed");
    close(print.size[0], 6.0, "run along u");
    assert!(print.size[1] < 0.5, "{print:?}");
    assert_eq!(room.graph.node(&id).map(|n| n.kind), Some(NodeKind::Run));
}

/// A tower is the same generator stood on end — and `at=` still means the plan
/// centre, so the two towers of a portal are exactly `width` apart.
#[test]
fn two_towers_stand_the_width_they_were_asked_for() {
    let mut room = Room::new();
    let mut left = truss(8.0, [0.0, 0.0, 1.0]);
    left.at = Some([-5.5, 0.0]);
    let mut right = truss(8.0, [0.0, 0.0, 1.0]);
    right.at = Some([5.5, 0.0]);
    let (a, _) = room.expect(left);
    let (b, _) = room.expect(right);
    let scene = room.scene();
    let span = scene.extent([a.as_str(), b.as_str()]).expect("two towers");
    close(span.centre[0], 0.0, "centred on the room");
    close(span.size[0], 11.0 + span.size[1], "outer width");
    assert_eq!(span.count, 2);
}

/// The chain grammar, whole: tower, corner, beam, corner, tower. The corner's
/// exit face is chosen by the *next* piece's direction.
#[test]
fn a_portal_closes_where_it_was_asked_to() {
    let mut room = Room::new();
    let mut base = truss(8.0, [0.0, 0.0, 1.0]);
    base.at = Some([-5.5, 0.0]);
    let (_, tip) = room.expect(base);

    let (_, tip) = room.expect(Request {
        piece: "corner".into(),
        from: tip,
        ..Request::default()
    });
    let (beam, tip) = room.expect(Request {
        piece: "truss".into(),
        from: tip,
        length: Some(11.0),
        direction: Some([1.0, 0.0, 0.0]),
        ..Request::default()
    });
    let (_, tip) = room.expect(Request {
        piece: "corner".into(),
        from: tip,
        ..Request::default()
    });
    let (leg, _) = room.expect(Request {
        piece: "truss".into(),
        from: tip,
        length: Some(8.0),
        direction: Some([0.0, 0.0, -1.0]),
        ..Request::default()
    });

    let scene = room.scene();
    let beam_print = scene.footprint(&beam).expect("the beam is placed");
    close(beam_print.size[0], 11.0, "beam run");
    // The far leg comes back **down**, which is the whole of what the second
    // corner's exit direction bought.
    let leg_print = scene.footprint(&leg).expect("the leg is placed");
    close(leg_print.size[2], 8.0, "leg height");
    close(leg_print.z, 4.0, "leg centre height");
    // The two legs stand 11 m apart plus the two corner blocks between them.
    let extent = scene
        .extent(room.graph.nodes().map(|n| n.id.as_str()))
        .expect("a portal");
    assert!(
        extent.size[0] > 11.0 && extent.size[0] < 12.0,
        "portal width {:?}",
        extent.size
    );
    close(extent.size[2], 8.0 + 2.0 * 0.17, "portal height");
}

/// Contract sentence 3, measured: a positive hinge angle turns the run
/// counterclockwise about `+axis`. Stage right, hinged `+30` about up, leaves
/// toward the crowd.
#[test]
fn a_positive_hinge_turns_counterclockwise_about_its_axis() {
    let out = hinged(30.0);
    assert!(out.x > 0.0 && out.y > 0.0, "left toward {out:?}");
    close(out.x, 30f64.to_radians().cos(), "u component");
    close(out.y, 30f64.to_radians().sin(), "v component");

    let back = hinged(-30.0);
    close(back.x, out.x, "mirrored u");
    close(back.y, -out.y, "mirrored v");
}

/// The direction a guardrail chain leaves in after one hinge of `angle` about
/// world up.
fn hinged(angle: f64) -> DVec3 {
    let mut room = Room::new();
    let mut run = truss(4.0, [1.0, 0.0, 0.0]);
    run.at = Some([0.0, 0.0]);
    let (_, tip) = room.expect(run);
    let (_, tip) = room.expect(Request {
        piece: "hinge".into(),
        from: tip,
        axis: Some([0.0, 0.0, 1.0]),
        angle: Some(angle),
        ..Request::default()
    });
    DVec3::from(tip.expect("a hinge leaves one leaf free").direction)
}

/// Off-module intent is built, not refused, and the announcement says what
/// happened.
#[test]
fn an_off_module_length_snaps_and_says_so() {
    let mut room = Room::new();
    let mut request = truss(7.2, [1.0, 0.0, 0.0]);
    request.at = Some([0.0, 0.0]);
    let plan = compile(&room.scene(), &request).expect("a snapped length still builds");
    assert!(
        plan.announce.iter().any(|line| line.contains("7.00")),
        "{:?}",
        plan.announce
    );
    close(plan.params["span"], 7.0, "built span");
    let _ = room.build(request);
}

/// A turn no joint here makes is refused with the turns it does make.
#[test]
fn an_impossible_turn_lists_the_legal_ones() {
    let mut room = Room::new();
    let mut run = truss(4.0, [1.0, 0.0, 0.0]);
    run.at = Some([0.0, 0.0]);
    let (_, tip) = room.expect(run);
    let refusal = room
        .build(Request {
            piece: "truss".into(),
            from: tip,
            length: Some(4.0),
            direction: Some([0.0, 1.0, 0.0]),
            ..Request::default()
        })
        .expect_err("a stick cannot turn out of a straight end");
    match refusal {
        Refusal::ImpossibleTurn { legal, .. } => {
            assert_eq!(legal.len(), 1);
            close(legal[0][0], 1.0, "the one way out");
        }
        other => panic!("wrong refusal: {other}"),
    }
}

/// A hinge axis in the plane of the run is refused, naming the plane that works.
#[test]
fn a_hinge_axis_along_the_run_is_refused() {
    let mut room = Room::new();
    let mut run = truss(4.0, [1.0, 0.0, 0.0]);
    run.at = Some([0.0, 0.0]);
    let (_, tip) = room.expect(run);
    let refusal = room
        .build(Request {
            piece: "hinge".into(),
            from: tip,
            axis: Some([1.0, 0.0, 0.0]),
            angle: Some(30.0),
            ..Request::default()
        })
        .expect_err("a hinge cannot turn about the run it is on");
    match refusal {
        Refusal::BadAxis { plane, .. } => assert_eq!(plane.len(), 4),
        other => panic!("wrong refusal: {other}"),
    }
}

/// A chain that comes back through itself is refused, naming what is in the way.
#[test]
fn a_collision_names_the_blocker() {
    let mut room = Room::new();
    let mut first = truss(6.0, [1.0, 0.0, 0.0]);
    first.at = Some([0.0, 0.0]);
    first.label = Some("downstage beam".into());
    room.expect(first);

    let mut through = truss(6.0, [1.0, 0.0, 0.0]);
    through.at = Some([1.0, 0.0]);
    let refusal = room
        .build(through)
        .expect_err("two sticks cannot share the same metre of air");
    match refusal {
        Refusal::Collision { label, .. } => {
            assert_eq!(label.as_deref(), Some("downstage beam"));
        }
        other => panic!("wrong refusal: {other}"),
    }
}

/// A name the catalog does not have is refused with names it does.
#[test]
fn an_unknown_piece_suggests_the_real_ones() {
    let room = Room::new();
    let refusal = compile(
        &room.scene(),
        &Request {
            piece: "trussss".into(),
            ..Request::default()
        },
    )
    .expect_err("no such piece");
    assert!(matches!(refusal, Refusal::UnknownPiece { .. }), "{refusal}");
}

/// A tip is a direction, not a name — and it is the *free* end, never the one
/// the piece is already bolted by.
#[test]
fn a_tip_is_the_free_end_as_a_vector() {
    let mut room = Room::new();
    let mut run = truss(4.0, [1.0, 0.0, 0.0]);
    run.at = Some([0.0, 0.0]);
    let (id, tip) = room.expect(run);
    let tip = tip.expect("a stick on the floor has both ends free, one of them upstream");
    close(
        DVec3::from(tip.direction).x,
        1.0,
        "the far end faces along u",
    );
    // The same answer through the query side, asked by direction.
    let scene = room.scene();
    let found = scene
        .tip(&id, Some(DVec3::X))
        .expect("the end facing stage right");
    close(DVec3::from(found.direction).x, 1.0, "queried direction");
}
