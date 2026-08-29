//! The addressing rule, measured.
//!
//! Every venue here is built from a socket table rather than a catalog: the
//! rule is about *order along a run*, and a GLB would only add a way for the
//! test to fail for a reason that is not the rule.

use std::collections::HashMap;

use glam::DVec3;

use super::*;
use crate::sockets::{ResolvedSocket, SocketMode, SocketType};
use crate::venue::{resolve, Edge, Node, NodeSockets, Params, VenueGraph};

// ---------------------------------------------------------------------------
// A rig, built
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Table(HashMap<String, Vec<ResolvedSocket>>);

impl NodeSockets for Table {
    fn sockets(&self, node: &Node) -> Vec<ResolvedSocket> {
        node.catalog_ref
            .as_ref()
            .and_then(|id| self.0.get(id))
            .cloned()
            .unwrap_or_default()
    }
}

fn socket(name: &str, ty: SocketType, position: DVec3, normal: DVec3) -> ResolvedSocket {
    ResolvedSocket {
        name: name.to_string(),
        socket_type: ty,
        position,
        normal,
        tangent: if normal.dot(DVec3::Y).abs() > 0.9 {
            DVec3::X
        } else {
            DVec3::Y.cross(normal).normalize()
        },
        mode: SocketMode::Face,
        outward: normal,
        roll: ty.roll(),
    }
}

/// The catalog this file rigs against: a floor-standing truss whose span lies
/// on local `+X` (the family's convention), and a fixture with one clamp.
fn table() -> Table {
    let mut table = Table::default();
    table.0.insert(
        "truss".into(),
        vec![
            socket(
                "base",
                SocketType::BottomMount,
                DVec3::new(0.0, -0.15, 0.0),
                DVec3::NEG_Y,
            ),
            socket(
                "under",
                SocketType::FloorTop,
                DVec3::new(0.0, -0.15, 0.0),
                DVec3::NEG_Y,
            ),
        ],
    );
    table.0.insert(
        "mover".into(),
        vec![socket(
            "clamp",
            SocketType::EquipmentMount,
            DVec3::new(0.0, 0.1, 0.0),
            DVec3::Y,
        )],
    );
    table
}

