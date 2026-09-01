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
//! Snapping is *hysteretic*: [`luma_scene::snap::attach_radius`] is how close
//! the cursor must come for a joint to take hold, and
//! [`luma_scene::snap::detach_radius`] is how far it must go for that joint to
//! let go. Both are **pixels at the projected host socket**, because which
//! joint a gesture means is what the operator is *pointing at*: adjacent deck
//! corners are 0.7 m apart, so a metre-space radius latched whichever of them
//! iteration order reached first and then a wider metre-space radius welded it
//! there. A room with no camera to project through (the first frame, before a
//! pane is laid out) falls back to the metre pair, which is also what the port
//! goldens pin.
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
    attach_radius, detach_radius, solve_snap, Aim, ScenePiece, SnapInput, SnapMatch,
};
use luma_scene::sockets::{Polarity, ResolvedSocket, SocketKind, SocketType};
use luma_scene::venue::{
    invert_placement, place_on, root_socket, NodeKind, NodeSockets, SurfacePlacement, VenueGraph,
    FLOOR_SOCKET,
};

/// How big a socket mark's box is, in pixels: the anchor a right-press finds
/// the joint's menu by, and the box a script clicks the middle of. **Not an
/// acceptance radius** — a press inside one means nothing more than a press on
/// the room at that pixel, and which joint that pixel is is the two radii
/// above.
pub(crate) const SOCKET_MARK_PICK_PX: f32 = 11.0;

/// The build vocabulary the page shares with the verbs, re-exported so this
/// module reads as one alphabet.
///
/// None of it is declared here: an extend's step, its stub and the stick it
/// runs are all `stage_ops`', because the page and the Python facade call the
/// same verb, and a second copy of the step would let a length the builder
/// offered be refused by the command it offered it to.
pub(crate) use luma_lib::models::venue_graph::Reach;
pub(crate) use luma_lib::services::stage_ops::{
    quantize, LENGTH_STEP_M, STUB_LENGTH_M, TRUSS_STRAIGHT,
};

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

/// How parallel a socket's normal must be to a hit face's before the hit
/// counts as being *on* that socket's face.
const FACE_ALIGNED: f64 = 0.8;

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

/// What the operator is doing. The builder's one state machine.
///
/// # Why every mode is in here
///
/// Three of these used to be flags beside it — `palette_open`, `tray_open` and
/// an `Option<Distribute>` on the [`crate::stage::Build`] — and the page grew
/// one panel per flag, because a boolean cannot say what to *hide*. A state
/// can: the chrome is a `match` on this, so a mode with no controls draws none
/// by construction rather than by every call site remembering to.
///
/// The ordering is the flow: you choose a thing, you place it, and placing it
/// on a face is what opens the row you configure.
#[derive(Debug, Default)]
pub(crate) enum Hand {
    /// Nothing in the air: clicks select, drags orbit.
    #[default]
    Idle,
    /// The add-element dialog is up. The one modal surface the builder has.
    Choosing(Choosing),
    /// A ghost follows the cursor and a click places it — and leaves the hand
    /// exactly here, so the next click places another.
    Placing(Box<Held>),
    /// A socket was clicked and a run is being measured out of it.
    Extending(Box<Extending>),
    /// A row of fixtures is being fitted to a face, previewed live.
    Configuring(Box<Configuring>),
}

impl Hand {
    /// Whether the pointer belongs to the builder rather than the camera.
    ///
    /// The pointer-ownership rule in one predicate: while this is true the
    /// build layer occludes the viewport, so an orbit cannot start under a
    /// ghost, and while it is false the layer is not mounted at all.
    ///
    /// Choosing and Configuring are *not* in it: both put a card on screen and
    /// the room behind it stays live, which is the whole reason the configure
    /// surface is a popover and not a modal.
    pub(crate) fn owns_pointer(&self) -> bool {
        matches!(self, Hand::Placing(_) | Hand::Extending(_))
    }

    /// Whether the pointer's *position* is the thing being edited.
    ///
    /// Owning the pointer and being driven by it are two facts, and only
    /// [`Hand::Placing`] is both. A run owns the pointer — an orbit starting
    /// mid-extend would swing the room out from under the measurement — but
    /// its length comes from the box on its own line, not from where the
    /// cursor is. Mounting move and click handlers for it anyway made the
    /// surface repaint the whole layer on every mouse move, which is what ate
    /// the drags aimed at the controls sitting on top of it.
    pub(crate) fn aims_with_pointer(&self) -> bool {
        matches!(self, Hand::Placing(_))
    }

