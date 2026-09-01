//! The authoring layer: intent stated in the venue frame, compiled to the graph
//! edits [`crate::venue`] already admits.
//!
//! [`crate::venue`] owns pose truth — a relation goes in, a pose comes out. This
//! module is the layer *above* it and answers a different question: given "a
//! truss, eight metres, running up from here", which relation is that? Keeping
//! the two apart is the whole reason neither grows the other's vocabulary —
//! [`compile`] never places anything, and the resolver never hears a direction
//! vector.
//!
//! # The frame
//!
//! Everything a caller states is in the **facade frame**: `+u` stage right,
//! `+v` toward the crowd, `+z` up, metres, degrees. That frame is the crate's
//! world space under a name a builder can say ([`crate::coords::world_from_three_d`]),
//! so there is no third convention here — only the one conversion at this
//! boundary, because the resolver below works in three space (Y-up).
//!
//! # Intent is quantized, and the answer is the truth
//!
//! A structural run comes in [`MODULE_M`] steps and a hinge in
//! [`HINGE_STEP_DEG`] steps. A request off the grid is **snapped and
//! announced** rather than refused: the caller's vector is intent, the plan is
//! what will exist, and [`Plan::announce`] carries the difference. Only three
//! things refuse — a turn the joint cannot make, a box already occupying the
//! space, and a name the catalog does not have. Each refusal carries the fix.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DMat4, DVec3};

use crate::aabb::{obb_intersects, DAabb};
use crate::catalog::{Family, Geometry};
use crate::coords;
use crate::sockets::{Polarity, ResolvedSocket, SocketType};
use crate::venue::{
    place_on, Edge, Node, NodeKind, NodeSockets, ResolvedVenue, SurfacePlacement, VenueGraph,
    FLOOR_SOCKET, RIG_SOCKET,
};

// ---------------------------------------------------------------------------
// The grid the whole surface is quantized to
// ---------------------------------------------------------------------------

/// The structural module, metres. Every buildable length is a whole multiple of
/// it; speakers, players and other endpoints are exempt because nothing chains
/// off them.
pub const MODULE_M: f64 = 0.5;

/// The step a hinge's deflection is quantized to, degrees.
pub const HINGE_STEP_DEG: f64 = 5.0;

/// How far a hinge may deflect either way, degrees. A turn past a quarter is a
/// corner block, not a hinge.
pub const HINGE_LIMIT_DEG: f64 = 90.0;

/// How far off a requested direction a solved joint may land and still count as
/// the turn that was asked for. A truss end mates in quarter turns, so anything
/// this module cannot hit misses by tens of degrees — the tolerance only has to
/// separate "exact" from "not the same joint at all".
const DIRECTION_TOLERANCE: f64 = 1e-3;

/// Clearance both boxes are shrunk by before the collision test, metres.
/// Contact is not collision: a mated piece touches its host by construction.
const COLLISION_CLEARANCE_M: f64 = 0.02;

/// How close two faces' normals must be before they count as pointing the same
/// way, as a difference of cosines. Wide, because the tie this resolves is
/// exact: a deck's four top edges inherit their normal from the top face they
/// sit on, so they point *identically* up and only their joint tells them apart.
const FACE_TIE: f64 = 1e-6;

/// How far off a stated `face=` the nearest face may point and still be the one
/// meant, as a cosine. Beyond sixty degrees the caller named a face the piece
/// does not have, and seating it on whatever happened to be nearest is how a
/// speaker ends up facing the wall with nothing said.
const FACE_MIN_DOT: f64 = 0.5;

/// The nearest buildable length, never shorter than one module.
#[must_use]
pub fn quantize_length(metres: f64) -> f64 {
    if !metres.is_finite() {
        return MODULE_M;
    }
    (metres / MODULE_M).round().max(1.0) * MODULE_M
}

/// The nearest legal hinge deflection: whole [`HINGE_STEP_DEG`] steps. Signed —
/// the sign is the right-hand rule about the stated axis.
///
/// **Snapping only.** A turn past ±[`HINGE_LIMIT_DEG`] is not a step to round
/// to the nearest of, it is a turn this joint cannot make, and [`compile`]
/// refuses it ([`Refusal::TurnTooFar`]) before anything reaches here. Clamping
/// it built a right angle when a hundred and twenty was asked for and called
/// that an announcement.
#[must_use]
pub fn quantize_hinge(degrees: f64) -> f64 {
    if !degrees.is_finite() {
        return 0.0;
    }
    (degrees / HINGE_STEP_DEG).round() * HINGE_STEP_DEG
}

// ---------------------------------------------------------------------------
// The facade frame
// ---------------------------------------------------------------------------

/// A facade vector `(u, v, z)` as the resolver's three-space vector.
#[must_use]
pub fn three_from_facade(v: DVec3) -> DVec3 {
    coords::three_from_world_d(v)
}

/// A three-space vector as facade `(u, v, z)`.
#[must_use]
pub fn facade_from_three(v: DVec3) -> DVec3 {
    coords::world_from_three_d(v)
}

/// Facade `(u, v, z)` rounded for a message. Metres to the centimetre: a
/// builder reads these, and three decimal places of a truss position is noise.
fn say(v: DVec3) -> String {
    format!("({:.2}, {:.2}, {:.2})", v.x, v.y, v.z)
}

// ---------------------------------------------------------------------------
// What a query answers with
// ---------------------------------------------------------------------------

/// Where a placed node actually is, in the frame the caller states intent in.
///
/// The **footprint**, not the mesh origin: `at` is the plan centre of the box
/// the piece fills and `size` is how far it reaches on each facade axis. That
/// is the number a caller can hand straight back to `place(at=)`, which is the
/// whole point — a query field that is not legal input to a write verb is a
/// number the caller has to translate, and translating is where a rig ends up
/// one module off centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Footprint {
    /// Plan centre, facade metres.
    pub at: [f64; 2],
    /// Centre height, facade metres.
    pub z: f64,
    /// World-axis-aligned extent on `(u, v, z)`.
    pub size: [f64; 3],
}

impl Footprint {
    /// The footprint of a local box carried by a world transform.
    #[must_use]
    pub fn of(world: &DMat4, bounds: DAabb) -> Footprint {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for corner in corners(bounds) {
            let p = facade_from_three(world.transform_point3(corner));
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let centre = (lo + hi) * 0.5;
        let size = hi - lo;
        Footprint {
            at: [centre.x, centre.y],
            z: centre.z,
            size: size.to_array(),
        }
    }
}

/// The bounding span of a set of footprints, in the facade frame.
///
/// The one-line "is this centred" check: `centre` is what a caller compares
/// against zero, `size` is what it compares against the room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    /// How many footprints went into it.
    pub count: usize,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub centre: [f64; 3],
    pub size: [f64; 3],
}

impl Extent {
    /// The extent of every `(world, bounds)` pair, or `None` for none at all.
    #[must_use]
    pub fn of(items: impl IntoIterator<Item = (DMat4, DAabb)>) -> Option<Extent> {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        let mut count = 0;
        for (world, bounds) in items {
            for corner in corners(bounds) {
                let p = facade_from_three(world.transform_point3(corner));
                lo = lo.min(p);
                hi = hi.max(p);
            }
            count += 1;
        }
        (count > 0).then(|| Extent {
            count,
            min: lo.to_array(),
            max: hi.to_array(),
            centre: ((lo + hi) * 0.5).to_array(),
            size: (hi - lo).to_array(),
        })
    }
}

/// The face a direction names, among candidates already known to be hostable.
///
/// Nearest by normal, and among faces pointing the *same* way the one a piece
/// can be set down on rather than the joint that only butts two pieces together
/// — a deck's four top edges inherit the top face's own up, so a plain "put it
/// on top" ties four ways and three of the four answers are edge joints.
fn nearest_face<'a>(
    faces: &'a [(ResolvedSocket, DVec3)],
    wanted: DVec3,
) -> Option<&'a (ResolvedSocket, DVec3)> {
    let best = faces
        .iter()
        .map(|(_, normal)| normal.dot(wanted))
        .fold(f64::NEG_INFINITY, f64::max);
    let tied = faces
        .iter()
        .filter(move |(_, normal)| normal.dot(wanted) >= best - FACE_TIE);
    let mut tied = tied.peekable();
    let first = *tied.peek()?;
    Some(
        tied.clone()
            .find(|(s, _)| s.socket_type.kind() == crate::sockets::SocketKind::Surface)
            .unwrap_or(first),
    )
}

/// Why this piece hosts nothing at all, where the catalog has a reason worth
/// saying. `None` for a piece that simply has no free face.
fn hosts_nothing_because(piece: &crate::catalog::Piece) -> Option<&'static str> {
    (piece.kind == crate::catalog::PieceKind::DjBooth)
        .then_some("the booth already carries its own players and mixer")
}

/// How far a placed box reaches along one world direction, metres.
fn span_along(world: &DMat4, bounds: DAabb, direction: DVec3) -> f64 {
    let direction = direction.normalize_or_zero();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for corner in corners(bounds) {
        let along = world.transform_point3(corner).dot(direction);
        lo = lo.min(along);
        hi = hi.max(along);
    }
    hi - lo
}

fn corners(b: DAabb) -> [DVec3; 8] {
    let (l, h) = (b.min, b.max);
    [
        DVec3::new(l.x, l.y, l.z),
        DVec3::new(h.x, l.y, l.z),
        DVec3::new(l.x, h.y, l.z),
        DVec3::new(h.x, h.y, l.z),
        DVec3::new(l.x, l.y, h.z),
        DVec3::new(h.x, l.y, h.z),
        DVec3::new(l.x, h.y, h.z),
        DVec3::new(h.x, h.y, h.z),
    ]
}

/// One free end of a placed node, as a direction rather than a name.
///
/// The whole of what replaces socket names on this surface: a tip is *where* a
/// chain can grow and *which way*, and both halves are vectors the caller can
/// state back.
#[derive(Clone, Debug, PartialEq)]
pub struct Tip {
    /// The node the end belongs to.
    pub node: String,
    /// The socket, kept because the graph edit below needs a name. Not part of
    /// the caller-facing vocabulary — the facade never shows it.
    pub socket: String,
    /// Which way the end faces, facade unit vector.
    pub direction: [f64; 3],
    /// Where the end is, facade metres.
    pub at: [f64; 3],
}

// ---------------------------------------------------------------------------
// The venue as the authoring layer reads it
// ---------------------------------------------------------------------------

/// One solve of a venue, plus the geometry supply, bound together.
///
/// Every query and every compile reads exactly this: the graph for relations,
/// the solve for poses, the supply for sockets and boxes. Constructed per call —
/// a plan is only true of the venue it was compiled against.
pub struct Scene<'a, S: NodeSockets + ?Sized> {
    graph: &'a VenueGraph,
    solved: &'a ResolvedVenue,
    sockets: &'a S,
}

