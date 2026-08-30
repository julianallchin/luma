//! What the cursor is holding, and what putting it down means.
//!
//! # One state machine
//!
//! [`Hand`] is the whole of the builder's transient state. Arming a palette
//! piece, dragging a fixture out of the tray, duplicating a wing and running a
//! truss out of a socket are four gestures that all end in a **placement**, and
//! they are mutually exclusive: a hand that was both "armed with a truss" and
//! "extending from a socket" could answer the next click two ways. So they are
//! variants of one enum rather than four booleans, and the only way to leave
//! one is through [`Hand::drop`] or [`Hand::clear`].
//!
//! # Two radii, and why they are not one
//!
//! Snapping is *hysteretic*: [`luma_scene::snap::ATTACH_THRESHOLD`] (0.5 m) is
//! how close the cursor must come for a joint to take hold, and
//! [`luma_scene::snap::DETACH_THRESHOLD`] (0.8 m) is how far it must go for
//! that joint to let go. Both are **world-space metres, measured at the host
//! socket**, because a screen-space radius would make a snap depend on the
//! camera and a piece would come loose when someone zoomed out. The one number
//! that *is* screen-space is [`SOCKET_MARK_PICK_PX`], which is only how big a
//! socket bead's hitbox is — an aiming affordance, never an acceptance test.
//!
//! # The ladder
//!
//! [`Room::land`] is socket → surface → grid, and it is
//! [`luma_scene::snap::solve_snap`] doing the search: pass 1 discrete sockets,
//! pass 2 the surface under the cursor, pass 3 the ground plane. This module
//! adds the two things the solver deliberately does not have — the latch that
//! makes it hysteretic, and the translation from a solved pose into the verb
//! that would commit it.

use std::collections::{BTreeMap, HashMap};

use glam::{DMat4, DVec3};
use luma_render::catalog::{VenueSockets, BASE_SOCKET, SEAT_SOCKET};
use luma_scene::coords;
use luma_scene::snap::{
    solve_snap, ScenePiece, SnapInput, SnapMatch, SnapSurface, ATTACH_THRESHOLD, DETACH_THRESHOLD,
};
use luma_scene::sockets::{Polarity, ResolvedSocket, SocketKind, SocketType};
use luma_scene::venue::{
    invert_placement, place_on, root_socket, NodeKind, NodeSockets, SurfacePlacement, VenueGraph,
    FLOOR_SOCKET,
};

/// How far from a socket bead's centre, in pixels, a press still counts as
/// aiming at it. An aiming radius only — acceptance is the world-space pair
/// above.
pub(crate) const SOCKET_MARK_PICK_PX: f32 = 11.0;

/// The step a truss span is quantized to, in metres. The generator quantizes
/// to whole panels of its own; this is the step the *builder* offers, and the
/// design doc names it.
pub(crate) const LENGTH_STEP_M: f64 = 0.5;

/// What a ray that hit nothing leaves in front of the cursor.
pub(crate) const STUB_LENGTH_M: f64 = 0.5;

/// The floor grid, in metres — the ladder's third rung, and the reason it is
/// called a grid rather than a plane.
///
/// A free placement on the venue's own floor lands on it, and a free yaw lands
/// on [`GRID_YAW_DEG`]. Not decoration: structure is built out of pieces
/// quantized to [`LENGTH_STEP_M`], so two things put down off the grid leave a
/// gap between them that no piece can bridge, and "extend to the gap" would be
/// refused for every gap in the room. Quantizing where a piece is *put down* is
/// what makes the measured distances buildable. A snap onto a socket or a
/// surface is not quantized — there the host decides, and the host is already
/// wherever it is.
pub(crate) const GRID_M: f64 = 0.5;

/// The step a free placement's spin lands on, in degrees.
pub(crate) const GRID_YAW_DEG: f64 = 15.0;

/// The mesh key the held piece is registered under while it is in the air. It
/// is not a node — it has no row yet — so it cannot be keyed by node id, and a
/// name no venue can mint keeps the two namespaces apart.
const HELD_ID: &str = "__held__";

/// The node id a ghost stands in for while it has no row. Only ever handed to
/// `NodeSockets`, which asks a node for its catalog entry and its parameters
/// and never for its identity.
pub(crate) const GHOST_NODE: &str = "__ghost__";

// ---------------------------------------------------------------------------
// What is held
// ---------------------------------------------------------------------------