    pub(crate) fn held(&self) -> Option<&Held> {
        match self {
            Hand::Placing(held) => Some(held),
            _ => None,
        }
    }

    pub(crate) fn extending(&self) -> Option<&Extending> {
        match self {
            Hand::Extending(run) => Some(run),
            _ => None,
        }
    }

    pub(crate) fn choosing(&self) -> Option<&Choosing> {
        match self {
            Hand::Choosing(choosing) => Some(choosing),
            _ => None,
        }
    }

    pub(crate) fn configuring(&self) -> Option<&Configuring> {
        match self {
            Hand::Configuring(row) => Some(row),
            _ => None,
        }
    }

    /// What `Escape` means here.
    ///
    /// One rung down the flow rather than straight to [`Hand::Idle`], because
    /// the states nest: a configure popover was opened from a piece still in
    /// the hand, so dismissing it should hand that piece back rather than make
    /// the operator pick it again. The dialog and a placing hand both drop out
    /// to idle, which is where a selection is cleared.
    pub(crate) fn escape(self) -> Self {
        match self {
            Hand::Configuring(row) => Hand::Placing(Held::new(row.what)),
            _ => Hand::Idle,
        }
    }
}

/// What the pointer's ray met in the picture: a piece's face, named by the
/// piece and given in the socket layer's world space. The viewport raycast
/// produces it; [`Room::land`] resolves it to the *named* face socket under
/// it, so a landing found by pointing is committed by the same verb as one
/// found by a bead.
#[derive(Debug, Clone)]
pub(crate) struct SurfaceHit {
    pub(crate) piece: String,
    pub(crate) point: DVec3,
    pub(crate) normal: DVec3,
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
    /// Where the last aim came from, kept so an edit to the held piece's own
    /// parameters (a span scrub) can re-solve the landing without waiting for
    /// the pointer to move again.
    pub(crate) cursor: Option<Aimed>,
}

/// One aim: the pointer, and what it met in the room.
///
/// Both halves, because the two answer different questions — the pixel chooses
/// *which* joint the gesture means, the world point is where a free seat
/// actually goes — and a re-solve after a parameter edit has to reproduce the
/// same aim exactly.
#[derive(Debug, Clone)]
pub(crate) struct Aimed {
    /// The pointer, in window pixels.
    pub(crate) at: gpui::Point<gpui::Pixels>,
    /// Where its ray met the room, in the socket layer's frame.
    pub(crate) world: DVec3,
    /// The mesh face under it, when a rendered frame had one to give.
    pub(crate) hit: Option<SurfaceHit>,
}

/// The add-element dialog's own state.
///
/// The query lives in the [`luma_ui::text_input::TextInput`] the page owns —
/// an editor is an entity and this is not — so all that is here is where the
/// keyboard is. Rows are derived from the catalog and the venue every frame
/// rather than cached: they are a pure function of a query over data already
/// in memory, and a cache of one would be a second answer to the same
/// question.
#[derive(Debug, Default)]
pub(crate) struct Choosing {
    /// Which row the keyboard is on, as an index into what the query left.
    pub(crate) cursor: usize,
}

/// A row of fixtures being fitted to one face, previewed live.
///
/// It holds the *request* — a fixture, a count, a layout — and the poses the
/// band math last answered with. Nothing here is committed: the preview is
/// [`luma_scene::distribute::offsets`] run on every edit, and the verb runs
/// once, on Apply. That is what makes the fit failure a thing you *see* rather
/// than a thing you find out.
#[derive(Debug)]
pub(crate) struct Configuring {
    /// What is being seated. Held so `Escape` can hand it back — see
    /// [`Hand::escape`].
    pub(crate) what: Holding,
    pub(crate) host_node: String,
    pub(crate) host_label: String,
    pub(crate) host_socket: String,
    /// Where on the face the popover hangs, in window space.
    pub(crate) at: gpui::Point<gpui::Pixels>,
    pub(crate) count: usize,
    pub(crate) layout: luma_scene::distribute::Layout,
    /// The last preview the band math answered with: one pose per body, or the
    /// reason there is none.
    pub(crate) preview: Result<Vec<glam::DMat4>, luma_scene::distribute::Fit>,
}

impl Held {
    pub(crate) fn new(what: Holding) -> Box<Self> {
        Box::new(Self::of(what))
    }

    fn of(what: Holding) -> Self {
        Self {
            what,
            latched: None,
            landed: None,
            cursor: None,
        }
    }