impl<'a, S: NodeSockets + ?Sized> Scene<'a, S> {
    #[must_use]
    pub fn new(graph: &'a VenueGraph, solved: &'a ResolvedVenue, sockets: &'a S) -> Self {
        Self {
            graph,
            solved,
            sockets,
        }
    }

    #[must_use]
    pub fn graph(&self) -> &'a VenueGraph {
        self.graph
    }

    /// One node's world transform, or `None` if the solve never reached it.
    #[must_use]
    pub fn world(&self, node: &str) -> Option<DMat4> {
        self.solved.pose(node).map(|p| p.world)
    }

    /// One node's local box, or `None` where the supply cannot measure one.
    #[must_use]
    pub fn bounds(&self, node: &str) -> Option<DAabb> {
        self.sockets.bounds(self.graph.node(node)?)
    }

    /// One node's footprint in the facade frame.
    #[must_use]
    pub fn footprint(&self, node: &str) -> Option<Footprint> {
        Some(Footprint::of(&self.world(node)?, self.bounds(node)?))
    }

    /// The extent of a set of nodes, ignoring any the solve has no pose for.
    ///
    /// A node the *supply* cannot measure — a fixture, whose box is a patch row
    /// rather than a mesh — counts as the point it hangs at. Leaving it out
    /// made `extent(kind="fixture")` answer `None` for a room full of lights,
    /// which reads as "nothing is there".
    #[must_use]
    pub fn extent<'n>(&self, nodes: impl IntoIterator<Item = &'n str>) -> Option<Extent> {
        Extent::of(nodes.into_iter().filter_map(|id| {
            Some((
                self.world(id)?,
                self.bounds(id)
                    .unwrap_or_else(|| DAabb::new(DVec3::ZERO, DVec3::ZERO)),
            ))
        }))
    }

    /// One node's sockets in world space, as `(socket, world position, world
    /// outward normal)`.
    fn world_sockets(&self, node: &str) -> Vec<(ResolvedSocket, DVec3, DVec3)> {
        let (Some(row), Some(world)) = (self.graph.node(node), self.world(node)) else {
            return Vec::new();
        };
        let mut out: Vec<(ResolvedSocket, DVec3, DVec3)> = self
            .sockets
            .sockets(row)
            .into_iter()
            .map(|socket| {
                let at = world.transform_point3(socket.position);
                let normal = world.transform_vector3(socket.normal).normalize_or_zero();
                (socket, at, normal)
            })
            .collect();
        if row.kind == NodeKind::Venue {
            for name in [FLOOR_SOCKET, RIG_SOCKET] {
                if let Some(socket) = crate::venue::root_socket(name) {
                    let at = world.transform_point3(socket.position);
                    let normal = world.transform_vector3(socket.normal).normalize_or_zero();
                    out.push((socket, at, normal));
                }
            }
        }
        out
    }

    /// Which of `node`'s sockets some relation already accounts for.
    fn claimed(&self, node: &str) -> BTreeSet<String> {
        let mut claimed = BTreeSet::new();
        for (child, edge) in self.graph.relations() {
            if child.id == node {
                claimed.insert(edge.my_socket.clone());
            }
            if edge.parent == node {
                claimed.insert(edge.their_socket.clone());
            }
        }
        for check in self.graph.constraints() {
            if check.node == node {
                claimed.insert(check.my_socket.clone());
            }
            if check.target_node == node {
                claimed.insert(check.target_socket.clone());
            }
        }
        claimed
    }

    /// The outward normal of the face `node` is mounted on, facade.
    ///
    /// For a light this is the beam it leaves at rest — beam is the mount
    /// normal — so it is the one field that answers "is this rig pointing where
    /// I meant" without a second call.
    #[must_use]
    pub fn mounted_face(&self, node: &str) -> Option<[f64; 3]> {
        let edge = self.graph.edge(node)?;
        let host = self
            .world_sockets(&edge.parent)
            .into_iter()
            .find(|(s, _, _)| s.name == edge.their_socket)?;
        Some(facade_from_three(host.2).to_array())
    }

    /// Every free end of `node`, as vectors.
    ///
    /// Only self-mating joints: a deck's top is a host with nothing on it, not
    /// an end a chain grows from.
    #[must_use]
    pub fn tips(&self, node: &str) -> Vec<Tip> {
        let claimed = self.claimed(node);
        self.world_sockets(node)
            .into_iter()
            .filter(|(s, _, _)| s.socket_type.polarity() == Polarity::Neutral)
            .filter(|(s, _, _)| !claimed.contains(&s.name))
            .map(|(s, at, normal)| Tip {
                node: node.to_string(),
                socket: s.name.clone(),
                direction: facade_from_three(normal).to_array(),
                at: facade_from_three(at).to_array(),
            })
            .collect()
    }

    /// The free end of `node` facing nearest `direction`, or the only one when
    /// no direction is stated.
    ///
    /// # Errors
    /// [`Refusal::NoTip`] when the node has no free end at all, and
    /// [`Refusal::AmbiguousTip`] when it has several and the caller named none —
    /// listing them as vectors, so the fix is in the message.
    pub fn tip(&self, node: &str, direction: Option<DVec3>) -> Result<Tip, Refusal> {
        let tips = self.tips(node);
        if tips.is_empty() {
            return Err(Refusal::NoTip {
                node: node.to_string(),
            });
        }
        let Some(direction) = direction else {
            if tips.len() == 1 {
                return Ok(tips.into_iter().next().expect("just checked"));
            }
            return Err(Refusal::AmbiguousTip {
                node: node.to_string(),
                ends: tips.iter().map(|t| t.direction).collect(),
            });
        };
        let wanted = direction.normalize_or_zero();
        tips.into_iter()
            .max_by(|a, b| {
                DVec3::from(a.direction)
                    .dot(wanted)
                    .total_cmp(&DVec3::from(b.direction).dot(wanted))
            })
            .ok_or_else(|| Refusal::NoTip {
                node: node.to_string(),
            })
    }

    /// The socket on `node` a caller means by a face vector: the host-capable
    /// socket whose outward normal points nearest `direction`.
    ///
    /// This is the whole of "no socket names": a face is named by pointing at
    /// it. Beam is the mount normal, so on a fixture host choosing the face is
    /// choosing where the light points at rest.
    ///
    /// # Errors
    /// [`Refusal::NoFace`], listing the faces the piece does have as vectors.
    pub fn face(&self, node: &str, direction: DVec3) -> Result<String, Refusal> {
        let wanted = three_from_facade(direction).normalize_or_zero();
        let faces: Vec<(ResolvedSocket, DVec3)> = self
            .world_sockets(node)
            .into_iter()
            .filter(|(s, _, _)| s.socket_type.polarity().can_host())
            .filter(|(s, _, _)| s.socket_type != SocketType::Grab)
            .map(|(s, _, normal)| (s, normal))
            .collect();
        nearest_face(&faces, wanted)
            .map(|(s, _)| s.name.clone())
            .ok_or_else(|| self.no_face(node, &faces))
    }

    /// How a node reads in a message: its id, its label and what it is.
    ///
    /// A refusal naming a bare uuid tells the reader nothing they can act on,
    /// and the graph is holding both halves of the answer.
    fn name_of(&self, node: &str) -> String {
        let Some(row) = self.graph.node(node) else {
            return format!("`{node}`");
        };
        if row.kind == NodeKind::Venue {
            return format!("the room itself (`{node}`)");
        }
        let piece = row
            .catalog_ref
            .as_deref()
            .and_then(crate::catalog::find)
            .map(|p| p.short);
        match (row.label.as_deref(), piece) {
            (Some(label), Some(piece)) => format!("`{node}` (\"{label}\", a {piece})"),
            (Some(label), None) => format!("`{node}` (\"{label}\")"),
            (None, Some(piece)) => format!("`{node}` (a {piece})"),
            (None, None) => format!("`{node}`"),
        }
    }

    /// The refusal for a face nothing here has, named the way a reader can act
    /// on.
    fn no_face(&self, node: &str, faces: &[(ResolvedSocket, DVec3)]) -> Refusal {
        Refusal::NoFace {
            name: self.name_of(node),
            hint: self
                .graph
                .node(node)
                .and_then(|row| row.catalog_ref.as_deref())
                .and_then(crate::catalog::find)
                .and_then(|piece| hosts_nothing_because(piece).map(str::to_string)),
            offered: faces
                .iter()
                .map(|(_, n)| facade_from_three(*n).to_array())
                .collect(),
        }
    }

    /// The first placed node whose box the candidate overlaps, ignoring
    /// `parent` (a mate touches its host by construction).
    fn blocker(
        &self,
        world: &DMat4,
        bounds: DAabb,
        parent: &str,
    ) -> Option<(String, Option<String>)> {
        for pose in self.solved.poses() {
            if pose.node == parent || pose.kind == NodeKind::Venue {
                continue;
            }
            let Some(row) = self.graph.node(&pose.node) else {
                continue;
            };
            let Some(other) = self.sockets.bounds(row) else {
                continue;
            };
            if obb_intersects(bounds, world, other, &pose.world, COLLISION_CLEARANCE_M) {
                return Some((pose.node.clone(), pose.label.clone()));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Why a request produced no plan.
///
/// Every variant carries the fix — the legal alternatives as vectors, the node
/// in the way, the nearest catalog names. A refusal a caller cannot act on is a
/// refusal that gets retried unchanged.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// No catalog piece answers to that name.
    UnknownPiece { name: String, near: Vec<String> },
    /// The named node is not in this venue, or the solve never reached it.
    UnknownNode(String),
    /// The node has no free end to grow from.
    NoTip { node: String },
    /// Several free ends and no direction to pick between them.
    AmbiguousTip { node: String, ends: Vec<[f64; 3]> },
    /// The host has no face at all a piece could sit on, or none pointing the
    /// way the caller named.
    NoFace {
        /// The host as a reader knows it — id, label and piece. A bare uuid is
        /// not something anybody can act on.
        name: String,
        /// Why this piece hosts nothing, where there is a reason to give.
        hint: Option<String>,
        offered: Vec<[f64; 3]>,
    },
    /// A piece with no half that bolts to the end it was handed.
    NoJoint {
        piece: String,
        /// The host, as [`Scene::name_of`] words it.
        host: String,
        /// What the host's end is, in the socket vocabulary's own word.
        joint: &'static str,
        /// Whether that joint articulates, and so whether `angle=` is the fix.
        turnable: Option<f64>,
    },
    /// A turn asked of a joint that is bolted rather than articulated.
    NoTurn { piece: String, joint: &'static str },
    /// A turn past what one joint can give.
    TurnTooFar { wanted: f64, limit: f64 },
    /// No joint this piece has makes that turn. `legal` is every direction it
    /// *can* leave in, as facade vectors.
    ImpossibleTurn {
        wanted: [f64; 3],
        legal: Vec<[f64; 3]>,
    },
    /// A hinge axis that is not perpendicular to the run it turns.
    BadAxis {
        wanted: [f64; 3],
        /// The two axes the incoming run admits, either sign.
        plane: Vec<[f64; 3]>,
    },
    /// A direction was given where a rotation axis was expected, or the other
    /// way round.
    WrongKind {
        expected: &'static str,
        got: &'static str,
    },
    /// The piece would land inside something already standing there.
    Collision { node: String, label: Option<String> },
    /// A `to=` target the run cannot reach along the direction it leaves in.
    CannotMeet {
        target: String,
        /// How far off the line the target's end sits, metres.
        offset_m: f64,
    },
    /// The request is missing something the piece cannot be built without.
    Incomplete(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let vectors = |v: &[[f64; 3]]| {
            v.iter()
                .map(|d| say(DVec3::from(*d)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Refusal::UnknownPiece { name, near } if near.is_empty() => {
                write!(f, "`{name}` is not a catalog piece — read `catalog()`")
            }
            Refusal::UnknownPiece { name, near } => write!(
                f,
                "`{name}` is not a catalog piece; did you mean {}?",
                near.join(", ")
            ),
            Refusal::UnknownNode(id) => write!(f, "`{id}` is not a placed node in this venue"),
            Refusal::NoTip { node } => {
                write!(f, "`{node}` has no free end to build from")
            }
            Refusal::AmbiguousTip { node, ends } => write!(
                f,
                "`{node}` has {} free ends — say which with end=, one of {}",
                ends.len(),
                vectors(ends)
            ),
            Refusal::NoFace {
                name,
                hint,
                offered,
            } if offered.is_empty() => write!(
                f,
                "{name} has no face anything mounts on{}",
                hint.as_deref()
                    .map(|why| format!(" — {why}"))
                    .unwrap_or_default()
            ),
            Refusal::NoFace { name, offered, .. } => write!(
                f,
                "{name} has no face that way; its faces point {}",
                vectors(offered)
            ),
            Refusal::NoJoint {
                piece,
                host,
                joint,
                turnable,
            } => {
                write!(f, "a `{piece}` has no half that bolts to a {joint}: {host} chains only to another piece with a {joint}")?;
                match turnable {
                    Some(step) => write!(
                        f,
                        " — to turn the chain, give the next piece angle= instead \
                         ({step:.0}deg steps, up to +-{HINGE_LIMIT_DEG:.0}deg)"
                    ),
                    None => Ok(()),
                }
            }
            Refusal::NoTurn { piece, joint } => write!(
                f,
                "a {joint} is bolted, so a `{piece}` on it has no angle to set; \
                 turn with a hinge or a corner instead"
            ),
            Refusal::TurnTooFar { wanted, limit } => write!(
                f,
                "angle={wanted:.0}deg is past this joint's +-{limit:.0}deg; chain \
                 another piece and turn again"
            ),
            Refusal::ImpossibleTurn { wanted, legal } if legal.is_empty() => write!(
                f,
                "nothing here leaves toward {}; this joint has no other way out",
                say(DVec3::from(*wanted))
            ),
            Refusal::ImpossibleTurn { wanted, legal } => write!(
                f,
                "nothing here leaves toward {}; this joint turns to {}",
                say(DVec3::from(*wanted)),
                vectors(legal)
            ),
            Refusal::BadAxis { wanted, plane } => write!(
                f,
                "axis={} is not perpendicular to the run it turns; this hinge \
                 turns about {}",
                say(DVec3::from(*wanted)),
                vectors(plane)
            ),
            Refusal::WrongKind { expected, got } => {
                write!(f, "{got} was given where {expected} was expected")
            }
            Refusal::Collision { node, label } => write!(
                f,
                "that lands inside `{node}`{}",
                label
                    .as_deref()
                    .map(|l| format!(" (\"{l}\")"))
                    .unwrap_or_default()
            ),
            Refusal::CannotMeet { target, offset_m } => write!(
                f,
                "this run misses `{target}` by {offset_m:.2} m across — it can only \
                 spend length along the way it leaves"
            ),
            Refusal::Incomplete(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Refusal {}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// One `place` or `add` — the whole of the chain grammar in one shape.
///
/// A single struct rather than two verbs because they build the *same piece*:
/// `place` anchors it by its footprint centre on a surface and `add` grows it
/// from a tip, and everything else — the direction, the length, the joint — is
/// common. Splitting them would put the quantization and the collision test in
/// two places.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// The catalog piece, by short name or stored id.
    pub piece: String,
    /// The end to grow from. `None` is a free placement.
    pub from: Option<Tip>,
    /// Plan centre for a free placement, facade metres.
    pub at: Option<[f64; 2]>,
    /// The surface a free placement sits on; `None` is the venue floor.
    pub on: Option<String>,
    /// Which face of `on` to sit on, as a direction. `None` takes the face
    /// pointing most nearly up.
    pub face: Option<[f64; 3]>,
    /// The way the piece runs, or the way a chain leaves the joint.
    pub direction: Option<[f64; 3]>,
    /// A hinge's pin, facade. Distinct from [`Self::direction`] on purpose —
    /// see [`Refusal::WrongKind`].
    pub axis: Option<[f64; 3]>,
    /// A hinge's deflection, degrees, right-hand rule about `axis`.
    pub angle: Option<f64>,
    /// Metres of run. Quantized to [`MODULE_M`].
    pub length: Option<f64>,
    /// A node this run must meet exactly.
    pub to: Option<String>,
    /// How high a free placement flies, metres.
    pub trim: f64,
    pub label: Option<String>,
}

/// The graph edits one request compiles to, and what they will produce.
///
/// The caller writes these rows; nothing here has happened yet. `at`, `size`
/// and `tip` are the solve's own answer for the piece the plan describes, which
/// is what makes the report the *actual* landing rather than the request echoed
/// back.
#[derive(Clone, Debug)]
pub struct Plan {
    pub kind: NodeKind,
    pub catalog_ref: String,
    pub label: Option<String>,
    pub params: BTreeMap<String, f64>,
    /// The relation that places it. `parent` is a node id already in the graph.
    pub edge: Edge,
    /// A new roll for the **parent's** own edge, when this piece's direction is
    /// what chose the parent joint's exit face. Written before the child.
    pub parent_roll: Option<f64>,
    /// A far end this run meets: `(my socket, target node, target socket)`.
    pub constraint: Option<(String, String, String)>,
    /// What was snapped, in words. Empty when the request was already legal.
    pub announce: Vec<String>,
    /// Where the piece will land.
    pub at: Footprint,
    /// The free end it will leave behind, with its socket name for the next
    /// edit and its direction for the caller.
    pub tip: Option<Tip>,
    /// The way this piece runs, facade unit vector — which is also which of its
    /// two free ends is the *tip*. A stick placed flat has both ends open and
    /// the chain grows out of the downstream one; without a run there is no
    /// downstream, only two ends and a coin toss.
    pub run: [f64; 3],
    /// The transform the solve will give it — carried so the caller can test
    /// what it is about to write without solving twice.
    pub world: DMat4,
}

impl Plan {
    /// Write this plan into a graph as node `id`.
    ///
    /// The **one** ordering the plan implies: a re-rolled parent is written
    /// before the child that chose the roll, because the child mates the face
    /// that roll moved. Callers that persist rows instead of holding a graph
    /// (the venue's own writer) follow the same order for the same reason.
    ///
    /// # Errors
    /// Every variant of [`crate::venue::EdgeError`]. A plan compiled against
    /// this graph cannot produce one; a plan compiled against a *different*
    /// graph can, which is what this checks.
    pub fn apply<S: NodeSockets + ?Sized>(
        &self,
        graph: &mut VenueGraph,
        id: &str,
        sockets: &S,
    ) -> Result<(), crate::venue::EdgeError> {
        if let Some(roll) = self.parent_roll {
            if let Some(mut edge) = graph.edge(&self.edge.parent).cloned() {
                edge.roll = roll;
                graph.attach(&self.edge.parent, edge, sockets)?;
            }
        }
        graph.insert(Node {
            id: id.to_string(),
            kind: self.kind,
            catalog_ref: Some(self.catalog_ref.clone()),
            label: self.label.clone(),
            params: self.params.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        });
        graph.attach(id, self.edge.clone(), sockets)?;
        if let Some((mine, target_node, target_socket)) = self.constraint.clone() {
            graph.load_constraint(crate::venue::Constraint {
                node: id.to_string(),
                my_socket: mine,
                target_node,
                target_socket,
            });
        }
        Ok(())
    }

    /// The tip this plan leaves, bound to the node id it was written under.
    #[must_use]
    pub fn tip_at(&self, id: &str) -> Option<Tip> {
        self.tip.clone().map(|tip| Tip {
            node: id.to_string(),
            ..tip
        })
    }
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

/// Turn one stated intent into the graph edits that realise it.
///
/// # Errors
/// Every variant of [`Refusal`]. Nothing here writes, so a refusal leaves the
/// venue exactly as it was.
pub fn compile<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    request: &Request,
) -> Result<Plan, Refusal> {
    let piece = crate::catalog::find(&request.piece).ok_or_else(|| Refusal::UnknownPiece {
        name: request.piece.clone(),
        near: near_names(&request.piece),
    })?;
    // A free placement has no joint to turn, so an angle on one is a hinge's
    // parameter given to a piece that is not a hinge. A *chained* piece is a
    // different question — its joint may articulate — and it is answered where
    // the joint is known.
    if request.angle.is_some()
        && request.axis.is_none()
        && !is_hinge(piece)
        && request.from.is_none()
    {
        return Err(Refusal::WrongKind {
            expected: "an axis= to turn about",
            got: "angle= alone",
        });
    }
    // One range rule, over every joint that turns. Off-step is snapped and
    // announced; past the limit is a turn no joint here makes, and the design's
    // answer to that is a refusal with the fix in it — a hinge that quietly
    // built a right angle for a hundred and twenty degrees was announcing a
    // shape nobody asked for.
    if let Some(angle) = request.angle {
        if !angle.is_finite() || angle.abs() > HINGE_LIMIT_DEG {
            return Err(Refusal::TurnTooFar {
                wanted: angle,
                limit: HINGE_LIMIT_DEG,
            });
        }
    }
    let mut announce = Vec::new();
    let params = piece_params(piece, request, &mut announce);

    let mut probe = Node {
        id: String::new(),
        kind: stick_kind(piece, request.direction),
        catalog_ref: Some(piece.id.to_string()),
        label: request.label.clone(),
        params: params.iter().map(|(k, v)| (k.clone(), *v)).collect(),
    };

    let mut plan = match request.from.as_ref() {
        None => free_placement(scene, request, piece, &probe, &mut announce)?,
        Some(tip) => chained(scene, request, piece, &probe, tip, &mut announce)?,
    };
    // The kind follows the *shape the piece ended up in*, which a chained
    // piece only knows once its joint has been solved: a leg is a tower whether
    // it was placed on the floor or added onto a corner, and filing the two
    // under different kinds is a query that silently misses half a rig.
    plan.kind = stick_kind(piece, Some(plan.run));
    probe.kind = plan.kind;

    // `to=` spends length after the joint has fixed the direction, so it is
    // resolved once the mate is known and then re-solved into the pose below.
    if let Some(target) = request.to.as_deref() {
        let metres = meet(scene, &plan, target)?;
        probe.params.set("span", metres);
        plan.params.insert("span".to_string(), metres);
        announce.push(format!("ran {metres:.2} m to meet `{target}`"));
        plan.world = pose_of(scene, &plan, &probe)?;
        plan.constraint = far_end(scene, &plan, &probe, target);
    }

    let bounds = scene
        .sockets
        .bounds(&probe)
        .unwrap_or_else(|| DAabb::new(DVec3::ZERO, DVec3::ZERO));
    if let Some((node, label)) = scene.blocker(&plan.world, bounds, &plan.edge.parent) {
        return Err(Refusal::Collision { node, label });
    }
    plan.at = Footprint::of(&plan.world, bounds);
    plan.tip = leftover_tip(scene, &plan, &probe);
    plan.announce = announce;
    plan.label = request.label.clone();
    Ok(plan)
}

/// The three or four catalog names nearest a miss, for the refusal's message.
fn near_names(wanted: &str) -> Vec<String> {
    let stem = wanted.rsplit('/').next().unwrap_or(wanted).to_lowercase();
    crate::catalog::pieces()
        .iter()
        .filter(|p| {
            stem.len() >= 3
                && (p.short.contains(&stem)
                    || p.id.to_lowercase().contains(&stem)
                    || p.display_name.to_lowercase().contains(&stem))
        })
        .map(|p| p.short.to_string())
        .take(3)
        .collect()
}

fn is_hinge(piece: &crate::catalog::Piece) -> bool {
    matches!(piece.geometry, Geometry::Procedural(Family::Hinge))
}

fn is_corner(piece: &crate::catalog::Piece) -> bool {
    matches!(piece.geometry, Geometry::Procedural(Family::Corner))
}

fn is_stick(piece: &crate::catalog::Piece) -> bool {
    matches!(piece.geometry, Geometry::Procedural(Family::Truss))
}

/// A mesh piece's own front, in its asset frame (`+Z` front — see
/// [`crate::sockets`]).
const LOCAL_FRONT: DVec3 = DVec3::Z;

/// Whether this piece has a front that means something, and so should meet the
/// house rather than the back wall when the caller states no direction.
///
/// The endpoints the frame contract already names — speakers, players, the
/// mixer, the booth. Structure has a run instead of a front, and a deck turned
/// to face anything is still a deck.
fn faces_house(piece: &crate::catalog::Piece) -> bool {
    use crate::catalog::PieceKind::{Cdj, DjBooth, Mixer, Speaker};
    matches!(piece.kind, Speaker | Cdj | Mixer | DjBooth)
}

/// The node kind a piece is filed under — the *only* thing in this module that
/// looks at what a piece is for rather than what shape it is, because
/// `describe()` and the derived groups read the kind back and a tower filed as
/// a `piece` reads as furniture.
///
/// One rule for both verbs. A stick standing up is a tower and a stick lying
/// along is a run, and which verb built it does not enter into it: filing the
/// placed leg and the chained leg under different kinds made `kind="tower"`
/// answer for half a rig.
fn stick_kind(piece: &crate::catalog::Piece, run: Option<[f64; 3]>) -> NodeKind {
    if !is_stick(piece) {
        return match piece.kind {
            crate::catalog::PieceKind::Floor => NodeKind::Stage,
            _ => NodeKind::Piece,
        };
    }
    let vertical = run.is_some_and(|d| DVec3::from(d).normalize_or_zero().z.abs() > 0.5);
    if vertical {
        NodeKind::Tower
    } else {
        NodeKind::Run
    }
}

/// The generator parameters this piece is built at, with everything the caller
/// asked for quantized and announced.
fn piece_params(
    piece: &crate::catalog::Piece,
    request: &Request,
    announce: &mut Vec<String>,
) -> BTreeMap<String, f64> {
    let mut params = BTreeMap::new();
    if is_stick(piece) {
        if let Some(length) = request.length {
            let built = quantize_length(length);
            if (built - length).abs() > 1e-9 {
                announce.push(format!(
                    "{length:.2} m is not a whole {MODULE_M} m module: built {built:.2} m"
                ));
            }
            params.insert("span".to_string(), built);
        }
    }
    if is_hinge(piece) {
        if let Some(angle) = request.angle {
            let turned = quantize_hinge(angle);
            if (turned - angle).abs() > 1e-9 {
                announce.push(format!(
                    "{angle:.1}deg is not a legal hinge step: turned {turned:.1}deg"
                ));
            }
            params.insert("angle".to_string(), turned.abs());
        }
    }
    params
}

/// Which of a piece's sockets can be *held* — the halves that could meet a
/// host.
fn held_sockets<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    probe: &Node,
) -> Vec<ResolvedSocket> {
    scene
        .sockets
        .sockets(probe)
        .into_iter()
        .filter(|s| s.socket_type.polarity().can_be_held())
        .collect()
}

/// The axis a piece *runs* along, in its own local frame.
///
/// Read off the two ends where the piece has them, and off the longest side of
/// its box where it does not. One rule, so "point it that way" means the same
/// thing for a generated stick and for a guardrail.
fn run_axis<S: NodeSockets + ?Sized>(scene: &Scene<'_, S>, probe: &Node) -> Option<DVec3> {
    let ends: Vec<ResolvedSocket> = scene
        .sockets
        .sockets(probe)
        .into_iter()
        .filter(|s| s.socket_type.polarity() == Polarity::Neutral)
        .collect();
    if ends.len() == 2 {
        let along = ends[1].position - ends[0].position;
        if along.length_squared() > 1e-9 {
            return Some(along.normalize());
        }
    }
    let bounds = scene.sockets.bounds(probe)?;
    let size = bounds.size();
    let longest = if size.x >= size.y && size.x >= size.z {
        DVec3::X
    } else if size.y >= size.z {
        DVec3::Y
    } else {
        DVec3::Z
    };
    Some(longest)
}

/// The box a piece fills where a bare `place` puts it on the floor — its
/// footprint at the origin, in facade axes.
///
/// The catalog's printed dimensions, and the reason they are computed rather
/// than transcribed: a mesh is modelled in whatever frame its author chose, the
/// seat turns it, and an endpoint turns again to meet the house. Any answer not
/// solved through the same [`place_on`] the placement uses is a second answer,
/// and the one three agents believed said a deck was 1 m across the stage.
///
/// `None` for a piece with no seat — a truss stands on its ends and has no
/// resting pose to print.
///
/// Kept in step with [`free_placement`] by construction: the yaw here is the
/// same [`twist`] of [`LOCAL_FRONT`] onto the house that a direction-less
/// placement of a [`faces_house`] piece takes.
#[must_use]
pub fn resting_footprint<S: NodeSockets + ?Sized>(sockets: &S, node: &Node) -> Option<Footprint> {
    let piece = crate::catalog::find(node.catalog_ref.as_deref()?)?;
    let bounds = sockets.bounds(node)?;
    let floor = crate::venue::root_socket(FLOOR_SOCKET)?;
    let held = sockets
        .sockets(node)
        .into_iter()
        .filter(|s| s.socket_type.polarity().can_be_held())
        .filter(|s| s.socket_type.mates(floor.socket_type))
        .min_by(|a, b| {
            a.normal
                .dot(crate::snap::WORLD_UP)
                .total_cmp(&b.normal.dot(crate::snap::WORLD_UP))
        })?;
    let seat = |yaw: f64| {
        place_on(
            DMat4::IDENTITY,
            &floor,
            &held,
            node.kind,
            SurfacePlacement {
                yaw,
                ..SurfacePlacement::FLUSH
            },
        )
    };
    let yaw = if faces_house(piece) {
        let host_normal = socket_world(&floor).z_axis.truncate().normalize_or_zero();
        let at_rest = seat(0.0).transform_vector3(LOCAL_FRONT).normalize_or_zero();
        twist(at_rest, three_from_facade(DVec3::Y), host_normal)
    } else {
        0.0
    };
    Some(Footprint::of(&seat(yaw), bounds))
}

/// A free placement: the piece is anchored by its footprint centre on a
/// surface, turned so it runs the way the caller pointed.
fn free_placement<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    request: &Request,
    piece: &crate::catalog::Piece,
    probe: &Node,
    announce: &mut Vec<String>,
) -> Result<Plan, Refusal> {
    let parent = match request.on.as_deref() {
        Some(id) => id.to_string(),
        None => scene.graph.root().to_string(),
    };
    let parent_world = scene
        .world(&parent)
        .ok_or_else(|| Refusal::UnknownNode(parent.clone()))?;
    // Which face, and which footing, are one question: a deck's top and its
    // four corners all face up, and only one of them is a joint a CDJ has a
    // half of. Choosing the face without asking what is being put on it is how
    // "put the player on the riser" became "nothing can be mounted here".
    let held = held_sockets(scene, probe);
    let wanted_face = match request.face {
        Some(face) => three_from_facade(DVec3::from(face)).normalize_or_zero(),
        None => three_from_facade(DVec3::Z),
    };
    let faces: Vec<(ResolvedSocket, DVec3)> = scene
        .world_sockets(&parent)
        .into_iter()
        .filter(|(s, _, _)| s.socket_type.polarity().can_host())
        .filter(|(s, _, _)| s.socket_type != SocketType::Grab)
        .map(|(s, _, normal)| (s, normal))
        .collect();
    let mates: Vec<(ResolvedSocket, DVec3)> = faces
        .iter()
        .filter(|(s, _)| held.iter().any(|h| h.socket_type.mates(s.socket_type)))
        .cloned()
        .collect();
    let host = nearest_face(&mates, wanted_face)
        .map(|(s, _)| s.clone())
        .ok_or_else(|| scene.no_face(&parent, &faces))?;
    // A stated `face=` the host does not have is a refusal, not a nearest
    // guess: seating a speaker on whatever happened to point closest is how
    // `face=` came to look like a parameter the room ignores.
    if request.face.is_some() {
        let landed = mates
            .iter()
            .find(|(s, _)| s.name == host.name)
            .map_or(0.0, |(_, n)| n.dot(wanted_face));
        if landed < FACE_MIN_DOT {
            return Err(scene.no_face(&parent, &faces));
        }
    }

    let candidates: Vec<ResolvedSocket> = held
        .into_iter()
        .filter(|s| s.socket_type.mates(host.socket_type))
        .collect();

    let host_world = parent_world * socket_world(&host);
    let host_normal = host_world.z_axis.truncate().normalize_or_zero();
    // Which of the piece's own axes the turn is measured on, and where that
    // axis is meant to end up. A stated `direction=` turns the piece's **run**;
    // an endpoint with no run to speak of is turned by its **front**, which is
    // the only thing anybody means by where a speaker points.
    let (reference, wanted) = match request.direction {
        Some(direction) => (
            run_axis(scene, probe),
            Some(three_from_facade(DVec3::from(direction)).normalize_or_zero()),
        ),
        None if faces_house(piece) => (Some(LOCAL_FRONT), Some(three_from_facade(DVec3::Y))),
        None => (run_axis(scene, probe), None),
    };
    let axis = run_axis(scene, probe);

    // Which footing, and how far round: the two together are the only freedom a
    // surface placement has, so they are chosen together against one measure.
    let mut best: Option<(f64, ResolvedSocket, f64)> = None;
    for held in &candidates {
        let flat = |yaw: f64| {
            place_on(
                parent_world,
                &host,
                held,
                probe.kind,
                SurfacePlacement {
                    u: 0.0,
                    v: 0.0,
                    yaw,
                    trim: request.trim,
                },
            )
        };
        let (yaw, error) = match (wanted, reference) {
            (Some(wanted), Some(reference)) => {
                let at_rest = flat(0.0).transform_vector3(reference).normalize_or_zero();
                let yaw = twist(at_rest, wanted, host_normal);
                let landed = flat(yaw).transform_vector3(reference).normalize_or_zero();
                // A run has no sign — a truss laid along `+u` is the same truss
                // laid along `-u` — but a front does, and a speaker turned to
                // face upstage is not the one that was asked for.
                let along = landed.dot(wanted);
                let error = 1.0
                    - if request.direction.is_some() {
                        along.abs().min(1.0)
                    } else {
                        along.min(1.0)
                    };
                (yaw, error)
            }
            _ => (0.0, 0.0),
        };
        if best.as_ref().is_none_or(|(e, _, _)| error < *e) {
            best = Some((error, held.clone(), yaw));
        }
    }
    let (error, held, yaw) = best.expect("candidates is non-empty");
    // Only a *stated* direction refuses. The house-facing default is a
    // convenience, and a piece whose joint cannot take it stays as it lay.
    if error > DIRECTION_TOLERANCE && request.direction.is_some() {
        return Err(Refusal::ImpossibleTurn {
            wanted: request.direction.unwrap_or([0.0; 3]),
            legal: legal_free_directions(scene, probe, parent_world, &host, &candidates),
        });
    }
    let yaw = if error > DIRECTION_TOLERANCE {
        0.0
    } else {
        yaw
    };

    // `at` is the footprint **centre**, so the seat is solved backwards from
    // where the box ends up rather than from where the socket lands — placing
    // by the socket is what put a tower half a module off its mark.
    //
    // Measured in the **host surface's own plane**, which is what `on=`
    // reframing means: `(u, v)` on a deck top is across that deck. On the
    // venue's floor the host plane's axes *are* the facade axes, so the
    // overwhelmingly common case reads absolutely and the two never disagree
    // where anyone can see it.
    let bounds = scene
        .sockets
        .bounds(probe)
        .unwrap_or_else(|| DAabb::new(DVec3::ZERO, DVec3::ZERO));
    let seat = SurfacePlacement {
        u: 0.0,
        v: 0.0,
        yaw,
        trim: request.trim,
    };
    let at_origin = place_on(parent_world, &host, &held, probe.kind, seat);
    let centre0 = Footprint::of(&at_origin, bounds).at;
    // The host's own mark is its **footprint centre**, not the socket the piece
    // happens to bolt to: `on=deck, at=(0, 0)` is the middle of that deck
    // whether the piece lands on its top or on a corner, and a socket-relative
    // origin put a tower half a deck away from where it was asked for. The axes
    // stay the **facade's**: `+u` is stage right on the floor, on a deck top
    // and on a grid alike. A host's authored tangent is a modelling detail — a
    // deck's happens to run toward the crowd — and letting it turn the caller's
    // plan a quarter turn is exactly the invisible frame this API exists to
    // kill.
    let mark = scene.footprint(&parent).map_or_else(
        || {
            let socket = facade_from_three(host_world.w_axis.truncate());
            [socket.x, socket.y]
        },
        |print| print.at,
    );
    let target = request
        .at
        .map_or(centre0, |at| [mark[0] + at[0], mark[1] + at[1]]);
    let delta = three_from_facade(DVec3::new(
        target[0] - centre0[0],
        target[1] - centre0[1],
        0.0,
    ));
    let in_host = host_world.inverse().transform_vector3(delta);
    let placement = SurfacePlacement {
        u: in_host.x,
        v: in_host.y,
        yaw,
        ..seat
    };
    let world = place_on(parent_world, &host, &held, probe.kind, placement);
    let landed = Footprint::of(&world, bounds);
    if request.at.is_some() {
        let off = (landed.at[0] - target[0]).hypot(landed.at[1] - target[1]);
        if off > 1e-3 {
            announce.push(format!(
                "the surface could not take that mark: centred at ({:.2}, {:.2})",
                landed.at[0], landed.at[1]
            ));
        }
    }

    let run = match request.direction {
        Some(d) => DVec3::from(d).normalize_or_zero(),
        None => axis.map_or(DVec3::ZERO, |a| {
            facade_from_three(world.transform_vector3(a).normalize_or_zero())
        }),
    };
    let mut params: BTreeMap<String, f64> =
        probe.params.iter().map(|(k, v)| (k.into(), v)).collect();
    params.insert("u".to_string(), placement.u);
    params.insert("v".to_string(), placement.v);
    params.insert("trim".to_string(), placement.trim);
    Ok(Plan {
        kind: probe.kind,
        catalog_ref: probe.catalog_ref.clone().unwrap_or_default(),
        label: None,
        params,
        edge: Edge {
            parent,
            my_socket: held.name.clone(),
            their_socket: host.name.clone(),
            roll: yaw,
        },
        parent_roll: None,
        constraint: None,
        announce: Vec::new(),
        at: landed,
        tip: None,
        run: run.to_array(),
        world,
    })
}

/// Every direction a free placement of this piece could run in, for a refusal.
fn legal_free_directions<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    probe: &Node,
    parent_world: DMat4,
    host: &ResolvedSocket,
    candidates: &[ResolvedSocket],
) -> Vec<[f64; 3]> {
    let Some(axis) = run_axis(scene, probe) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for held in candidates {
        let world = place_on(
            parent_world,
            host,
            held,
            probe.kind,
            SurfacePlacement::FLUSH,
        );
        let dir = facade_from_three(world.transform_vector3(axis).normalize_or_zero());
        push_unique(&mut out, dir);
    }
    out
}

fn push_unique(out: &mut Vec<[f64; 3]>, dir: DVec3) {
    let rounded = DVec3::new(round3(dir.x), round3(dir.y), round3(dir.z));
    if !out
        .iter()
        .any(|d| (DVec3::from(*d) - rounded).length() < 1e-6)
    {
        out.push(rounded.to_array());
    }
}

fn round3(v: f64) -> f64 {
    let r = (v * 1000.0).round() / 1000.0;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

/// The turn about `normal` that takes `from` onto `to`, in the plane
/// perpendicular to `normal`.
fn twist(from: DVec3, to: DVec3, normal: DVec3) -> f64 {
    let flat = |v: DVec3| (v - normal * v.dot(normal)).normalize_or_zero();
    let (a, b) = (flat(from), flat(to));
    if a.length_squared() < 0.5 || b.length_squared() < 0.5 {
        return 0.0;
    }
    a.cross(b).dot(normal).atan2(a.dot(b))
}

/// One node's socket by name, the root's synthesized floor included.
fn host_socket<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    node: &str,
    name: &str,
) -> Result<ResolvedSocket, Refusal> {
    scene
        .world_sockets(node)
        .into_iter()
        .find(|(s, _, _)| s.name == name)
        .map(|(s, _, _)| s)
        .ok_or_else(|| Refusal::UnknownNode(node.to_string()))
}

/// The socket frame [`place_on`] mates against, in the host node's own space.
fn socket_world(socket: &ResolvedSocket) -> DMat4 {
    let z = socket.normal.normalize_or_zero();
    let x = socket.tangent.normalize_or_zero();
    let x = (x - z * z.dot(x)).normalize_or_zero();
    let y = z.cross(x).normalize_or_zero();
    DMat4::from_cols(
        x.extend(0.0),
        y.extend(0.0),
        z.extend(0.0),
        socket.position.extend(1.0),
    )
}

/// The quarter turns a bolted joint admits. A truss end is a square section on
/// a fixed bolt circle, so this is the joint's whole freedom.
const QUARTER_TURNS: [f64; 4] = [
    0.0,
    std::f64::consts::FRAC_PI_2,
    std::f64::consts::PI,
    -std::f64::consts::FRAC_PI_2,
];

/// A chained piece: bolted onto a tip, turned so the chain leaves the way the
/// caller pointed.
fn chained<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    request: &Request,
    piece: &crate::catalog::Piece,
    probe: &Node,
    tip: &Tip,
    announce: &mut Vec<String>,
) -> Result<Plan, Refusal> {
    let parent = tip.node.clone();
    let mut parent_world = scene
        .world(&parent)
        .ok_or_else(|| Refusal::UnknownNode(parent.clone()))?;
    let host = host_socket(scene, &parent, &tip.socket)?;
    let mut parent_roll = None;

    // A joint whose exit the *next* piece chooses: re-roll it now, before the
    // child mates it. This is the corner's whole contract — `add("corner")`
    // says "turn here" and the direction on the piece after it says where to.
    if let Some(direction) = request.direction {
        if let Some(roll) = exit_roll(scene, &parent, &tip.socket, DVec3::from(direction))? {
            parent_roll = Some(roll.0);
            parent_world = roll.1;
        }
    }

    let held = chain_socket(scene, piece, probe, &host, &parent)?;
    let host_world = parent_world * socket_world(&host);
    let out = host_world.z_axis.truncate().normalize_or_zero();

    let roll = if is_hinge(piece) {
        hinge_roll(
            scene,
            request,
            probe,
            parent_world,
            &host,
            &held,
            out,
            announce,
        )?
    } else if let Some(angle) = request.angle {
        // The joint itself is the hinge. A rail chain articulates at its posts
        // — that is what a crowd barrier does round a corner — so the turn is
        // the mate's own roll, in the steps the socket declares, and no piece
        // is inserted to carry it.
        articulated_roll(piece, &host, angle, announce)?
    } else {
        // A stick bolted to an end runs straight out of it; the only thing a
        // roll would change is which way up the section is, which nothing here
        // means by `direction`.
        if let Some(direction) = request.direction {
            let wanted = three_from_facade(DVec3::from(direction)).normalize_or_zero();
            if !is_corner(piece) && wanted.dot(out) < 1.0 - DIRECTION_TOLERANCE {
                return Err(Refusal::ImpossibleTurn {
                    wanted: direction,
                    legal: vec![facade_from_three(out).to_array()],
                });
            }
        }
        0.0
    };

    let world = place_on(
        parent_world,
        &host,
        &held,
        probe.kind,
        SurfacePlacement {
            yaw: roll,
            ..SurfacePlacement::FLUSH
        },
    );
    let bounds = scene
        .sockets
        .bounds(probe)
        .unwrap_or_else(|| DAabb::new(DVec3::ZERO, DVec3::ZERO));
    // A joint is a box, and a box on the run is length the run does not get.
    // Said out loud because it is the arithmetic nobody does: a leg, a corner,
    // a beam of the nominal width and a corner puts the far leg half a corner
    // past where the width says it is.
    if is_corner(piece) {
        announce.push(format!(
            "the corner adds {:.2} m along the run",
            span_along(&world, bounds, out)
        ));
    }
    Ok(Plan {
        kind: probe.kind,
        catalog_ref: probe.catalog_ref.clone().unwrap_or_default(),
        label: None,
        params: probe
            .params
            .iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        edge: Edge {
            parent,
            my_socket: held.name.clone(),
            their_socket: host.name.clone(),
            roll,
        },
        parent_roll,
        constraint: None,
        announce: Vec::new(),
        at: Footprint::of(&world, bounds),
        tip: None,
        // Where the chain goes on from here. An articulated joint turns about
        // its own vertical, so the run leaves turned by exactly the angle the
        // caller asked for — reporting the unturned normal would send the next
        // piece's tip search the wrong way round the corner.
        run: facade_from_three(if held.mode == crate::sockets::SocketMode::Upright {
            rotate_about(out, DVec3::Y, roll)
        } else {
            out
        })
        .to_array(),
        world,
    })
}

/// The roll a turn at an articulated joint spends, radians.
///
/// The joint's own freedom, read off the socket: a rail post steps in whole
/// degrees and a bolted end does not step at all. Off-step is snapped and
/// announced like every other quantity here; past the limit is refused, because
/// a chain built at a right angle when a hundred and twenty was asked for is
/// not the shape anybody wanted.
fn articulated_roll(
    piece: &crate::catalog::Piece,
    host: &ResolvedSocket,
    angle: f64,
    announce: &mut Vec<String>,
) -> Result<f64, Refusal> {
    let crate::sockets::RollFreedom::Steps(step) = host.roll else {
        return Err(Refusal::NoTurn {
            piece: piece.short.to_string(),
            joint: host.socket_type.as_str(),
        });
    };
    let turned = (angle / step).round() * step;
    if (turned - angle).abs() > 1e-9 {
        announce.push(format!(
            "{angle:.1}deg is not a whole {step:.0}deg step at this joint: turned {turned:.1}deg"
        ));
    }
    Ok(turned.to_radians())
}

/// The half of the joint the new piece meets its host by.
///
/// The generated families each have one upstream end and the rest are ways out,
/// so "which end goes on first" is a property of the family rather than a
/// choice: a stick enters by `end_a`, a corner block by its `-x` way, a hinge by
/// its fixed leaf. Anything else takes the first socket that mates, which is the
/// same rule the snap search scores with.
fn chain_socket<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    piece: &crate::catalog::Piece,
    probe: &Node,
    host: &ResolvedSocket,
    parent: &str,
) -> Result<ResolvedSocket, Refusal> {
    let mates: Vec<ResolvedSocket> = held_sockets(scene, probe)
        .into_iter()
        .filter(|s| s.socket_type.mates(host.socket_type))
        .collect();
    let preferred = if is_stick(piece) {
        Some("end_a")
    } else if is_corner(piece) {
        Some("face_-x")
    } else if is_hinge(piece) {
        Some("leaf_fixed")
    } else {
        None
    };
    if let Some(name) = preferred {
        if let Some(found) = mates.iter().find(|s| s.name == name) {
            return Ok(found.clone());
        }
    }
    mates.into_iter().next().ok_or_else(|| Refusal::NoJoint {
        piece: piece.short.to_string(),
        host: scene.name_of(parent),
        joint: host.socket_type.as_str(),
        turnable: match host.roll {
            crate::sockets::RollFreedom::Steps(step) => Some(step),
            _ => None,
        },
    })
}

/// Re-roll a joint whose exit the caller has just named, and report the parent
/// frame that roll produces.
///
/// `None` when the host end's direction is not a choice — a straight run leaves
/// the way it points, and a hinge fixed its own exit when it was built.
fn exit_roll<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    node: &str,
    socket: &str,
    direction: DVec3,
) -> Result<Option<(f64, DMat4)>, Refusal> {
    let row = scene
        .graph
        .node(node)
        .ok_or_else(|| Refusal::UnknownNode(node.to_string()))?;
    let is_turn = row
        .catalog_ref
        .as_deref()
        .and_then(crate::catalog::find)
        .is_some_and(is_corner);
    if !is_turn {
        return Ok(None);
    }
    let Some(edge) = scene.graph.edge(node) else {
        return Ok(None);
    };
    let grandparent = scene
        .world(&edge.parent)
        .ok_or_else(|| Refusal::UnknownNode(edge.parent.clone()))?;
    let grandparent_socket = host_socket(scene, &edge.parent, &edge.their_socket)?;
    let held = scene
        .sockets
        .sockets(row)
        .into_iter()
        .find(|s| s.name == edge.my_socket)
        .ok_or_else(|| Refusal::UnknownNode(node.to_string()))?;
    let local = scene
        .sockets
        .sockets(row)
        .into_iter()
        .find(|s| s.name == socket)
        .ok_or_else(|| Refusal::UnknownNode(node.to_string()))?;

    let wanted = three_from_facade(direction).normalize_or_zero();
    let mut legal = Vec::new();
    let mut best: Option<(f64, f64, DMat4)> = None;
    for roll in QUARTER_TURNS {
        let world = place_on(
            grandparent,
            &grandparent_socket,
            &held,
            row.kind,
            SurfacePlacement {
                yaw: roll,
                ..SurfacePlacement::FLUSH
            },
        );
        let out = world.transform_vector3(local.normal).normalize_or_zero();
        push_unique(&mut legal, facade_from_three(out));
        let error = 1.0 - out.dot(wanted).min(1.0);
        if best.as_ref().is_none_or(|(e, _, _)| error < *e) {
            best = Some((error, roll, world));
        }
    }
    let (error, roll, world) = best.expect("four quarter turns");
    if error > DIRECTION_TOLERANCE {
        return Err(Refusal::ImpossibleTurn {
            wanted: direction.to_array(),
            legal,
        });
    }
    Ok(Some((roll, world)))
}

/// The quarter turn that puts a hinge's pin on the stated axis with the stated
/// sign.
///
/// The generator's pin is fixed in the hinge's own frame and the joint turns in
/// quarters, so the four rolls are the four ways the pin can lie perpendicular
/// to the run — two axes, two signs. Rather than deriving which is which
/// through three frames, each candidate is **measured**: the leaf's out
/// direction is compared against the right-hand-rule turn of the incoming run.
/// That makes the sign convention true by construction instead of by argument.
#[allow(clippy::too_many_arguments)]
fn hinge_roll<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    request: &Request,
    probe: &Node,
    parent_world: DMat4,
    host: &ResolvedSocket,
    held: &ResolvedSocket,
    incoming: DVec3,
    announce: &mut Vec<String>,
) -> Result<f64, Refusal> {
    let Some(axis) = request.axis else {
        return Err(Refusal::Incomplete(
            "a hinge needs axis= (what it turns about) and angle= (how far, \
             right-hand rule, in 5deg steps to +-90)"
                .into(),
        ));
    };
    if request.direction.is_some() {
        return Err(Refusal::WrongKind {
            expected: "axis= for a hinge",
            got: "direction=",
        });
    }
    let angle = quantize_hinge(request.angle.unwrap_or(0.0));
    let axis3 = three_from_facade(DVec3::from(axis)).normalize_or_zero();
    if axis3.length_squared() < 0.5 {
        return Err(Refusal::BadAxis {
            wanted: axis,
            plane: perpendiculars(incoming),
        });
    }
    if axis3.dot(incoming).abs() > 1e-3 {
        return Err(Refusal::BadAxis {
            wanted: axis,
            plane: perpendiculars(incoming),
        });
    }
    let wanted = rotate_about(incoming, axis3, angle.to_radians());

    let exit = leaf_socket(scene, probe, &held.name);
    let mut best: Option<(f64, f64)> = None;
    for roll in QUARTER_TURNS {
        let world = place_on(
            parent_world,
            host,
            held,
            probe.kind,
            SurfacePlacement {
                yaw: roll,
                ..SurfacePlacement::FLUSH
            },
        );
        let out = exit
            .as_ref()
            .map(|s| world.transform_vector3(s.normal).normalize_or_zero())
            .unwrap_or(incoming);
        let error = 1.0 - out.dot(wanted).min(1.0);
        if best.as_ref().is_none_or(|(e, _)| error < *e) {
            best = Some((error, roll));
        }
    }
    let (error, roll) = best.expect("four quarter turns");
    if error > DIRECTION_TOLERANCE {
        return Err(Refusal::BadAxis {
            wanted: axis,
            plane: perpendiculars(incoming),
        });
    }
    if (angle - request.angle.unwrap_or(0.0)).abs() > 1e-9 {
        // Already announced by `piece_params`; the roll is what made it real.
        let _ = announce;
    }
    Ok(roll)
}

/// The socket a hinge leaves by: the neutral end that is not the one it entered
/// on.
fn leaf_socket<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    probe: &Node,
    entered: &str,
) -> Option<ResolvedSocket> {
    scene
        .sockets
        .sockets(probe)
        .into_iter()
        .find(|s| s.socket_type.polarity() == Polarity::Neutral && s.name != entered)
}