/// The builder's transient state. See the module docs.
#[derive(Debug, Default)]
pub(crate) enum Hand {
    /// Nothing in the air: clicks select, drags orbit.
    #[default]
    Empty,
    /// A ghost follows the cursor and a release places it.
    Holding(Held),
    /// A socket was clicked and a run is being measured out of it.
    Extending(Extending),
}

impl Hand {
    /// Whether the pointer belongs to the builder rather than the camera.
    ///
    /// The pointer-ownership rule in one predicate: while this is true the
    /// build layer occludes the viewport, so an orbit cannot start under a
    /// ghost, and while it is false the layer is not mounted at all.
    pub(crate) fn owns_pointer(&self) -> bool {
        !matches!(self, Hand::Empty)
    }

    pub(crate) fn held(&self) -> Option<&Held> {
        match self {
            Hand::Holding(held) => Some(held),
            _ => None,
        }
    }

    pub(crate) fn extending(&self) -> Option<&Extending> {
        match self {
            Hand::Extending(run) => Some(run),
            _ => None,
        }
    }

    /// The one-line readout the stage page prints and the harness reads.
    ///
    /// Every state the builder can be in has a spelling here, which is what
    /// makes each of them reachable from a test without a GPU: the picture is
    /// evidence for a human, and this line is evidence for a machine.
    pub(crate) fn readout(&self) -> String {
        match self {
            Hand::Empty => "Hand: empty".to_string(),
            Hand::Holding(held) => format!("Hand: holding {}", held.what.label()),
            Hand::Extending(run) => format!(
                "Hand: extending {} {}",
                run.from_node_label, run.from_socket
            ),
        }
    }
}

/// A piece in the air, and where it would land.
#[derive(Debug)]
pub(crate) struct Held {
    pub(crate) what: Holding,
    /// The joint the ghost is currently stuck to, if any. Held across pointer
    /// samples: it is the whole of the hysteresis, because the search itself
    /// has no memory (see [`luma_scene::snap::DETACH_THRESHOLD`]).
    pub(crate) latched: Option<SnapMatch>,
    /// The last solved landing. `None` until the cursor has moved once.
    pub(crate) landed: Option<Landed>,
}

impl Held {
    pub(crate) fn new(what: Holding) -> Self {
        Self {
            what,
            latched: None,
            landed: None,
        }
    }
}

/// What kind of thing is in the air. The three differ only in which verb
/// commits them, which is why they are one enum and not three hands.
#[derive(Debug, Clone)]
pub(crate) enum Holding {
    /// A catalog piece that does not exist yet — `attach` or `place_free`.
    Piece {
        catalog_ref: String,
        kind: NodeKind,
        display_name: String,
        /// The socket a free drop rests on, when the palette row names one:
        /// a tower and a stick are the same generator put down on different
        /// footings (`luma_render::catalog::footings`). `None` leaves it to
        /// [`Room::seat_socket`], which finds an authored underside by its
        /// geometry rather than by a table of names.
        footing: Option<&'static str>,
        params: BTreeMap<String, f64>,
    },
    /// A subtree copied off an existing one — `attach` per node, root first.
    Duplicate {
        root: String,
        display_name: String,
        /// Inverts the copy's handedness about its root socket.
        flip: bool,
    },
    /// A patched fixture that has never been placed — `reattach`.
    Tray { node: String, label: String },
}

impl Holding {
    pub(crate) fn label(&self) -> &str {
        match self {
            Holding::Piece { display_name, .. } | Holding::Duplicate { display_name, .. } => {
                display_name
            }
            Holding::Tray { label, .. } => label,
        }
    }

    /// The catalog entry whose sockets and geometry the ghost is drawn from.
    /// A tray fixture has none — it hangs by its clamp and its housing is the
    /// QLC+ definition's business.
    pub(crate) fn catalog_ref(&self) -> Option<&str> {
        match self {
            Holding::Piece { catalog_ref, .. } => Some(catalog_ref),
            Holding::Duplicate { .. } | Holding::Tray { .. } => None,
        }
    }

    pub(crate) fn kind(&self) -> NodeKind {
        match self {
            Holding::Piece { kind, .. } => *kind,
            Holding::Duplicate { .. } => NodeKind::Piece,
            Holding::Tray { .. } => NodeKind::Fixture,
        }
    }