/// A rig under construction: runs on the floor, fixtures under the runs.
struct Rig {
    graph: VenueGraph,
    fixtures: Vec<Fixture>,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            graph: VenueGraph::new(Node {
                id: "venue".into(),
                kind: NodeKind::Venue,
                catalog_ref: None,
                label: None,
                params: Params::default(),
            }),
            fixtures: Vec::new(),
        }
    }

    fn run(mut self, id: &str) -> Rig {
        self.graph.insert(Node {
            id: id.into(),
            kind: NodeKind::Run,
            catalog_ref: Some("truss".into()),
            label: None,
            params: Params::default(),
        });
        self.graph.insert_edge(
            id,
            Edge {
                parent: "venue".into(),
                my_socket: "base".into(),
                their_socket: crate::venue::FLOOR_SOCKET.into(),
                roll: 0.0,
            },
        );
        self
    }

    /// One fixture hung under `run`, `along` metres along its span.
    fn fixture(mut self, id: &str, run: &str, along: f64, channels: u16) -> Rig {
        let mut params = Params::default();
        params.set("u", along);
        self.graph.insert(Node {
            id: id.into(),
            kind: NodeKind::Fixture,
            catalog_ref: Some("mover".into()),
            label: None,
            params,
        });
        self.graph.insert_edge(
            id,
            Edge {
                parent: run.into(),
                my_socket: "clamp".into(),
                their_socket: "under".into(),
                roll: 0.0,
            },
        );
        self.fixtures.push(Fixture {
            id: id.into(),
            channels,
            address: Address::Unset,
        });
        self
    }

    /// A fixture nobody has dragged out of the tray.
    fn unplaced(mut self, id: &str, channels: u16) -> Rig {
        self.fixtures.push(Fixture {
            id: id.into(),
            channels,
            address: Address::Unset,
        });
        self
    }

    /// Set a fixture's address by hand, as the patch page does.
    fn pin(self, id: &str, universe: u16, address: u16) -> Rig {
        self.address(id, universe, address, Address::Pinned)
    }

    /// The address the row already carries, nobody having typed it — what an
    /// earlier auto-patch, or a one-at-a-time add, left behind.
    fn stored(self, id: &str, universe: u16, address: u16) -> Rig {
        self.address(id, universe, address, Address::Derived)
    }

    fn address(
        mut self,
        id: &str,
        universe: u16,
        address: u16,
        how: fn(Footprint) -> Address,
    ) -> Rig {
        let fixture = self
            .fixtures
            .iter_mut()
            .find(|f| f.id == id)
            .expect("the address names a fixture the rig has");
        let footprint = Footprint::new(universe, address, fixture.channels)
            .expect("the address has to be addressable");
        fixture.address = how(footprint);
        self
    }

    /// Move a placed fixture along its run.
    fn slide(mut self, id: &str, along: f64) -> Rig {
        let mut moved = self
            .graph
            .node(id)
            .expect("slide names a placed node")
            .clone();
        moved.params.set("u", along);
        self.graph.insert(moved);
        self
    }

    /// A corner block bolted to the end of `parent`, and nothing else. It is a
    /// piece, not a run: the thing the doc's "one universe per structure" claim
    /// hangs on.
    fn corner(mut self, id: &str, parent: &str, along: f64) -> Rig {
        self.piece(id, NodeKind::Piece, parent, along);
        self
    }

    /// A run bolted to a piece rather than to the floor — the far side of a
    /// corner.
    fn run_from(mut self, id: &str, parent: &str, along: f64) -> Rig {
        self.piece(id, NodeKind::Run, parent, along);
        self
    }

    fn piece(&mut self, id: &str, kind: NodeKind, parent: &str, along: f64) {
        let mut params = Params::default();
        params.set("u", along);
        self.graph.insert(Node {
            id: id.into(),
            kind,
            catalog_ref: Some("truss".into()),
            label: None,
            params,
        });
        self.graph.insert_edge(
            id,
            Edge {
                parent: parent.into(),
                my_socket: "base".into(),
                their_socket: "under".into(),
                roll: 0.0,
            },
        );
    }

    fn allocate(&self) -> Allocation {
        let sockets = table();
        super::allocate(&resolve(&self.graph, &sockets), &self.fixtures)
    }

    /// The same rig with its fixture list rotated — a different input order,
    /// the same venue.
    fn reordered(&self, by: usize) -> Allocation {
        let sockets = table();
        let mut fixtures = self.fixtures.clone();
        let step = by % fixtures.len().max(1);
        fixtures.rotate_left(step);
        super::allocate(&resolve(&self.graph, &sockets), &fixtures)
    }

    fn next(&self, run: Option<&str>, channels: u16, count: usize) -> Vec<Footprint> {
        let sockets = table();
        super::next_addresses(
            &resolve(&self.graph, &sockets),
            &self.fixtures,
            run,
            channels,
            count,
        )
    }
}

// ---------------------------------------------------------------------------
// Reading an allocation
// ---------------------------------------------------------------------------

/// The fixtures of one universe in address order — what the rig sheet reads.
fn order_in(allocation: &Allocation, universe: u16) -> Vec<&str> {
    let mut rows: Vec<&Assignment> = allocation
        .assignments
        .iter()
        .filter(|a| a.footprint.universe() == universe)
        .collect();
    rows.sort_by_key(|a| a.footprint.address());
    rows.iter().map(|a| a.fixture.as_str()).collect()
}

fn at(allocation: &Allocation, fixture: &str) -> Footprint {
    allocation
        .get(fixture)
        .unwrap_or_else(|| panic!("{fixture} was allocated"))
        .footprint
}