/// The two axes a run admits a turn about, both signs, as facade vectors.
fn perpendiculars(run: DVec3) -> Vec<[f64; 3]> {
    let run = run.normalize_or_zero();
    let seed = if run.dot(DVec3::Y).abs() < 0.9 {
        DVec3::Y
    } else {
        DVec3::X
    };
    let a = run.cross(seed).normalize_or_zero();
    let b = run.cross(a).normalize_or_zero();
    let mut out = Vec::new();
    for v in [a, -a, b, -b] {
        push_unique(&mut out, facade_from_three(v));
    }
    out
}

/// Rodrigues: `v` turned `radians` about `axis`, right-handed.
fn rotate_about(v: DVec3, axis: DVec3, radians: f64) -> DVec3 {
    let k = axis.normalize_or_zero();
    let (s, c) = radians.sin_cos();
    (v * c + k.cross(v) * s + k * k.dot(v) * (1.0 - c)).normalize_or_zero()
}

/// The free end a plan leaves behind, in the frame the plan will land in.
///
/// A piece with one way left has one tip. A piece with several — a stick laid
/// flat, whose two ends are both open — has its tip picked by the way it
/// **runs**: the chain grows downstream, and the run is the only thing that
/// says which end that is.
fn leftover_tip<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    plan: &Plan,
    probe: &Node,
) -> Option<Tip> {
    let free: Vec<ResolvedSocket> = scene
        .sockets
        .sockets(probe)
        .into_iter()
        .filter(|s| s.socket_type.polarity() == Polarity::Neutral)
        .filter(|s| s.name != plan.edge.my_socket)
        .collect();
    let run = three_from_facade(DVec3::from(plan.run)).normalize_or_zero();
    let socket = if free.len() == 1 {
        free.into_iter().next()?
    } else if run.length_squared() > 0.5 {
        free.into_iter().max_by(|a, b| {
            let along = |s: &ResolvedSocket| {
                plan.world
                    .transform_vector3(s.normal)
                    .normalize_or_zero()
                    .dot(run)
            };
            along(a).total_cmp(&along(b))
        })?
    } else {
        return None;
    };
    let at = plan.world.transform_point3(socket.position);
    let direction = plan
        .world
        .transform_vector3(socket.normal)
        .normalize_or_zero();
    Some(Tip {
        node: String::new(),
        socket: socket.name,
        direction: facade_from_three(direction).to_array(),
        at: facade_from_three(at).to_array(),
    })
}