    /// The socket a free drop seats on, when this thing has one.
    ///
    /// A fixture does not: its clamp is its only socket and it is the one it
    /// hangs by, so a fixture put down on the floor rests on that same clamp.
    pub(crate) fn footing(&self) -> Option<&str> {
        match self {
            Holding::Piece { footing, .. } => *footing,
            Holding::Duplicate { .. } | Holding::Tray { .. } => None,
        }
    }
}

/// A run being measured out of an open socket.
///
/// The ray is cast once, when the socket is clicked — not per frame. Its
/// answer is a property of the room, and re-casting it while a length is being
/// typed would let the number the operator is editing move under them.
#[derive(Debug)]
pub(crate) struct Extending {
    pub(crate) from_node: String,
    pub(crate) from_node_label: String,
    pub(crate) from_socket: String,
    /// What the ray met, when it met anything.
    pub(crate) reach: Option<Reach>,
    /// The length asked for, in metres, already quantized to
    /// [`LENGTH_STEP_M`].
    pub(crate) length_m: f64,
}

impl Extending {
    /// Whether this length is refused. Longer than a measured gap is the
    /// design's second and last hard error: it is what stops structure
    /// growing through structure.
    pub(crate) fn refused(&self) -> Option<String> {
        let reach = self.reach.as_ref()?;
        (self.length_m > reach.gap_m + f64::EPSILON).then(|| {
            format!(
                "{:.2} m is longer than the {:.2} m gap",
                self.length_m, reach.gap_m
            )
        })
    }

    /// Whether this length bridges the gap exactly, and therefore owes a
    /// far-end constraint on the socket it reaches.
    pub(crate) fn bridges(&self) -> Option<&Reach> {
        let reach = self.reach.as_ref()?;
        ((self.length_m - reach.gap_m).abs() <= f64::EPSILON.max(1e-6)).then_some(reach)
    }

    /// The measurement readout: metres, with feet as the small print. Feet are
    /// display-only — nothing in the model is ever imperial.
    pub(crate) fn measurement(&self) -> String {
        let metres = self.reach.as_ref().map_or(self.length_m, |r| r.gap_m);
        format!("Gap: {metres:.2} m")
    }
}

/// What an extend ray met.
#[derive(Debug, Clone)]
pub(crate) struct Reach {
    pub(crate) node: String,
    pub(crate) socket: String,
    /// Centre-to-centre distance between the two socket points, quantized down
    /// to a buildable span.
    pub(crate) gap_m: f64,
}

// ---------------------------------------------------------------------------
// Where it would land
// ---------------------------------------------------------------------------

/// A solved placement: the pose to draw, the verb to call, and the reason it
/// would be refused.
#[derive(Debug, Clone)]
pub(crate) struct Landed {
    /// The ghost's world frame, in the socket layer's Y-up space.
    pub(crate) world: DMat4,
    pub(crate) how: Landing,
    /// Set when the placement is one the resolver would not accept. The ghost
    /// draws red and the release commits nothing.
    pub(crate) refused: Option<String>,
}

impl Landed {
    /// The one-line readout, which is how a headless test sees a snap.
    pub(crate) fn readout(&self) -> String {
        match &self.how {
            Landing::Socket {
                parent,
                my_socket,
                their_socket,
                ..
            } => format!("Landing: attach {my_socket} to {parent} {their_socket}"),
            Landing::Free {
                surface,
                my_socket,
                seat,
            } => {
                let host = surface.as_ref().map_or_else(
                    || FLOOR_SOCKET.to_string(),
                    |(node, s)| format!("{node} {s}"),
                );
                format!(
                    "Landing: place {my_socket} on {host} at u {:.2} v {:.2} yaw {:.0} trim {:.2}",
                    seat.u,
                    seat.v,
                    seat.yaw.to_degrees(),
                    seat.trim
                )
            }
        }
    }
}

/// Which verb a landing is.
#[derive(Debug, Clone)]
pub(crate) enum Landing {
    /// Socket met socket: `attach` for a new node, `reattach` for one that
    /// already has a row.
    Socket {
        parent: String,
        my_socket: String,
        their_socket: String,
        yaw: f64,
    },
    /// Seated on a surface at `(u, v, yaw, trim)` — `place_free`. `surface`
    /// is `None` for the venue's own floor, which is what the handler defaults
    /// to.
    Free {
        surface: Option<(String, String)>,
        my_socket: String,
        seat: SurfacePlacement,
    },
}