/// The invariant every case shares: no two fixtures claim a channel.
fn assert_disjoint(allocation: &Allocation) {
    let claims = &allocation.assignments;
    for (i, a) in claims.iter().enumerate() {
        for b in &claims[i + 1..] {
            assert!(
                !a.footprint.overlaps(&b.footprint),
                "{} {:?} overlaps {} {:?}",
                a.fixture,
                a.footprint,
                b.fixture,
                b.footprint
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Physical order along the run
// ---------------------------------------------------------------------------

#[test]
fn addresses_follow_position_along_the_run_not_the_order_fixtures_were_added() {
    // Added back-to-front on purpose: creation order is the answer this rule
    // exists to *not* give.
    let rig = Rig::new()
        .run("truss")
        .fixture("d", "truss", 3.0, 8)
        .fixture("b", "truss", 1.0, 8)
        .fixture("a", "truss", 0.0, 8)
        .fixture("c", "truss", 2.0, 8);
    let allocation = rig.allocate();

    let universe = at(&allocation, "a").universe();
    assert_eq!(order_in(&allocation, universe), ["a", "b", "c", "d"]);
    assert_disjoint(&allocation);
}

#[test]
fn sliding_a_fixture_along_the_run_re_patches_it_into_its_new_place() {
    let rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 8)
        .fixture("b", "truss", 1.0, 8)
        .fixture("c", "truss", 2.0, 8);
    let before = rig.allocate();
    let head = at(&before, "a");

    // Take the first fixture past the last one; nothing else moves on the truss.
    let after = rig.slide("a", 9.0).allocate();

    let universe = head.universe();
    assert_eq!(order_in(&before, universe), ["a", "b", "c"]);
    assert_eq!(order_in(&after, universe), ["b", "c", "a"]);
    // The block still starts where it started — the run kept its universe and
    // its first free channel; only the order inside it changed.
    assert_eq!(at(&after, "b"), head);
    assert_disjoint(&after);
}

#[test]
fn fixtures_at_the_same_station_order_by_node_id() {
    let rig = Rig::new()
        .run("truss")
        .fixture("z", "truss", 1.0, 4)
        .fixture("a", "truss", 1.0, 4);
    let allocation = rig.allocate();
    assert_eq!(
        order_in(&allocation, at(&allocation, "a").universe()),
        ["a", "z"]
    );
}

#[test]
fn addresses_within_a_run_are_contiguous() {
    let rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 6)
        .fixture("b", "truss", 1.0, 11)
        .fixture("c", "truss", 2.0, 3);
    let allocation = rig.allocate();

    let mut rows: Vec<&Assignment> = allocation.assignments.iter().collect();
    rows.sort_by_key(|a| a.footprint.address());
    for pair in rows.windows(2) {
        assert_eq!(
            pair[1].footprint.address(),
            pair[0].footprint.last() + 1,
            "{} does not follow {} without a gap",
            pair[1].fixture,
            pair[0].fixture
        );
    }
}

// ---------------------------------------------------------------------------
// One universe per run
// ---------------------------------------------------------------------------

#[test]
fn each_run_gets_a_universe_of_its_own() {
    let rig = Rig::new()
        .run("upstage")
        .run("downstage")
        .fixture("u1", "upstage", 0.0, 8)
        .fixture("u2", "upstage", 1.0, 8)
        .fixture("d1", "downstage", 0.0, 8);
    let allocation = rig.allocate();

    assert_eq!(
        at(&allocation, "u1").universe(),
        at(&allocation, "u2").universe()
    );
    assert_ne!(
        at(&allocation, "u1").universe(),
        at(&allocation, "d1").universe(),
        "a data fault on one truss must not reach the other"
    );
}

#[test]
fn a_run_that_would_cross_the_boundary_rolls_whole_and_says_so() {
    // A hand-set address parked near the end of the first universe leaves a
    // tail too short for the run's block.
    let mut rig = Rig::new().run("truss").unplaced("pinned", 60);
    rig = rig.pin("pinned", 1, UNIVERSE_SIZE - 59);
    for i in 0..30 {
        rig = rig.fixture(&format!("m{i:02}"), "truss", f64::from(i), 16);
    }
    let allocation = rig.allocate();

    let universes: std::collections::BTreeSet<u16> = allocation
        .assignments
        .iter()
        .filter(|a| !a.pinned)
        .map(|a| a.footprint.universe())
        .collect();
    assert_eq!(
        universes.len(),
        1,
        "the run split instead of rolling: {universes:?}"
    );
    let taken = *universes.iter().next().expect("one universe");
    assert_ne!(
        taken,
        at(&allocation, "pinned").universe(),
        "the run stayed in the universe it did not fit"
    );
    assert!(
        allocation.notes.iter().any(
            |n| matches!(n, Note::RunRolled { run, taken: t, .. } if run == "truss" && *t == taken)
        ),
        "the roll was not reported: {:?}",
        allocation.notes
    );
    assert_disjoint(&allocation);
}

#[test]
fn a_run_wider_than_a_universe_is_the_only_thing_that_splits() {
    let mut rig = Rig::new().run("truss");
    for i in 0..40 {
        rig = rig.fixture(&format!("bar{i:02}"), "truss", f64::from(i), 32);
    }
    let allocation = rig.allocate();

    let universes: std::collections::BTreeSet<u16> = allocation
        .assignments
        .iter()
        .map(|a| a.footprint.universe())
        .collect();
    assert!(universes.len() > 1, "40 x 32 channels fit in one universe?");
    assert!(
        allocation.notes.iter().any(|n| matches!(
            n,
            Note::RunSplit { run, universes } if run == "truss" && universes.len() > 1
        )),
        "the split was not reported: {:?}",
        allocation.notes
    );
    // Split or not, physical order still runs through the whole rig.
    let mut rows: Vec<&Assignment> = allocation.assignments.iter().collect();
    rows.sort_by_key(|a| (a.footprint.universe(), a.footprint.address()));
    let names: Vec<&str> = rows.iter().map(|a| a.fixture.as_str()).collect();
    let mut expected = names.clone();
    expected.sort_unstable();
    assert_eq!(names, expected, "the split lost physical order");
    assert_disjoint(&allocation);
}

// ---------------------------------------------------------------------------
// The tray, and pins
// ---------------------------------------------------------------------------

/// The limit `on_run` documents, pinned rather than implied.
///
/// A fixture on the corner block itself *is* on the run the corner is bolted
/// to — that is what "nearest run ancestor" buys. But the truss on the far side
/// of the corner is its own `Run` node, so it is its own run and its own
/// universe. Change that and this test is the thing that has to change with it.
#[test]
fn straight_corner_straight_is_two_runs_and_two_blocks() {
    let rig = Rig::new()
        .run("upstage")
        .corner("elbow", "upstage", 4.0)
        .run_from("wing", "elbow", 0.2)
        .fixture("up-1", "upstage", 0.0, 8)
        .fixture("up-2", "upstage", 2.0, 8)
        .fixture("hook", "elbow", 0.05, 8)
        .fixture("wing-1", "wing", 0.5, 8)
        .fixture("wing-2", "wing", 1.5, 8);
    let allocation = rig.allocate();
    assert_disjoint(&allocation);

    let run_of = |id: &str| {
        allocation
            .get(id)
            .unwrap_or_else(|| panic!("{id} was allocated"))
            .run
            .clone()
    };
    // A fixture on a piece bolted to a run rides that run.
    assert_eq!(run_of("hook").as_deref(), Some("upstage"));
    // The far side of the corner does not.
    assert_eq!(run_of("wing-1").as_deref(), Some("wing"));

    let upstage = at(&allocation, "up-1").universe();
    let wing = at(&allocation, "wing-1").universe();
    assert_ne!(
        upstage, wing,
        "a corner-connected truss is a second run, so it takes a second universe"
    );
    for id in ["up-2", "hook"] {
        assert_eq!(at(&allocation, id).universe(), upstage);
    }
    assert_eq!(at(&allocation, "wing-2").universe(), wing);
    assert_eq!(order_in(&allocation, wing), ["wing-1", "wing-2"]);
}

#[test]
fn unplaced_fixtures_fill_the_gaps_the_runs_left_starting_at_universe_one() {
    let rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 8)
        .fixture("b", "truss", 1.0, 8)
        .unplaced("tray", 4);
    let allocation = rig.allocate();

    let run_universe = at(&allocation, "a").universe();
    let tray = at(&allocation, "tray");
    assert_eq!(tray.universe(), run_universe);
    assert!(
        tray.address() > at(&allocation, "b").last(),
        "the tray fixture landed inside the run's block"
    );
    assert!(allocation.get("tray").expect("allocated").run.is_none());
    assert_disjoint(&allocation);
}

#[test]
fn a_pinned_address_is_kept_and_the_derived_block_flows_around_it() {
    let mut rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 8)
        .fixture("b", "truss", 1.0, 8)
        .fixture("c", "truss", 2.0, 8);
    let free = rig.allocate();
    let untouched = at(&free, "b");

    // Pin b somewhere the derived pass would not have put it.
    rig = rig.pin("b", 1, untouched.address() + 100);
    let pinned = rig.allocate();

    assert_eq!(at(&pinned, "b").address(), untouched.address() + 100);
    assert!(pinned.get("b").expect("allocated").pinned);
    assert!(!pinned.get("a").expect("allocated").pinned);
    // a and c keep their derived, contiguous block; only b sits out.
    assert_eq!(at(&pinned, "c").address(), at(&pinned, "a").last() + 1);
    assert_disjoint(&pinned);
}