/// The pose a plan will produce for `probe`, re-solved after its span changed.
fn pose_of<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    plan: &Plan,
    probe: &Node,
) -> Result<DMat4, Refusal> {
    let parent_world = scene
        .world(&plan.edge.parent)
        .ok_or_else(|| Refusal::UnknownNode(plan.edge.parent.clone()))?;
    let host = host_socket(scene, &plan.edge.parent, &plan.edge.their_socket)?;
    let held = scene
        .sockets
        .sockets(probe)
        .into_iter()
        .find(|s| s.name == plan.edge.my_socket)
        .ok_or_else(|| Refusal::UnknownNode(plan.edge.parent.clone()))?;
    Ok(place_on(
        parent_world,
        &host,
        &held,
        probe.kind,
        SurfacePlacement {
            yaw: plan.edge.roll,
            ..SurfacePlacement::FLUSH
        },
    ))
}

/// How long a run has to be to land its far end on `target`.
///
/// Length is the only thing a straight run can spend, so the target has to lie
/// on the line it leaves along; the refusal reports how far off it is, which is
/// the number that says whether a hinge belongs before it.
fn meet<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    plan: &Plan,
    target: &str,
) -> Result<f64, Refusal> {
    let parent_world = scene
        .world(&plan.edge.parent)
        .ok_or_else(|| Refusal::UnknownNode(plan.edge.parent.clone()))?;
    let host = host_socket(scene, &plan.edge.parent, &plan.edge.their_socket)?;
    let host_world = parent_world * socket_world(&host);
    let origin = host_world.w_axis.truncate();
    let along = host_world.z_axis.truncate().normalize_or_zero();

    let ends = scene.tips(target);
    if ends.is_empty() {
        return Err(Refusal::NoTip {
            node: target.to_string(),
        });
    }
    let mut best: Option<(f64, f64)> = None;
    for end in ends {
        let at = three_from_facade(DVec3::from(end.at));
        let reach = (at - origin).dot(along);
        let offset = (at - origin - along * reach).length();
        if best.as_ref().is_none_or(|(o, _)| offset < *o) {
            best = Some((offset, reach));
        }
    }
    let (offset, reach) = best.expect("ends is non-empty");
    if offset > MODULE_M / 2.0 || reach <= 0.0 {
        return Err(Refusal::CannotMeet {
            target: target.to_string(),
            offset_m: offset,
        });
    }
    Ok(quantize_length(reach))
}