// ---------------------------------------------------------------------------
// The room the ladder searches
// ---------------------------------------------------------------------------

/// Every placed node's frame and sockets, in the shape the solver walks.
///
/// Built once per graph change rather than per pointer sample: it is a
/// projection of the resolved venue, and re-deriving it on every mouse move
/// would re-resolve every socket of every node sixty times a second for an
/// answer that only changes when the graph does.
pub(crate) struct Room {
    root: String,
    poses: HashMap<String, DMat4>,
    sockets: HashMap<String, Vec<ResolvedSocket>>,
    pieces: Vec<ScenePiece>,
}

impl Room {
    /// Project a solved graph into the search's own shape.
    ///
    /// The root's two synthesized surfaces (`floor` and `rig`) are added by
    /// hand because they are not catalog sockets — they are the venue frame,
    /// and [`luma_scene::venue::root_socket`] is the one place they are
    /// spelled.
    pub(crate) fn new(
        graph: &VenueGraph,
        sockets: &VenueSockets,
        poses: HashMap<String, DMat4>,
    ) -> Self {
        let root = graph.root().to_string();
        let mut by_node: HashMap<String, Vec<ResolvedSocket>> = HashMap::new();
        let mut pieces = Vec::new();
        for node in graph.nodes() {
            let Some(world) = poses.get(&node.id).copied() else {
                continue;
            };
            let mut resolved = if node.id == root {
                Vec::new()
            } else {
                sockets.sockets(node)
            };
            if node.id == root {
                resolved.extend(
                    [FLOOR_SOCKET, luma_scene::venue::RIG_SOCKET]
                        .into_iter()
                        .filter_map(root_socket),
                );
            }
            pieces.push(ScenePiece {
                id: node.id.clone(),
                mesh_path: node.id.clone(),
                world_matrix: world,
            });
            by_node.insert(node.id.clone(), resolved);
        }
        Self {
            root,
            poses,
            sockets: by_node,
            pieces,
        }
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) fn pose(&self, node: &str) -> Option<DMat4> {
        self.poses.get(node).copied()
    }