#[test]
fn a_pin_the_run_block_would_have_covered_pushes_the_block_past_it() {
    let mut rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 8)
        .fixture("b", "truss", 1.0, 8)
        .unplaced("hand_set", 8);
    let start = at(&rig.allocate(), "a");
    rig = rig.pin("hand_set", start.universe(), start.address());

    let allocation = rig.allocate();
    assert_eq!(at(&allocation, "hand_set"), start);
    assert!(at(&allocation, "a").address() > start.last());
    assert_disjoint(&allocation);
}

// ---------------------------------------------------------------------------
// Refusals: the range and the collision
// ---------------------------------------------------------------------------

#[test]
fn a_footprint_that_would_run_past_the_end_of_a_universe_does_not_exist() {
    let channels = 24;
    let last_legal = Footprint::new(1, UNIVERSE_SIZE - channels + 1, channels)
        .expect("the last address a 24-channel fixture can take");
    assert_eq!(last_legal.last(), UNIVERSE_SIZE);
    assert_eq!(
        Footprint::new(1, last_legal.address() + 1, channels),
        None,
        "one channel past the end is still past the end"
    );
    assert_eq!(Footprint::new(1, 0, channels), None, "DMX starts at 1");
    assert_eq!(
        Footprint::new(1, 1, 0),
        None,
        "a fixture with no channels could never collide with anything"
    );
}