/// The far-end check a `to=` run earns, when its end really did land on the
/// target's.
fn far_end<S: NodeSockets + ?Sized>(
    scene: &Scene<'_, S>,
    plan: &Plan,
    probe: &Node,
    target: &str,
) -> Option<(String, String, String)> {
    let mine = leftover_tip(scene, plan, probe)?;
    let at = three_from_facade(DVec3::from(mine.at));
    let end = scene.tips(target).into_iter().min_by(|a, b| {
        let d = |t: &Tip| (three_from_facade(DVec3::from(t.at)) - at).length();
        d(a).total_cmp(&d(b))
    })?;
    let gap = (three_from_facade(DVec3::from(end.at)) - at).length();
    (gap <= crate::venue::CONSTRAINT_TOLERANCE_M)
        .then(|| (mine.socket, target.to_string(), end.socket))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sockets::resolve_socket;
    use crate::venue::{resolve, Params, ResolvedVenue, VenueGraph};
    use std::collections::HashMap;

    /// The geometry supply, built the way the real one is: the **catalog's own**
    /// authored sockets, resolved against a measured box. Only the measurement
    /// is a stand-in, so a question asked here is asked of the same pieces a
    /// venue holds. Procedural pieces have no authored sockets by design and
    /// are therefore not answerable here — see `luma_render::catalog`.
    struct Meshes(HashMap<&'static str, DAabb>);

    impl Meshes {
        /// Boxes measured off the shipped GLBs, to the centimetre.
        fn shipped() -> Self {
            let box_of = |x: f64, y: f64, z: f64| {
                DAabb::new(
                    DVec3::new(-x / 2.0, -y / 2.0, -z / 2.0),
                    DVec3::new(x / 2.0, y / 2.0, z / 2.0),
                )
            };
            Meshes(HashMap::from([
                (
                    "stage_lab/stage_praticavel_2x1x1.glb",
                    box_of(2.0, 1.01, 1.0),
                ),
                ("stage_lab/stage_praticavel_1x1.glb", box_of(1.0, 0.3, 1.0)),
                ("stage_lab/speaker_jbl_vtx_v20.glb", box_of(0.91, 0.28, 0.4)),
                ("stage_lab/guardrail.glb", box_of(0.4, 1.0, 2.14)),
                ("assembly/dj_booth", box_of(2.0, 1.17, 1.0)),
            ]))
        }
    }

    impl NodeSockets for Meshes {
        fn sockets(&self, node: &Node) -> Vec<ResolvedSocket> {
            let Some(piece) = node.catalog_ref.as_deref().and_then(crate::catalog::find) else {
                return Vec::new();
            };
            let Some(bounds) = self.bounds(node) else {
                return Vec::new();
            };
            piece
                .sockets
                .iter()
                .map(|def| resolve_socket(def, &bounds))
                .collect()
        }

        fn bounds(&self, node: &Node) -> Option<DAabb> {
            self.0.get(node.catalog_ref.as_deref()?).copied()
        }
    }

    /// A room with a floor and nothing on it.
    fn room() -> VenueGraph {
        VenueGraph::new(Node {
            id: "root".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        })
    }

    /// Compile one request against a graph and write it in as `id`, answering
    /// with the plan — the two steps every caller of this module takes.
    fn build(
        graph: &mut VenueGraph,
        solved: &mut ResolvedVenue,
        supply: &Meshes,
        id: &str,
        request: &Request,
    ) -> Result<Plan, Refusal> {
        let plan = compile(&Scene::new(graph, solved, supply), request)?;
        plan.apply(graph, id, supply)
            .expect("the plan is this graph's");
        *solved = resolve(graph, supply);
        Ok(plan)
    }

    fn request(piece: &str) -> Request {
        Request {
            piece: piece.into(),
            ..Request::default()
        }
    }

    /// The bug three agents hit first: `on=` landed a piece half a deck away in
    /// plan and sunk into its host, because a deck's four top *edges* inherit
    /// their normal from the top face and won the "which face points up" tie.
    #[test]
    fn a_piece_placed_on_a_host_sits_on_its_top_centred_on_the_hosts_mark() {
        let supply = Meshes::shipped();
        let mut graph = room();
        let mut solved = resolve(&graph, &supply);
        let deck = build(
            &mut graph,
            &mut solved,
            &supply,
            "deck",
            &Request {
                at: Some([3.0, 2.0]),
                ..request("deck")
            },
        )
        .expect("a deck on the floor");
        assert!((deck.at.at[0] - 3.0).abs() < 1e-9 && (deck.at.at[1] - 2.0).abs() < 1e-9);

        let on = build(
            &mut graph,
            &mut solved,
            &supply,
            "riser",
            &Request {
                on: Some("deck".into()),
                at: Some([0.0, 0.0]),
                ..request("deck_1x1")
            },
        )
        .expect("a deck on a deck");
        // `at=(0, 0)` on a host is the host's own footprint centre, on **both**
        // axes.
        assert!(
            (on.at.at[0] - deck.at.at[0]).abs() < 1e-9
                && (on.at.at[1] - deck.at.at[1]).abs() < 1e-9,
            "landed at {:?}, host is at {:?}",
            on.at.at,
            deck.at.at
        );
        // And it stands *on* the top rather than inside it.
        let host_top = deck.at.z + deck.at.size[2] / 2.0;
        let bottom = on.at.z - on.at.size[2] / 2.0;
        assert!(
            (bottom - host_top).abs() < 1e-9,
            "its underside is at {bottom}, the host's top at {host_top}"
        );
        assert_eq!(on.edge.their_socket, "top");
    }

    /// An endpoint has a front, and nobody who puts a speaker down means it to
    /// face the back wall.
    #[test]
    fn an_endpoint_faces_the_house_with_no_direction_stated() {
        let supply = Meshes::shipped();
        let mut graph = room();
        let mut solved = resolve(&graph, &supply);
        let speaker = build(
            &mut graph,
            &mut solved,
            &supply,
            "pa",
            &Request {
                at: Some([4.0, 0.0]),
                ..request("vtx_v20")
            },
        )
        .expect("a speaker on the floor");
        let front = facade_from_three(
            speaker
                .world
                .transform_vector3(LOCAL_FRONT)
                .normalize_or_zero(),
        );
        assert!(front.y > 0.99, "the box faces {front:?}, not the crowd");
        // Which is also the box being wider across the stage than it is deep.
        assert!(speaker.at.size[0] > speaker.at.size[1]);
    }

    /// A face the host does not have is a refusal, not a nearest guess — and
    /// the refusal names the node the way a reader knows it.
    #[test]
    fn a_face_nothing_has_is_refused_by_name() {
        let supply = Meshes::shipped();
        let mut graph = room();
        let mut solved = resolve(&graph, &supply);
        build(
            &mut graph,
            &mut solved,
            &supply,
            "booth",
            &Request {
                at: Some([0.0, 0.0]),
                label: Some("the booth".into()),
                ..request("dj_booth")
            },
        )
        .expect("a booth on the floor");
        let refusal = compile(
            &Scene::new(&graph, &solved, &supply),
            &Request {
                on: Some("booth".into()),
                at: Some([0.0, 0.0]),
                ..request("deck_1x1")
            },
        )
        .expect_err("the booth hosts nothing");
        let said = refusal.to_string();
        assert!(said.contains("the booth"), "{said}");
        assert!(said.contains("dj_booth"), "{said}");
        assert!(said.contains("carries its own"), "{said}");
    }

    /// A guardrail chain turns at its posts, and the post is the only hinge it
    /// has: `add("hinge")` there refuses with the fix in the message.
    #[test]
    fn a_rail_chain_turns_at_its_post_and_says_so_when_it_cannot() {
        let supply = Meshes::shipped();
        let mut graph = room();
        let mut solved = resolve(&graph, &supply);
        let rail = build(
            &mut graph,
            &mut solved,
            &supply,
            "rail",
            &Request {
                at: Some([0.0, 0.0]),
                ..request("guardrail")
            },
        )
        .expect("a rail on the floor");
        let tip = rail.tip_at("rail").expect("a rail has two ends");

        let turned = compile(
            &Scene::new(&graph, &solved, &supply),
            &Request {
                from: Some(tip.clone()),
                angle: Some(32.0),
                ..request("guardrail")
            },
        )
        .expect("a rail turns at its post");
        // Off-step is snapped and announced, as every other quantity here is.
        assert!(
            turned.announce.iter().any(|line| line.contains("30.0deg")),
            "{:?}",
            turned.announce
        );
        // And the run leaves turned, so the next piece grows the right way —
        // a positive angle counterclockwise about `+z`, the same right-hand
        // rule a hinge takes, which swings a rail running toward the crowd
        // round toward stage left.
        let run = DVec3::from(turned.run);
        assert!(run.z.abs() < 1e-9, "{run:?}");
        assert!((run.y - 30f64.to_radians().cos()).abs() < 1e-9, "{run:?}");
        assert!((run.x + 30f64.to_radians().sin()).abs() < 1e-9, "{run:?}");

        // Past the joint's limit is a refusal naming the limit.
        let far = compile(
            &Scene::new(&graph, &solved, &supply),
            &Request {
                from: Some(tip.clone()),
                angle: Some(120.0),
                ..request("guardrail")
            },
        )
        .expect_err("a post does not fold back on itself");
        assert!(far.to_string().contains("+-90deg"), "{far}");

        // And a piece with no half that bolts to a rail end refuses with the
        // turn as the fix, rather than with a truncated list of nothing.
        let wrong = compile(
            &Scene::new(&graph, &solved, &supply),
            &Request {
                from: Some(tip),
                axis: Some([0.0, 0.0, 1.0]),
                angle: Some(30.0),
                ..request("hinge")
            },
        )
        .expect_err("a hinge is not a rail post");
        let said = wrong.to_string();
        assert!(said.contains("rail_end"), "{said}");
        assert!(said.contains("angle="), "{said}");
        assert!(!said.ends_with("turns to "), "{said}");
    }

    /// A fixture has no measurable box and every one of them used to fall out
    /// of `extent`, which answered `None` for a room full of lights.
    #[test]
    fn a_node_with_no_measurable_box_still_spans_the_point_it_hangs_at() {
        let supply = Meshes::shipped();
        let mut graph = room();
        let mut solved = resolve(&graph, &supply);
        build(
            &mut graph,
            &mut solved,
            &supply,
            "deck",
            &Request {
                at: Some([2.0, 0.0]),
                ..request("deck")
            },
        )
        .expect("a deck on the floor");
        // A node the supply cannot measure: no catalog ref, so no box.
        graph.insert_placed(
            Node {
                id: "head".into(),
                kind: NodeKind::Fixture,
                catalog_ref: None,
                label: None,
                params: Params::default(),
            },
            crate::venue::Edge {
                parent: "deck".into(),
                my_socket: "top".into(),
                their_socket: "top".into(),
                roll: 0.0,
            },
        );
        solved = resolve(&graph, &supply);
        let scene = Scene::new(&graph, &solved, &supply);
        let span = scene.extent(["head"]).expect("a point is an extent");
        assert_eq!(span.count, 1);
        assert!(span.size.iter().all(|s| s.abs() < 1e-9));
    }

    #[test]
    fn the_facade_frame_is_world_space_under_another_name() {
        // Toward the crowd is house, which is world +y; up is world +z.
        let crowd = three_from_facade(DVec3::new(0.0, 1.0, 0.0));
        assert!(crowd.abs_diff_eq(DVec3::new(0.0, 0.0, -1.0), 1e-12));
        let up = three_from_facade(DVec3::Z);
        assert!(up.abs_diff_eq(DVec3::Y, 1e-12));
        for v in [DVec3::X, DVec3::Y, DVec3::Z, DVec3::new(1.0, -2.0, 3.0)] {
            assert!(facade_from_three(three_from_facade(v)).abs_diff_eq(v, 1e-12));
        }
    }

    #[test]
    fn lengths_snap_to_the_module() {
        assert!((quantize_length(7.2) - 7.0).abs() < 1e-12);
        assert!((quantize_length(7.3) - 7.5).abs() < 1e-12);
        // Never nothing: a run of no length is not a run.
        assert!((quantize_length(0.01) - MODULE_M).abs() < 1e-12);
    }

    #[test]
    fn hinge_angles_snap_to_whole_steps_and_nothing_else() {
        assert!((quantize_hinge(31.0) - 30.0).abs() < 1e-12);
        assert!((quantize_hinge(-33.0) + 35.0).abs() < 1e-12);
        // No clamp: a turn past the limit is refused upstream, and rounding it
        // to the limit here would be this function deciding the shape.
        assert!((quantize_hinge(120.0) - 120.0).abs() < 1e-12);
        assert!((quantize_hinge(-120.0) + 120.0).abs() < 1e-12);
    }

    /// A hinge asked past a quarter turn used to build a right angle and
    /// *announce* it, which is a shape nobody asked for reported as a courtesy.
    /// Snapping is for legal steps; a turn the joint cannot make is a refusal,
    /// and it is the same refusal a rail post gives.
    #[test]
    fn a_turn_past_the_limit_is_refused_rather_than_quietly_squared_off() {
        let supply = Meshes::shipped();
        let graph = room();
        let solved = resolve(&graph, &supply);
        let scene = Scene::new(&graph, &solved, &supply);
        let refused = compile(
            &scene,
            &Request {
                axis: Some([0.0, 0.0, 1.0]),
                angle: Some(120.0),
                ..request("hinge")
            },
        )
        .expect_err("a hinge does not fold past a quarter turn");
        let said = refused.to_string();
        assert!(said.contains("angle=120deg"), "{said}");
        assert!(said.contains("+-90deg"), "{said}");
        assert!(said.contains("chain another piece"), "{said}");
        // The legal end of the range still compiles as far as the geometry.
        assert!(!matches!(
            compile(
                &scene,
                &Request {
                    axis: Some([0.0, 0.0, 1.0]),
                    angle: Some(90.0),
                    ..request("hinge")
                },
            ),
            Err(Refusal::TurnTooFar { .. })
        ));
    }

    /// The sign convention, asserted rather than argued: a positive angle about
    /// `+z` turns a run counterclockwise seen from above — stage right swings
    /// toward the crowd.
    #[test]
    fn a_positive_hinge_turns_counterclockwise_about_its_axis() {
        let run = three_from_facade(DVec3::X);
        let axis = three_from_facade(DVec3::Z);
        let turned = facade_from_three(rotate_about(run, axis, 30f64.to_radians()));
        assert!(turned.x > 0.0 && turned.y > 0.0, "turned {turned:?}");
        assert!((turned.x - 30f64.to_radians().cos()).abs() < 1e-9);
        assert!((turned.y - 30f64.to_radians().sin()).abs() < 1e-9);
        // And a negative angle is the mirror of it.
        let back = facade_from_three(rotate_about(run, axis, -30f64.to_radians()));
        assert!((back.x - turned.x).abs() < 1e-9 && (back.y + turned.y).abs() < 1e-9);
    }

    #[test]
    fn a_perpendicular_plane_holds_four_axes_and_excludes_the_run() {
        let run = three_from_facade(DVec3::X);
        let plane = perpendiculars(run);
        assert_eq!(plane.len(), 4);
        for axis in plane {
            let axis = three_from_facade(DVec3::from(axis));
            assert!(axis.dot(run).abs() < 1e-9, "{axis:?} is not perpendicular");
        }
    }

    #[test]
    fn a_footprint_is_the_box_in_facade_axes() {
        // A one-metre cube at the three-space origin, lifted two metres.
        let bounds = DAabb::new(DVec3::splat(-0.5), DVec3::splat(0.5));
        let world = DMat4::from_translation(DVec3::new(1.0, 2.0, 3.0));
        let print = Footprint::of(&world, bounds);
        // three (1, 2, 3) is facade (1, -3, 2).
        assert!((print.at[0] - 1.0).abs() < 1e-12);
        assert!((print.at[1] + 3.0).abs() < 1e-12);
        assert!((print.z - 2.0).abs() < 1e-12);
        assert!(print.size.iter().all(|s| (s - 1.0).abs() < 1e-12));
    }

    #[test]
    fn an_extent_spans_every_footprint() {
        let cube = DAabb::new(DVec3::splat(-0.5), DVec3::splat(0.5));
        let extent = Extent::of([
            (DMat4::from_translation(DVec3::new(-4.0, 0.0, 0.0)), cube),
            (DMat4::from_translation(DVec3::new(4.0, 0.0, 0.0)), cube),
        ])
        .expect("two boxes");
        assert_eq!(extent.count, 2);
        assert!((extent.size[0] - 9.0).abs() < 1e-12);
        assert!(extent.centre[0].abs() < 1e-12);
        assert!(Extent::of([]).is_none());
    }

    #[test]
    fn a_twist_is_the_turn_between_two_directions() {
        let up = DVec3::Y;
        let a = DVec3::X;
        let b = DVec3::Z;
        // three-space +X onto +Z about +Y is a quarter turn the negative way.
        assert!((twist(a, b, up) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(twist(a, a, up).abs() < 1e-12);
    }
}