    pub(crate) fn sockets_of(&self, node: &str) -> &[ResolvedSocket] {
        self.sockets.get(node).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn socket(&self, node: &str, name: &str) -> Option<&ResolvedSocket> {
        self.sockets_of(node).iter().find(|s| s.name == name)
    }

    /// One socket's world position.
    pub(crate) fn socket_world(&self, node: &str, name: &str) -> Option<DVec3> {
        let pose = self.pose(node)?;
        let socket = self.socket(node, name)?;
        Some(pose.transform_point3(socket.position))
    }

    /// Every socket in the room, as `(node, socket)`. The builder's markers
    /// and the duplicate's landing pads are both filtered out of this one
    /// walk, so "what can I click" has a single answer.
    pub(crate) fn open_sockets(&self) -> impl Iterator<Item = (&str, &ResolvedSocket)> {
        self.sockets
            .iter()
            .flat_map(|(node, list)| list.iter().map(move |s| (node.as_str(), s)))
            .filter(|(_, s)| s.socket_type != SocketType::Grab)
    }

    /// Solve the ladder for a held piece.
    ///
    /// `latched` is last frame's joint; supplying it is what makes the snap
    /// hysteretic. `surface` is the hit under the cursor when the caller has
    /// one — the raycast lives outside the solver so the solver stays pure
    /// math, and outside this too so that a builder with no rendered frame
    /// still snaps to sockets and to the floor.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn land(
        &self,
        held_sockets: &[ResolvedSocket],
        kind: NodeKind,
        footing: Option<&str>,
        cursor: DVec3,
        latched: Option<&SnapMatch>,
        surface: Option<&SnapSurface>,
        exclude: Option<&str>,
    ) -> Option<(Landed, Option<SnapMatch>)> {
        // The latch first: a joint that has taken hold keeps the ghost until
        // the cursor leaves the wider radius, whatever the search would say.
        if let Some(latch) = latched {
            if let Some(landed) = self.hold(latch, held_sockets, kind, cursor) {
                return Some((landed, Some(latch.clone())));
            }
        }

        let mut lookup = HashMap::new();
        for (node, list) in &self.sockets {
            lookup.insert(node.clone(), list.clone());
        }
        lookup.insert(HELD_ID.to_string(), held_sockets.to_vec());

        let result = solve_snap(&SnapInput {
            held_mesh_path: HELD_ID,
            cursor_world: cursor,
            current_quaternion: None,
            pieces: &self.pieces,
            exclude_id: exclude,
            shift_held: false,
            surface,
            lookup_sockets: &lookup,
        });

        match (&result.matched, &result.parent_id) {
            // A discrete joint, close enough to take hold.
            (Some(matched), Some(parent)) if result.score <= ATTACH_THRESHOLD => {
                let world = self.mate(
                    parent,
                    &matched.host_socket,
                    held_sockets,
                    &matched.held_socket,
                    kind,
                )?;
                Some((
                    Landed {
                        world,
                        how: Landing::Socket {
                            parent: parent.clone(),
                            my_socket: matched.held_socket.clone(),
                            their_socket: matched.host_socket.clone(),
                            yaw: 0.0,
                        },
                        refused: None,
                    },
                    Some(matched.clone()),
                ))
            }
            // A surface, or the ground plane: a free seat either way.
            _ => {
                let footing = footing.or_else(|| {
                    result
                        .matched
                        .as_ref()
                        .map(|m| m.held_socket.as_str())
                        .or_else(|| Self::seat_socket(held_sockets).map(|s| s.name.as_str()))
                })?;
                let held = held_sockets.iter().find(|s| s.name == footing)?;
                let (host_node, host_socket) = match (&result.matched, &result.parent_id) {
                    (Some(m), Some(parent)) => (parent.clone(), m.host_socket.clone()),
                    _ => (self.root.clone(), FLOOR_SOCKET.to_string()),
                };
                let host = self.socket(&host_node, &host_socket)?.clone();
                let parent_world = self.pose(&host_node)?;
                // Seat the piece where the cursor is, in the host surface's own
                // plane. `invert_placement` is the inverse of the mate the
                // resolver will perform, so what is written down is exactly
                // what will be read back.
                let flush = place_on(parent_world, &host, held, kind, SurfacePlacement::FLUSH);
                // The flush mate seats the held socket on the host socket; the
                // cursor is where it should be instead, so the whole pose slides
                // by the difference and `invert_placement` reads that slide back
                // as `(u, v, yaw, trim)` in the surface's own plane.
                let slide = cursor - flush.transform_point3(held.position);
                let seat = invert_placement(
                    DMat4::from_translation(slide) * flush,
                    parent_world,
                    &host,
                    held,
                    kind,
                );
                // Trim is the operator's, not the cursor's: a drop is on the
                // surface, and flying it is an edit afterwards.
                let mut seat = SurfacePlacement { trim: 0.0, ..seat };
                // The grid rung. Only the venue's own floor is quantized — a
                // deck's top is a surface, and a surface is wherever the host
                // put it.
                if host_node == self.root {
                    let step = GRID_YAW_DEG.to_radians();
                    seat = SurfacePlacement {
                        u: (seat.u / GRID_M).round() * GRID_M,
                        v: (seat.v / GRID_M).round() * GRID_M,
                        yaw: (seat.yaw / step).round() * step,
                        trim: 0.0,
                    };
                }
                let world = place_on(parent_world, &host, held, kind, seat);
                let surface = (host_node != self.root || host_socket != FLOOR_SOCKET)
                    .then(|| (host_node, host_socket));
                Some((
                    Landed {
                        world,
                        how: Landing::Free {
                            surface,
                            my_socket: held.name.clone(),
                            seat,
                        },
                        refused: None,
                    },
                    None,
                ))
            }
        }
    }

    /// Keep a joint that has already taken hold, while the cursor is still
    /// inside the wider radius.
    fn hold(
        &self,
        latch: &SnapMatch,
        held_sockets: &[ResolvedSocket],
        kind: NodeKind,
        cursor: DVec3,
    ) -> Option<Landed> {
        let parent = latch.host_id.as_deref()?;
        let at = self.socket_world(parent, &latch.host_socket)?;
        if at.distance(cursor) > DETACH_THRESHOLD {
            return None;
        }
        let world = self.mate(
            parent,
            &latch.host_socket,
            held_sockets,
            &latch.held_socket,
            kind,
        )?;
        Some(Landed {
            world,
            how: Landing::Socket {
                parent: parent.to_string(),
                my_socket: latch.held_socket.clone(),
                their_socket: latch.host_socket.clone(),
                yaw: 0.0,
            },
            refused: None,
        })
    }