#[test]
fn occupancy_names_the_fixture_already_holding_a_channel() {
    let held = Footprint::new(3, 100, 16).expect("addressable");
    let occupancy = Occupancy::of([(held, "mover_4".to_string())]);

    let straddles_the_start = Footprint::new(3, 90, 16).expect("addressable");
    let straddles_the_end = Footprint::new(3, 115, 16).expect("addressable");
    let clear = Footprint::new(3, 116, 16).expect("addressable");
    let other_universe = Footprint::new(4, 100, 16).expect("addressable");

    assert_eq!(occupancy.conflict(&straddles_the_start), Some("mover_4"));
    assert_eq!(occupancy.conflict(&straddles_the_end), Some("mover_4"));
    assert_eq!(occupancy.conflict(&clear), None);
    assert_eq!(
        occupancy.conflict(&other_universe),
        None,
        "universe 4 is not universe 3"
    );
}

#[test]
fn occupancy_cells_carry_the_footprint_and_report_a_pre_existing_overlap() {
    let a = Footprint::new(1, 10, 4).expect("addressable");
    let b = Footprint::new(1, 12, 4).expect("addressable");
    let cells = Occupancy::of([(a, "a".to_string()), (b, "b".to_string())]).cells(1);

    assert_eq!(cells.len(), UNIVERSE_SIZE as usize);
    assert_eq!(cells[8].fixture, None, "address 9 is free");
    assert_eq!(cells[9].fixture.as_deref(), Some("a"));
    assert_eq!(cells[9].channel, 0);
    assert_eq!(cells[10].channel, 1, "address 11 is a's second channel");
    assert!(cells[11].collision, "address 12 is claimed by a and b");
    assert!(cells[12].collision, "address 13 too");
    assert!(!cells[9].collision);
    assert_eq!(
        cells[14].fixture.as_deref(),
        Some("b"),
        "address 15 is b's last channel, past a's end"
    );
    assert_eq!(cells[14].channel, 3);
    assert_eq!(cells[15].fixture, None, "address 16 is past both");
}

// ---------------------------------------------------------------------------
// Determinism, and what a distribution asks for
// ---------------------------------------------------------------------------