    /// The same thing, picked up again with nothing landed.
    ///
    /// What keeps place mode sticky: a commit rebuilds the hand from the piece
    /// it just put down, so the parameters the operator tuned for the last one
    /// — a truss span, a hinge angle — are the next one's starting point. The
    /// landing is dropped because the ghost has to be re-solved against
    /// wherever the cursor is now, and keeping it would flash the new piece at
    /// the old one's pose for one frame.
    pub(crate) fn again(what: &Holding) -> Box<Self> {
        Self::new(what.clone())
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
    Unplaced { node: String, label: String },
    /// A fixture from the library that has no row anywhere yet — `distribute`,
    /// which is how *every* new fixture is created, a single one being a row
    /// of one. It carries what the fit needs so the preview and the commit
    /// measure the same body.
    Fixture {
        path: String,
        label: String,
        /// The mode it will be patched in. `None` until the definition lands.
        mode: Option<String>,
        /// The body's width in metres, from the QLC+ definition — the same
        /// number `luma_lib`'s `body_width_m` gives the commit, so a row that
        /// previews as fitting cannot be refused for length on apply.
        width_m: f64,
        /// The body's `[width, height, depth]` in metres, from the same
        /// definition — the collision box a held light carries so it cannot
        /// be pushed through structure. `None` until the definition lands,
        /// which is also "no box yet": a refusal invented from a guessed size
        /// would block placements the real body allows.
        dims: Option<[f64; 3]>,
    },
}

impl Holding {
    pub(crate) fn label(&self) -> &str {
        match self {
            Holding::Piece { display_name, .. } | Holding::Duplicate { display_name, .. } => {
                display_name
            }
            Holding::Unplaced { label, .. } | Holding::Fixture { label, .. } => label,
        }
    }

    pub(crate) fn kind(&self) -> NodeKind {
        match self {
            Holding::Piece { kind, .. } => *kind,
            Holding::Duplicate { .. } => NodeKind::Piece,
            Holding::Unplaced { .. } | Holding::Fixture { .. } => NodeKind::Fixture,
        }
    }