    /// The world pose of one named pair, mated flush — the same arithmetic the
    /// resolver will perform when the edge is written.
    fn mate(
        &self,
        parent: &str,
        their_socket: &str,
        held_sockets: &[ResolvedSocket],
        my_socket: &str,
        kind: NodeKind,
    ) -> Option<DMat4> {
        let host = self.socket(parent, their_socket)?;
        let held = held_sockets.iter().find(|s| s.name == my_socket)?;
        Some(place_on(
            self.pose(parent)?,
            host,
            held,
            kind,
            SurfacePlacement::FLUSH,
        ))
    }

    /// The socket a piece is *put down* on when no palette entry named one:
    /// the holdable socket facing furthest from local up.
    ///
    /// A rule rather than a table. Every piece that can rest on something has
    /// an underside, and an underside is the socket whose outward normal
    /// points down in the piece's own frame — which is exactly what the
    /// catalog authors with `normal(DVec3::NEG_Y)` on every `bottom`, `mount`,
    /// `base` and `seat` it declares.
    pub(crate) fn seat_socket(held: &[ResolvedSocket]) -> Option<&ResolvedSocket> {
        held.iter()
            .filter(|s| s.socket_type.polarity().can_be_held())
            .min_by(|a, b| {
                a.normal
                    .dot(luma_scene::snap::WORLD_UP)
                    .total_cmp(&b.normal.dot(luma_scene::snap::WORLD_UP))
            })
    }

    /// Cast along a socket's outward normal and report the first open,
    /// compatible socket it reaches.
    ///
    /// A *socket* search, not a mesh raycast: what the extend gesture wants to
    /// know is where the run could end, and the answer is a joint, not a
    /// triangle. Candidates must lie ahead of the origin along the normal and
    /// within [`RAY_HALF_WIDTH_M`] of the line, which is the section's own
    /// half-width — a truss that would miss by more than its own width is not
    /// on the way to anywhere.
    pub(crate) fn cast(&self, from_node: &str, from_socket: &str) -> Option<Reach> {
        let origin = self.socket_world(from_node, from_socket)?;
        let socket = self.socket(from_node, from_socket)?;
        let pose = self.pose(from_node)?;
        let direction = pose.transform_vector3(socket.normal).normalize_or_zero();
        let mut best: Option<Reach> = None;
        for (node, candidate) in self.open_sockets() {
            if node == from_node || !candidate.socket_type.mates(socket.socket_type) {
                continue;
            }
            let Some(at) = self.socket_world(node, &candidate.name) else {
                continue;
            };
            let along = (at - origin).dot(direction);
            if along <= LENGTH_STEP_M {
                continue;
            }
            if (at - origin - direction * along).length() > RAY_HALF_WIDTH_M {
                continue;
            }
            let gap_m = quantize_down(along);
            if gap_m < LENGTH_STEP_M {
                continue;
            }
            if best.as_ref().is_none_or(|b| gap_m < b.gap_m) {
                best = Some(Reach {
                    node: node.to_string(),
                    socket: candidate.name.clone(),
                    gap_m,
                });
            }
        }
        best
    }
}

/// How far off the ray's line a socket may sit and still count as on the way:
/// the truss section's own half-width.
const RAY_HALF_WIDTH_M: f64 = 0.15;

/// The largest buildable length no greater than `metres`.
pub(crate) fn quantize_down(metres: f64) -> f64 {
    (metres / LENGTH_STEP_M).floor() * LENGTH_STEP_M
}

/// The nearest buildable length.
pub(crate) fn quantize(metres: f64) -> f64 {
    (metres / LENGTH_STEP_M).round().max(1.0) * LENGTH_STEP_M
}

/// Metres as feet and inches, for the small print under a measurement. Display
/// only — nothing in the model is imperial.
pub(crate) fn feet_and_inches(metres: f64) -> String {
    let total_inches = metres * 39.370_078_74;
    let feet = (total_inches / 12.0).floor();
    let inches = total_inches - feet * 12.0;
    format!("{feet:.0} ft {inches:.0} in")
}