/// The answer is a function of the *venue*, not of the order the rows came
/// back in — a `SELECT` with no `ORDER BY`, a `HashMap` iteration, a drag that
/// renumbered nothing, must all patch the same.
///
/// Compared as a table keyed by fixture rather than as the raw `Vec`: pins are
/// emitted in input order by design (`Allocation::assignments`), so their
/// *position* in the list legitimately follows the input while their addresses
/// must not.
#[test]
fn the_input_order_of_the_fixture_list_does_not_reach_the_answer() {
    let rig = Rig::new()
        .run("a")
        .run("b")
        .fixture("x", "a", 2.0, 12)
        .fixture("y", "a", 0.5, 12)
        .fixture("z", "b", 1.0, 7)
        .fixture("w", "b", 3.0, 7)
        .unplaced("tray", 3)
        .pin("w", 4, 100);

    let table = |allocation: &Allocation| {
        let mut rows: Vec<(String, Footprint, Option<String>, bool)> = allocation
            .assignments
            .iter()
            .map(|a| (a.fixture.clone(), a.footprint, a.run.clone(), a.pinned))
            .collect();
        rows.sort();
        rows
    };

    let straight = rig.allocate();
    for shuffle in [1usize, 3, 5] {
        let reordered = rig.reordered(shuffle);
        assert_eq!(
            table(&straight),
            table(&reordered),
            "rotating the fixture list by {shuffle} changed the patch"
        );
        assert_eq!(straight.notes, reordered.notes);
        assert_disjoint(&reordered);
    }
}

/// The bug two occupancies made possible: an offer that the door refuses.
///
/// The rig is what one-at-a-time adds actually write — every fixture packed
/// into universe 1 in creation order, nobody having auto-patched since — while
/// the rule would put the second run in universe 2. An offer computed against
/// the rule alone lands in the hole the rule left in universe 1, which a stored
/// row is sitting in, and `services::patch::admit` throws it out.
#[test]
fn next_addresses_never_offers_a_stored_slot() {
    let mut rig = Rig::new().run("bar").run("wash");
    for index in 0..8 {
        rig = rig.fixture(&format!("bar-{index}"), "bar", f64::from(index), 16);
    }
    for index in 0..4 {
        rig = rig.fixture(&format!("wash-{index}"), "wash", f64::from(index), 16);
    }
    // Packed sequentially into universe 1, the way the add dialog writes them.
    for (index, id) in (0..8)
        .map(|i| format!("bar-{i}"))
        .chain((0..4).map(|i| format!("wash-{i}")))
        .enumerate()
    {
        rig = rig.stored(
            &id,
            1,
            u16::try_from(index * 16 + 1).expect("inside a universe"),
        );
    }

    let stored = Occupancy::of(
        rig.fixtures
            .iter()
            .filter_map(|f| Some((f.address.footprint()?, f.id.clone()))),
    );
    // The rule really does disagree with the rows — otherwise this test would
    // pass for the wrong reason.
    assert_ne!(
        at(&rig.allocate(), "wash-0").universe(),
        1,
        "the rule puts the second run in its own universe; the rows do not"
    );

    for run in [None, Some("bar"), Some("wash")] {
        for offered in rig.next(run, 16, 3) {
            assert_eq!(
                stored.conflict(&offered),
                None,
                "offered {offered:?} for run {run:?}, which a stored row already holds"
            );
        }
    }
}

#[test]
fn next_addresses_appends_after_what_is_already_on_the_run() {
    let rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 8)
        .fixture("b", "truss", 1.0, 8);
    let allocation = rig.allocate();
    let next = rig.next(Some("truss"), 8, 3);

    assert_eq!(next.len(), 3);
    assert_eq!(next[0].universe(), at(&allocation, "b").universe());
    assert_eq!(next[0].address(), at(&allocation, "b").last() + 1);
    for pair in next.windows(2) {
        assert_eq!(pair[1].address(), pair[0].last() + 1);
    }
    for placed in [at(&allocation, "a"), at(&allocation, "b")] {
        assert!(next.iter().all(|f| !f.overlaps(&placed)));
    }
}

#[test]
fn next_addresses_for_a_run_with_nothing_on_it_takes_a_fresh_universe() {
    let rig = Rig::new()
        .run("used")
        .run("empty")
        .fixture("a", "used", 0.0, 8);
    let taken = at(&rig.allocate(), "a").universe();
    let next = rig.next(Some("empty"), 8, 2);

    assert_eq!(next.len(), 2);
    assert!(next.iter().all(|f| f.universe() != taken));
    assert_eq!(next[0].universe(), next[1].universe());
}

#[test]
fn next_addresses_with_no_run_answers_the_tray_rule() {
    let rig = Rig::new()
        .run("truss")
        .fixture("a", "truss", 0.0, 8)
        .unplaced("tray", 8);
    let allocation = rig.allocate();
    let next = rig.next(None, 8, 1);

    assert_eq!(next.len(), 1);
    assert_eq!(next[0].address(), at(&allocation, "tray").last() + 1);
    assert_eq!(next[0].universe(), at(&allocation, "tray").universe());
}