    /// The socket a free drop seats on, when this thing has one.
    ///
    /// A fixture does not: its clamp is its only socket and it is the one it
    /// hangs by, so a fixture put down on the floor rests on that same clamp.
    pub(crate) fn footing(&self) -> Option<&str> {
        match self {
            Holding::Piece { footing, .. } => *footing,
            Holding::Duplicate { .. } | Holding::Unplaced { .. } | Holding::Fixture { .. } => None,
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

    /// The measurement readout: the gap the ray found, with feet as the small
    /// print. Feet are display-only — nothing in the model is ever imperial.
    ///
    /// `None` when the ray met nothing, because there is no gap to report and a
    /// readout echoing the length back as a "gap" would invent a measurement
    /// out of the number being typed.
    pub(crate) fn measurement(&self) -> Option<String> {
        let reach = self.reach.as_ref()?;
        Some(format!("Gap: {:.2} m", reach.gap_m))
    }
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
    /// Each placed piece's local bounds, for the collision test. Fixtures and
    /// the root have none — a light clamped to a truss is not what stops a
    /// truss going through a wall.
    boxes: HashMap<String, luma_scene::aabb::DAabb>,
}

/// Contact is not collision: both boxes shrink by this before the overlap
/// test, so a piece mated flush on a face stays legal and a piece *through*
/// another does not.
const COLLISION_CLEARANCE_M: f64 = 0.02;

impl Room {
    /// The nearest placed piece under a ray, by its collision box.
    ///
    /// The headless twin of the viewport's mesh raycast: the harness has no
    /// rendered frame to pick against, and the boxes the collision test
    /// already keeps are the same truth at lower resolution. Ray in the socket
    /// layer's world space.
    pub(crate) fn pick(&self, origin: glam::DVec3, dir: glam::DVec3) -> Option<String> {
        let mut best: Option<(f64, &String)> = None;
        for (node, bounds) in &self.boxes {
            let Some(pose) = self.poses.get(node) else {
                continue;
            };
            let Some(t) = luma_scene::aabb::ray_obb(origin, dir, *bounds, pose) else {
                continue;
            };
            if best.as_ref().is_none_or(|(nearest, _)| t < *nearest) {
                best = Some((t, node));
            }
        }
        best.map(|(_, node)| node.clone())
    }

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
        let mut boxes = HashMap::new();
        for node in graph.nodes() {
            let Some(world) = poses.get(&node.id).copied() else {
                continue;
            };
            if let Some(bounds) = piece_bounds(sockets, node) {
                boxes.insert(node.id.clone(), bounds);
            }
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
            boxes,
        }
    }

    /// One placed piece's local bounds, when it has any.
    pub(crate) fn bounds_of(&self, node: &str) -> Option<luma_scene::aabb::DAabb> {
        self.boxes.get(node).copied()
    }

    /// The first placed piece a body at `world` would pass through, skipping
    /// the pieces named — the host a landing mates (contact by construction)
    /// and the piece being moved.
    fn collision(
        &self,
        body: luma_scene::aabb::DAabb,
        world: DMat4,
        skip: &[Option<&str>],
    ) -> Option<&str> {
        self.boxes.iter().find_map(|(id, other)| {
            if skip.iter().any(|name| *name == Some(id.as_str())) {
                return None;
            }
            let pose = self.poses.get(id)?;
            luma_scene::aabb::obb_intersects(body, &world, *other, pose, COLLISION_CLEARANCE_M)
                .then_some(id.as_str())
        })
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

    /// The named face socket under a viewport hit: the closest hosting
    /// surface socket on the hit piece whose plane the hit lies in.
    ///
    /// A *named* socket, deliberately: the solver can seat on a virtual
    /// surface, but a virtual surface cannot be written down — the graph's
    /// edges name sockets — so resolving the hit to the authored socket here
    /// is what lets a landing found by pointing be committed by the same verb
    /// as one found by a bead.
    fn face_socket_for(&self, hit: &SurfaceHit, kind: NodeKind) -> Option<(String, String)> {
        let pose = self.pose(&hit.piece)?;
        let inverse = pose.inverse();
        let local_point = inverse.transform_point3(hit.point);
        let local_normal = inverse.transform_vector3(hit.normal).normalize_or_zero();
        self.sockets_of(&hit.piece)
            .iter()
            .filter(|socket| {
                socket.socket_type.kind() == SocketKind::Surface
                    && socket.socket_type.polarity().can_host()
                    && (kind == NodeKind::Fixture || socket.socket_type != SocketType::TrussFace)
            })
            // The face the hit is *on*, not merely the nearest: a hit on a
            // side face must not seat the piece on the top.
            .filter(|socket| socket.normal.normalize_or_zero().dot(local_normal) > FACE_ALIGNED)
            .min_by(|a, b| {
                a.position
                    .distance(local_point)
                    .total_cmp(&b.position.distance(local_point))
            })
            .map(|socket| (hit.piece.clone(), socket.name.clone()))
    }

    /// Solve the ladder for a held piece.
    ///
    /// `latched` is last frame's joint; supplying it is what makes the snap
    /// hysteretic. `surface` is the mesh hit under the cursor when the caller
    /// has one — the raycast lives outside the solver so the solver stays pure
    /// math, and outside this too so that a builder with no rendered frame
    /// still snaps to sockets and to the floor.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn land(
        &self,
        held_sockets: &[ResolvedSocket],
        kind: NodeKind,
        footing: Option<&str>,
        cursor: DVec3,
        aim: Option<&Aim<'_>>,
        latched: Option<&SnapMatch>,
        surface: Option<&SurfaceHit>,
        exclude: Option<&str>,
        body: Option<luma_scene::aabb::DAabb>,
    ) -> Option<(Landed, Option<SnapMatch>)> {
        // The latch first: a joint that has taken hold keeps the ghost until
        // the cursor leaves the wider radius, whatever the search would say.
        if let Some(latch) = latched {
            if let Some(landed) = self.hold(latch, held_sockets, kind, cursor, aim, exclude, body) {
                return Some((landed, Some(latch.clone())));
            }
        }

        let mut lookup = HashMap::new();
        for (node, list) in &self.sockets {
            let list = if kind == NodeKind::Fixture {
                list.clone()
            } else {
                list.iter()
                    .filter(|socket| socket.socket_type != SocketType::TrussFace)
                    .cloned()
                    .collect()
            };
            lookup.insert(node.clone(), list);
        }
        lookup.insert(HELD_ID.to_string(), held_sockets.to_vec());

        let result = solve_snap(&SnapInput {
            held_mesh_path: HELD_ID,
            cursor_world: cursor,
            current_quaternion: None,
            pieces: &self.pieces,
            exclude_id: exclude,
            shift_held: false,
            surface: None,
            aim,
            lookup_sockets: &lookup,
        });

        match (&result.matched, &result.parent_id) {
            // A discrete joint, close enough to take hold.
            (Some(matched), Some(parent)) if result.score <= attach_radius(aim) => {
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
                        refused: self.refusal(body, world, &[Some(parent.as_str()), exclude]),
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
                // The face under the pointer, then the floor. The discrete
                // pass answers only within its attach radius, so the ray is
                // what lets a piece land anywhere on a face rather than only
                // near its named centre.
                let (host_node, host_socket) = match (&result.matched, &result.parent_id) {
                    (Some(m), Some(parent)) => (parent.clone(), m.host_socket.clone()),
                    _ => surface
                        .filter(|hit| exclude != Some(hit.piece.as_str()))
                        .and_then(|hit| self.face_socket_for(hit, kind))
                        .unwrap_or_else(|| (self.root.clone(), FLOOR_SOCKET.to_string())),
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
                let refused = self.refusal(body, world, &[Some(host_node.as_str()), exclude]);
                let free_floor = host_node == self.root && host_socket == FLOOR_SOCKET;
                let surface = (!free_floor).then_some((host_node, host_socket));
                Some((
                    Landed {
                        world,
                        how: Landing::Free {
                            surface,
                            my_socket: held.name.clone(),
                            seat,
                        },
                        refused,
                    },
                    None,
                ))
            }
        }
    }

    /// Keep a joint that has already taken hold, while the cursor is still
    /// inside the wider radius.
    #[allow(clippy::too_many_arguments)]
    fn hold(
        &self,
        latch: &SnapMatch,
        held_sockets: &[ResolvedSocket],
        kind: NodeKind,
        cursor: DVec3,
        aim: Option<&Aim<'_>>,
        exclude: Option<&str>,
        body: Option<luma_scene::aabb::DAabb>,
    ) -> Option<Landed> {
        let parent = latch.host_id.as_deref()?;
        let at = self.socket_world(parent, &latch.host_socket)?;
        // In the same space the search chose it: a joint let go of in metres
        // while it was chosen in pixels is a joint that never lets go — 0.8 m
        // of hysteresis over corners 0.7 m apart welded the wrong one on.
        let left = match aim {
            Some(aim) => aim
                .reach(at)
                .is_none_or(|(px, _)| px > detach_radius(Some(aim))),
            None => at.distance(cursor) > detach_radius(None),
        };
        if left {
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
            refused: self.refusal(body, world, &[Some(parent), exclude]),
        })
    }

    /// The collision arm of every landing: `Some(reason)` when the body at
    /// `world` passes through a placed piece that is neither its host nor
    /// itself.
    fn refusal(
        &self,
        body: Option<luma_scene::aabb::DAabb>,
        world: DMat4,
        skip: &[Option<&str>],
    ) -> Option<String> {
        let body = body?;
        self.collision(body, world, skip)
            .map(|_| "would pass through structure".to_string())
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
}

/// One node's local bounds: the catalog's measurement for a mesh piece, the
/// generator's arithmetic for a procedural one at this node's own parameters.
/// A node with neither (a fixture, the root) has no box and no collisions.
fn piece_bounds(
    sockets: &VenueSockets,
    node: &luma_scene::venue::Node,
) -> Option<luma_scene::aabb::DAabb> {
    let reference = node.catalog_ref.as_deref()?;
    match luma_scene::catalog::piece(reference)?.geometry {
        luma_scene::catalog::Geometry::Mesh { .. } | luma_scene::catalog::Geometry::Assembly(_) => {
            sockets.catalog().bounds(reference)
        }
        luma_scene::catalog::Geometry::Procedural(family) => {
            Some(luma_render::catalog::procedural_bounds(
                luma_render::catalog::node_params(family, &node.params),
            ))
        }
    }
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
pub(crate) fn compatible(host: &ResolvedSocket, held: &[ResolvedSocket], kind: NodeKind) -> bool {
    // A truss face is where a rig hangs — clamps only. Structure joins
    // structure at ends, corners and decks; a stick T-boned into another
    // stick's side is a joint no coupler makes.
    if kind != NodeKind::Fixture && host.socket_type == SocketType::TrussFace {
        return false;
    }
    held.iter().any(|s| {
        s.socket_type.mates(host.socket_type) && s.socket_type.polarity() != Polarity::Female
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_length_longer_than_the_gap_is_refused_and_an_equal_one_is_not() {
        let run = Extending {
            from_node: "a".into(),
            from_socket: "end_b".into(),
            reach: Some(Reach {
                node_id: "b".into(),
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
        assert!(exact.refused().is_none(), "exactly the gap bridges it");
        let stub = Extending {
            length_m: 1.5,
            ..exact
        };
        assert!(stub.refused().is_none(), "a stub is not a refusal");
        // What a bridge *owes* — the far-end check on the socket it reached —
        // is `stage_ops::Stage::extend`'s, not this card's: the page shows a
        // length and the verb decides what that length means.
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