/// The world point a screen ray meets the floor at, in the socket layer's
/// Y-up space.
///
/// The builder's fallback target: with no rendered frame there is no mesh to
/// hit, and the floor is a plane whose intersection is arithmetic. Returns
/// `None` for a ray that runs away from the plane, which is a camera looking
/// at the sky.
pub(crate) fn floor_point(ray: &luma_scene::Ray) -> Option<DVec3> {
    let origin = coords::three_from_world(ray.origin).as_dvec3();
    let dir = coords::three_from_world(ray.dir).as_dvec3();
    let denominator = dir.dot(luma_scene::snap::WORLD_UP);
    if denominator.abs() < 1e-6 {
        return None;
    }
    let t = -origin.dot(luma_scene::snap::WORLD_UP) / denominator;
    (t > 0.0).then(|| origin + dir * t)
}

/// The palette's footing for one catalog entry: the socket a free drop of it
/// rests on.
///
/// Two entries share one generator — a stick lies on its `seat`, a tower
/// stands on its `base` — so the footing is what the *palette row* means, not
/// what the piece is.
pub(crate) fn footing_for(catalog_ref: &str, tower: bool) -> Option<&'static str> {
    if tower && catalog_ref == TRUSS_STRAIGHT {
        Some(BASE_SOCKET)
    } else if luma_scene::catalog::piece(catalog_ref).is_some_and(|p| p.geometry.is_procedural()) {
        Some(SEAT_SOCKET)
    } else {
        // A mesh piece's underside is authored, and its name varies by family
        // (`bottom`, `mount`, `base`). `Room::seat_socket` finds it by
        // geometry rather than by a table of names.
        None
    }
}

/// The catalog id of the straight generator — the piece a tower is made of.
pub(crate) const TRUSS_STRAIGHT: &str = "truss/straight";

/// Whether a socket can host something — used to decide which beads light up
/// while a piece is held.
pub(crate) fn can_host(socket: &ResolvedSocket) -> bool {
    socket.socket_type.polarity().can_host() && socket.socket_type != SocketType::Grab
}

/// Whether a socket is something a row of fixtures can be spread along.
///
/// A truss face, a deck top or edge, the venue's own floor and grid — anything
/// with a length and a normal. A truss end is a host, but it is a bolt circle:
/// it takes one piece, at one place, and "eight of them along it" means
/// nothing.
pub(crate) fn is_feature(socket: &ResolvedSocket) -> bool {
    can_host(socket)
        && matches!(
            socket.socket_type.kind(),
            SocketKind::Surface | SocketKind::Edge
        )
}

/// Whether any of a held piece's sockets could mate this host.
pub(crate) fn compatible(host: &ResolvedSocket, held: &[ResolvedSocket]) -> bool {
    held.iter().any(|s| {
        s.socket_type.mates(host.socket_type) && s.socket_type.polarity() != Polarity::Female
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_length_longer_than_the_gap_is_refused_and_an_equal_one_bridges() {
        let run = Extending {
            from_node: "a".into(),
            from_node_label: "A".into(),
            from_socket: "end_b".into(),
            reach: Some(Reach {
                node: "b".into(),
                socket: "end_a".into(),
                gap_m: 3.0,
            }),
            length_m: 3.5,
        };
        assert!(run.refused().is_some(), "3.5 m into a 3.0 m gap");
        let exact = Extending {
            length_m: 3.0,
            ..run
        };
        assert!(exact.refused().is_none());
        assert_eq!(exact.bridges().map(|r| r.node.clone()), Some("b".into()));
        let stub = Extending {
            length_m: 1.5,
            ..exact
        };
        assert!(stub.refused().is_none(), "a stub is not a refusal");
        assert!(stub.bridges().is_none(), "a stub owes no far end");
    }

    #[test]
    fn lengths_are_quantized_to_the_half_metre() {
        assert!((quantize(3.2) - 3.0).abs() < 1e-9);
        assert!((quantize(3.3) - 3.5).abs() < 1e-9);
        assert!((quantize_down(3.4) - 3.0).abs() < 1e-9);
        // Never zero: a run of no length is not a run.
        assert!((quantize(0.1) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_seat_is_the_socket_facing_furthest_from_up() {
        let up = ResolvedSocket::from_frame(
            "top",
            SocketType::BottomMount,
            DVec3::ZERO,
            DVec3::Y,
            DVec3::X,
        );
        let down = ResolvedSocket::from_frame(
            "bottom",
            SocketType::BottomMount,
            DVec3::ZERO,
            DVec3::NEG_Y,
            DVec3::X,
        );
        let sockets = vec![up, down];
        assert_eq!(
            Room::seat_socket(&sockets).map(|s| s.name.as_str()),
            Some("bottom")
        );
    }
}