// ---------------------------------------------------------------------------
// The golden
// ---------------------------------------------------------------------------

/// The rig the golden pins: two runs, eight fixtures on one and six on the
/// other, one of them addressed by hand. Added in an order that has nothing to
/// do with where they hang.
fn two_runs() -> Rig {
    let mut rig = Rig::new().run("downstage").run("upstage");
    for (index, along) in [3.5, 0.5, 2.5, 1.5, 3.0, 1.0, 2.0, 0.0]
        .into_iter()
        .enumerate()
    {
        rig = rig.fixture(&format!("mover_{index}"), "downstage", along, 16);
    }
    for (index, along) in [1.2, 0.0, 3.6, 2.4, 4.8, 6.0].into_iter().enumerate() {
        rig = rig.fixture(&format!("bar_{index}"), "upstage", along, 9);
    }
    // Paperwork the rental company already printed: this bar is on 2/300.
    rig.pin("bar_2", 2, 300)
}

/// A single run whose block will not fit in the universe it is offered.
fn crowded() -> Rig {
    let mut rig = Rig::new().run("truss").unplaced("house_dimmer", 96);
    rig = rig.pin("house_dimmer", 1, UNIVERSE_SIZE - 95);
    for index in 0..24 {
        #[allow(clippy::cast_precision_loss)]
        let along = index as f64 * 0.5;
        rig = rig.fixture(&format!("par_{index:02}"), "truss", along, 18);
    }
    rig
}

fn capture(rig: &Rig) -> serde_json::Value {
    let allocation = rig.allocate();
    let rows: Vec<serde_json::Value> = allocation
        .assignments
        .iter()
        .map(|a| {
            serde_json::json!({
                "node": a.fixture,
                "run": a.run,
                "universe": a.footprint.universe(),
                "address": a.footprint.address(),
                "footprint": [a.footprint.address(), a.footprint.last()],
                "pinned": a.pinned,
            })
        })
        .collect();
    let notes: Vec<serde_json::Value> = allocation
        .notes
        .iter()
        .map(|note| match note {
            Note::RunRolled {
                run,
                offered,
                taken,
            } => serde_json::json!({ "runRolled": run, "offered": offered, "taken": taken }),
            Note::RunSplit { run, universes } => {
                serde_json::json!({ "runSplit": run, "universes": universes })
            }
            Note::NoRoom { fixture } => serde_json::json!({ "noRoom": fixture }),
        })
        .collect();
    serde_json::json!({ "patch": rows, "notes": notes })
}

/// The two refusals, captured as answers rather than as panics: a hand-set
/// address that lands on somebody, and one whose footprint runs off the end.
fn refusals() -> serde_json::Value {
    let allocation = two_runs().allocate();
    let occupancy = Occupancy::of(
        allocation
            .assignments
            .iter()
            .map(|a| (a.footprint, a.fixture.clone())),
    );
    let victim = at(&allocation, "mover_0");
    let collide = Footprint::new(victim.universe(), victim.address(), 16).expect("addressable");
    serde_json::json!({
        "collision": {
            "universe": collide.universe(),
            "address": collide.address(),
            "refusedBecauseOf": occupancy.conflict(&collide),
        },
        "pastTheEnd": {
            "address": UNIVERSE_SIZE,
            "channels": 16,
            "footprint": Footprint::new(1, UNIVERSE_SIZE, 16).map(|f| f.address()),
        },
    })
}

fn golden() -> String {
    let mut out = serde_json::to_string_pretty(&serde_json::json!({
        "twoRuns": capture(&two_runs()),
        "crowdedRun": capture(&crowded()),
        "refusals": refusals(),
    }))
    .expect("the capture serializes");
    out.push('\n');
    out
}

#[test]
fn patch_allocation_golden_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../harness/goldens/patch-allocation.json");
    let contents = golden();
    let same = std::fs::read_to_string(&path).is_ok_and(|old| old == contents);
    if !same {
        std::fs::write(&path, &contents).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
    assert!(
        same,
        "the patch-allocation golden was stale and has been rewritten — review and commit it"
    );
}
