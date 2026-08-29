//! The venue graph and the one resolver that turns it into poses.
//!
//! `docs/design/venue-graph.md`. A venue is a **tree of relations**: every node
//! stores `(parent, my_socket, their_socket, roll, params)` and no world pose.
//! Poses exist only as the output of [`resolve`], which is called on load and
//! after every edit. Moving a truss moves what is bolted to it because there is
//! nothing else it could do.
//!
//! # Frame
//!
//! Everything here is the **socket layer's** frame — asset space, Y-up, `f64`
//! (see the crate docs). [`crate::coords`] is the one bridge out of it, and
//! [`NodePose::data_pose`] is the one place this module crosses.
//!
//! # What a node's pose *means*
//!
//! For a piece, [`NodePose::world`] is its mesh transform. For a **fixture** it
//! is the mount frame: beam = mount normal, so the pose a fixture carries is
//! the one its host socket gives it, turned into the same convention a stored
//! fixture row carried. That is why a fixture needs no rest-axis of its own —
//! see the design doc's "Beam = mount normal".
//!
//! # Determinism
//!
//! Depth-first, children sorted by node id. Two solves of the same graph
//! produce byte-identical poses; the golden capture depends on it.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DMat4, DVec3};

use crate::coords;
use crate::sockets::{ResolvedSocket, RollFreedom, SocketMode, SocketType};

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// The closed alphabet of node kinds. A new set object is a [`NodeKind::Piece`]
/// with sockets, never a new kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    /// The root: the venue floor plus the audience direction. Exactly one per
    /// venue, and the only node with no parent.
    Venue,
    /// A deck or riser — a surface other things stand on.
    Stage,
    /// Truss segments carrying one `along(t)` over their total length.
    Run,
    /// A vertical run, height in 0.5 m steps.
    Tower,
    /// Anything else in the catalog.
    Piece,
    /// A patched fixture's placement. `catalog_ref` is the `fixtures` row id.
    Fixture,
    /// `count` copies of `catalog_ref` spread over `span`. Its children are
    /// derived at solve time, not stored.
    Array,
}

impl NodeKind {
    /// Every variant, in declaration order.
    pub const ALL: [NodeKind; 7] = [
        NodeKind::Venue,
        NodeKind::Stage,
        NodeKind::Run,
        NodeKind::Tower,
        NodeKind::Piece,
        NodeKind::Fixture,
        NodeKind::Array,
    ];

    /// The wire name, shared with the database's `kind` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Venue => "venue",
            NodeKind::Stage => "stage",
            NodeKind::Run => "run",
            NodeKind::Tower => "tower",
            NodeKind::Piece => "piece",
            NodeKind::Fixture => "fixture",
            NodeKind::Array => "array",
        }
    }

    /// The variant a `kind` column holds, or `None` for an unknown string.
    #[must_use]
    pub fn from_name(name: &str) -> Option<NodeKind> {
        NodeKind::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// The socket the resolver synthesizes on the root, standing for the venue
/// floor. The root has no catalog entry, so it has no authored sockets; this
/// one is the host every free placement on the floor mates with.
pub const FLOOR_SOCKET: &str = "floor";

/// The other socket the resolver synthesizes on the root: the **grid**, a
/// down-facing plane at the venue origin.
///
/// The design doc's placement vocabulary is `(u, v, yaw, trim)` on a surface,
/// and `trim` is "how high it flies" — but a plane facing *up* gives a flown
/// fixture an upward beam, because beam is the mount normal. A rig hung from a
/// grid is the case `trim` was written for, so the grid is a host in its own
/// right rather than a special case inside the floor. Everything else about it
/// is the floor: same origin, same `(u, v)`, same `trim`.
pub const RIG_SOCKET: &str = "rig";

/// A node's parameters, by key. The vocabulary is `u`, `v`, `trim` on a
/// placement; `span`, `angle`, `faces` on a generated piece; `count`, `span` on
/// an array.
///
/// Deliberately untyped: the database column is `(node_id, key, value)`, and a
/// struct-per-kind here would be a second declaration of the same list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params(BTreeMap<String, f64>);

impl Params {
    /// The value under `key`, or `default`.
    #[must_use]
    pub fn get(&self, key: &str, default: f64) -> f64 {
        self.0.get(key).copied().unwrap_or(default)
    }

    /// The value under `key`, if it is set at all.
    #[must_use]
    pub fn lookup(&self, key: &str) -> Option<f64> {
        self.0.get(key).copied()
    }

    /// Set one key. A non-finite value is dropped rather than stored: NaN in a
    /// transform poisons every descendant's pose, and there is no pose a caller
    /// could have meant by it.
    pub fn set(&mut self, key: impl Into<String>, value: f64) {
        if value.is_finite() {
            self.0.insert(key.into(), value);
        }
    }

    /// Every `(key, value)`, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64)> {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

impl FromIterator<(String, f64)> for Params {
    fn from_iter<T: IntoIterator<Item = (String, f64)>>(iter: T) -> Self {
        let mut params = Params::default();
        for (key, value) in iter {
            params.set(key, value);
        }
        params
    }
}

/// One row of `venue_nodes`.
#[derive(Clone, Debug)]
pub struct Node {
    /// Stable identity, and what an edge names.
    pub id: String,
    pub kind: NodeKind,
    /// What geometry this node has: a catalog piece id for structure, a
    /// `fixtures` row id for a [`NodeKind::Fixture`]. `None` on the root.
    pub catalog_ref: Option<String>,
    pub label: Option<String>,
    pub params: Params,
}

/// One row of `venue_edges` — the relation that produced a pose, kept rather
/// than discarded the moment the drag ends.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub parent: String,
    /// The socket on the child.
    pub my_socket: String,
    /// The socket on the parent it mates.
    pub their_socket: String,
    /// The mate's turn about the shared normal, radians. On a surface
    /// placement this is what the stage vocabulary calls **yaw**; the column is
    /// `roll` because a socket's freedom is a roll, and one number should not
    /// have two homes. Clamped by the host socket's [`RollFreedom`] at solve.
    pub roll: f64,
}

/// One row of `venue_constraints` — a far end. A **check**, never an edge: it
/// is evaluated after the solve and never participates in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub node: String,
    pub my_socket: String,
    pub target_node: String,
    pub target_socket: String,
}

// ---------------------------------------------------------------------------
// Geometry supply
// ---------------------------------------------------------------------------

/// Where a node's sockets come from.
///
/// Not [`crate::snap::SocketLookup`], which keys on a catalog id alone: a
/// generated piece's sockets are a function of its **parameters** (a 3 m truss
/// and a 6 m truss have different end frames), so the node is the key. The
/// implementation lives in `luma_render::catalog`, which is the only module
/// that knows both the catalog and the geometry.
pub trait NodeSockets {
    /// `node`'s sockets in its own local (Y-up) frame.
    fn sockets(&self, node: &Node) -> Vec<ResolvedSocket>;

    /// Whether this node's geometry is the geometry it names.
    ///
    /// A venue outlives a catalog: a row naming a piece that has since been
    /// dropped still has a pose, and [`Self::sockets`] is expected to give it
    /// something to hang by rather than nothing. Saying so is a *separate*
    /// question from placing it, and it is the supply's to answer — the
    /// resolver has no catalog of its own and must not grow one.
    ///
    /// Defaults to "yes", so a test table or a fixture supply does not have to
    /// have an opinion.
    fn is_known(&self, node: &Node) -> bool {
        let _ = node;
        true
    }
}

// ---------------------------------------------------------------------------
// The graph
// ---------------------------------------------------------------------------

/// Why an edge may not be inserted.
///
/// These are the invariants the design doc enumerates, enforced where they
/// live: at edge insert, not at solve. The solve has no failure mode left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeError {
    /// No node with this id.
    UnknownNode(String),
    /// The parent named by the edge is not in the graph.
    UnknownParent(String),
    /// The root is the venue frame and has no parent.
    RootHasNoParent,
    /// The parent is the child, or one of its descendants.
    Cycle { child: String, parent: String },
    /// The child's catalog entry has no socket by that name.
    MissingSocket { node: String, socket: String },
    /// The parent's catalog entry has no socket by that name.
    MissingHostSocket { node: String, socket: String },
    /// The pair exists but cannot mate: different joint kinds, or the wrong
    /// halves (see [`crate::sockets::Polarity`]).
    Polarity { held: SocketType, host: SocketType },
}

impl std::fmt::Display for EdgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgeError::UnknownNode(id) => write!(f, "no node `{id}` in this venue"),
            EdgeError::UnknownParent(id) => write!(f, "no parent `{id}` in this venue"),
            EdgeError::RootHasNoParent => f.write_str("the venue root cannot be attached"),
            EdgeError::Cycle { child, parent } => {
                write!(f, "`{parent}` is inside `{child}`, so attaching would loop")
            }
            EdgeError::MissingSocket { node, socket } => {
                write!(f, "`{node}` has no socket `{socket}`")
            }
            EdgeError::MissingHostSocket { node, socket } => {
                write!(f, "`{node}` has no socket `{socket}` to attach to")
            }
            EdgeError::Polarity { held, host } => write!(
                f,
                "a `{}` socket does not mate a `{}` socket",
                held.as_str(),
                host.as_str()
            ),
        }
    }
}

impl std::error::Error for EdgeError {}

/// One venue's nodes, edges, params and constraints.
///
/// Built from the four tables and handed to [`resolve`]. Nodes are keyed by id
/// and edges by child id, so "exactly one parent" is a map key here just as it
/// is a primary key in the schema.
#[derive(Clone, Debug)]
pub struct VenueGraph {
    root: String,
    nodes: BTreeMap<String, Node>,
    edges: BTreeMap<String, Edge>,
    constraints: Vec<Constraint>,
}

impl VenueGraph {
    /// A venue holding nothing but its root.
    #[must_use]
    pub fn new(root: Node) -> Self {
        let mut nodes = BTreeMap::new();
        let root_id = root.id.clone();
        nodes.insert(root_id.clone(), root);
        Self {
            root: root_id,
            nodes,
            edges: BTreeMap::new(),
            constraints: Vec::new(),
        }
    }

    /// The root node's id — the venue frame.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Add a node with no edge yet. A node with no edge and no descendants is
    /// unplaced (a fixture in the patch tray); [`resolve`] leaves it out of the
    /// walk rather than guessing a pose.
    pub fn insert(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// The node with this id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Every node, in id order.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// The edge that places `id`, if it has one.
    #[must_use]
    pub fn edge(&self, id: &str) -> Option<&Edge> {
        self.edges.get(id)
    }

    /// Every far-end check.
    #[must_use]
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Record a far-end check. Not an edge: it never participates in the solve.
    pub fn constrain(&mut self, constraint: Constraint) {
        self.constraints.push(constraint);
    }

    /// Place `child` by mating two sockets.
    ///
    /// This is the **only** writer of an edge, which is what makes the acyclic
    /// invariant an insertion-order property rather than a check the solver has
    /// to repeat.
    ///
    /// # Errors
    /// Every variant of [`EdgeError`]: an unknown node or parent, the root, a
    /// cycle, a socket neither catalog entry declares, or a pair whose
    /// polarity forbids the joint.
    pub fn attach<S: NodeSockets + ?Sized>(
        &mut self,
        child: &str,
        edge: Edge,
        sockets: &S,
    ) -> Result<(), EdgeError> {
        self.check_edge(child, &edge, sockets)?;
        self.edges.insert(child.to_string(), edge);
        Ok(())
    }

    /// Install an edge that was already admitted, without re-checking it.
    ///
    /// The loader's entry point. [`Self::attach`] is the writer and enforces
    /// the invariants there; re-enforcing them on read would mean a venue whose
    /// catalog entry has since been dropped fails to load at all, instead of
    /// reporting one node as [`Warning::UnknownCatalogRef`] and drawing the
    /// rest. Rows outlive catalogs.
    ///
    /// The solve is still total: a cycle among loaded rows cannot be reached
    /// from the root, so its members simply get no pose.
    pub fn insert_edge(&mut self, child: &str, edge: Edge) {
        self.edges.insert(child.to_string(), edge);
    }

    /// Whether [`Self::attach`] would succeed, without performing it.
    ///
    /// # Errors
    /// As [`Self::attach`].
    pub fn check_edge<S: NodeSockets + ?Sized>(
        &self,
        child: &str,
        edge: &Edge,
        sockets: &S,
    ) -> Result<(), EdgeError> {
        let child_node = self
            .nodes
            .get(child)
            .ok_or_else(|| EdgeError::UnknownNode(child.to_string()))?;
        if child == self.root {
            return Err(EdgeError::RootHasNoParent);
        }
        let parent_node = self
            .nodes
            .get(&edge.parent)
            .ok_or_else(|| EdgeError::UnknownParent(edge.parent.clone()))?;

        // Invariant 1: acyclic. The parent must not be the child or inside it.
        // Walking *up* from the parent is the cheap direction: the chain is
        // bounded by the node count, and a graph that already satisfies the
        // invariant has no loop to fall into.
        let mut cursor = Some(edge.parent.as_str());
        let mut budget = self.nodes.len();
        while let Some(id) = cursor {
            if id == child {
                return Err(EdgeError::Cycle {
                    child: child.to_string(),
                    parent: edge.parent.clone(),
                });
            }
            let Some(next) = budget.checked_sub(1) else {
                break;
            };
            budget = next;
            cursor = self.edges.get(id).map(|e| e.parent.as_str());
        }

        // Invariant 3: a resolvable, polarity-compatible socket pair.
        let held = find_socket(&sockets.sockets(child_node), &edge.my_socket).ok_or_else(|| {
            EdgeError::MissingSocket {
                node: child.to_string(),
                socket: edge.my_socket.clone(),
            }
        })?;
        let host = self
            .host_socket(parent_node, &edge.their_socket, sockets)
            .ok_or_else(|| EdgeError::MissingHostSocket {
                node: edge.parent.clone(),
                socket: edge.their_socket.clone(),
            })?;
        if !held.socket_type.mates(host.socket_type) {
            return Err(EdgeError::Polarity {
                held: held.socket_type,
                host: host.socket_type,
            });
        }
        Ok(())
    }

    /// Remove a node's edge, leaving it unplaced.
    pub fn detach(&mut self, child: &str) {
        self.edges.remove(child);
    }

    /// `id` and everything hanging off it, in id order. Includes `id` itself.
    #[must_use]
    pub fn subtree(&self, id: &str) -> Vec<String> {
        let mut out = BTreeSet::new();
        out.insert(id.to_string());
        // Fixed-point over a bounded set: a graph this deep is a few hundred
        // nodes, and the alternative is a child index that has to be kept true.
        loop {
            let grown: Vec<String> = self
                .edges
                .iter()
                .filter(|(child, edge)| out.contains(&edge.parent) && !out.contains(*child))
                .map(|(child, _)| child.clone())
                .collect();
            if grown.is_empty() {
                return out.into_iter().collect();
            }
            out.extend(grown);
        }
    }

    /// A parent's socket by name, with the root's synthesized floor.
    fn host_socket<S: NodeSockets + ?Sized>(
        &self,
        parent: &Node,
        name: &str,
        sockets: &S,
    ) -> Option<ResolvedSocket> {
        if parent.kind == NodeKind::Venue {
            if let Some(socket) = root_socket(name) {
                return Some(socket);
            }
        }
        find_socket(&sockets.sockets(parent), name)
    }
}

/// The venue floor: an up-facing plane at the root's origin.
///
/// Synthesized rather than authored because the root has no catalog entry, and
/// because a free placement wants exactly one host on the floor rather than one
/// per deck the room happens to contain.
///
/// Its frame fixes what `(u, v)` mean on the floor: `+Z` is up, `+X` (the
/// tangent) is **stage right**, and the bitangent `Z x X` is therefore
/// **toward the house** — data space `-Y`. The apparent sign flip is the
/// data-to-three mirror, not a choice; it is why `invert_placement` exists
/// rather than a sign convention anyone has to remember.
#[cfg(test)]
fn floor_socket() -> ResolvedSocket {
    root_socket(FLOOR_SOCKET).expect("the floor is a root socket")
}

/// One of the root's two synthesized hosts, by name.
#[must_use]
pub fn root_socket(name: &str) -> Option<ResolvedSocket> {
    let normal = match name {
        FLOOR_SOCKET => crate::snap::WORLD_UP,
        RIG_SOCKET => -crate::snap::WORLD_UP,
        _ => return None,
    };
    Some(ResolvedSocket {
        name: name.to_string(),
        socket_type: SocketType::Ground,
        position: DVec3::ZERO,
        normal,
        tangent: DVec3::X,
        mode: SocketMode::Face,
        outward: normal,
        roll: SocketType::Ground.roll(),
    })
}

fn find_socket(sockets: &[ResolvedSocket], name: &str) -> Option<ResolvedSocket> {
    sockets.iter().find(|s| s.name == name).cloned()
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// How close a far end has to come before it counts as met, in metres. A
/// truss bolt hole is millimetres; anything looser is a gap someone can see.
pub const CONSTRAINT_TOLERANCE_M: f64 = 1e-3;

/// One node's resolved pose.
#[derive(Clone, Debug)]
pub struct NodePose {
    /// The node's id, or `"<array id>#<index>"` for a derived array member.
    pub node: String,
    pub kind: NodeKind,
    pub catalog_ref: Option<String>,
    pub label: Option<String>,
    /// The node this hangs off, or `None` for the root.
    pub parent: Option<String>,
    /// World transform in the socket layer's frame (Y-up, `f64`). A piece's
    /// mesh transform; a fixture's mount frame — see the module docs.
    pub world: DMat4,
    /// Which member of an array this is, for a derived node.
    pub array_index: Option<u32>,
    /// The parameters the pose was built from, so a caller that needs the
    /// generator's span or a placement's trim does not go back to the graph.
    pub params: Params,
}

impl NodePose {
    /// The stored convention: position in metres and the Euler triple, both in
    /// data space (Z-up). This is what the database used to hold, what
    /// `scene_desc` carries, and what `Mount::from_stored` reads.
    #[must_use]
    pub fn data_pose(&self) -> ([f64; 3], [f64; 3]) {
        coords::data_pose_of_d(self.world)
    }

    /// The same pose as a data-space basis, with no trip through Euler angles —
    /// what `fixture_kinematics::Mount::from_frame` takes.
    #[must_use]
    pub fn data_basis(&self) -> (DVec3, glam::DMat3) {
        coords::data_basis_from_three(self.world)
    }

    /// Whether this pose stands for one physical object the set-piece layer
    /// draws — structure and props, one entry per thing in the room.
    ///
    /// False for the three poses that are frames rather than objects:
    /// - the **root**, which is the venue frame and has no geometry;
    /// - an **array anchor** (`array_index == None` on a [`NodeKind::Array`]),
    ///   which is the seat its members are spread over — it carries their
    ///   `catalog_ref`, so a consumer that does not skip it draws N+1 copies
    ///   with a second one on top of the middle member;
    /// - a node whose `catalog_ref` is absent, which has no geometry to stand
    ///   for.
    ///
    /// A [`NodeKind::Fixture`] is excluded too: it is a light, drawn from the
    /// patch with its own definition, not from the set-piece list.
    ///
    /// The predicate lives here because every consumer needs the same answer —
    /// the renderer's piece list, the agent binding's `venue.pieces`, and the
    /// React store's mesh list — and the three of them derived it separately
    /// until one of them forgot the anchor.
    #[must_use]
    pub fn is_set_piece(&self) -> bool {
        self.catalog_ref.is_some()
            && !matches!(self.kind, NodeKind::Venue | NodeKind::Fixture)
            && !(self.kind == NodeKind::Array && self.array_index.is_none())
    }
}

/// Whether a far-end check is met.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConstraintStatus {
    /// The two sockets meet, within [`CONSTRAINT_TOLERANCE_M`].
    Satisfied,
    /// Both ends resolved and they do not meet, by this many metres.
    Violated { gap_m: f64 },
    /// One end does not resolve: the node is unplaced, gone, or has no such
    /// socket.
    Dangling,
}

/// One far-end check, evaluated.
#[derive(Clone, Debug)]
pub struct ConstraintReport {
    pub node: String,
    pub my_socket: String,
    pub target_node: String,
    pub target_socket: String,
    pub status: ConstraintStatus,
}

/// An open structural socket — a truss end nothing is bolted to.
///
/// Only self-mating joints ([`crate::sockets::Polarity::Neutral`]) count: a
/// deck's four corners are not "dangling" for having nothing standing on them,
/// but the open end of a run is what a builder needs told about.
///
/// "Open" means no relation names it: neither half of a joint is dangling, and
/// neither is an end a [`Constraint`] checks — a far end is exactly the case
/// where the builder has already said what the socket is for.
#[derive(Clone, Debug)]
pub struct DanglingSocket {
    pub node: String,
    pub socket: String,
    pub socket_type: SocketType,
}

/// Something the solve had to decide for the caller.
#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// The node's `catalog_ref` resolves to nothing, so it has no geometry and
    /// no sockets. Left at its parent's mate point rather than dropped: a piece
    /// in the wrong place is debuggable, a piece that vanished is not.
    UnknownCatalogRef(String),
    /// The edge names a socket the geometry does not have. Same treatment.
    MissingSocket(String),
    /// The requested roll is not a freedom this joint has, so it was quantized.
    RollClamped { requested: f64, applied: f64 },
    /// An array with a `count` outside `1..=`[`MAX_ARRAY_COUNT`].
    ArrayCountClamped { requested: f64, applied: u32 },
}

/// Ceiling on a single array's expansion. ~500 nodes is the working bound for
/// a whole venue (design doc, Performance), so one array asking for more than
/// this is a typo, not a rig.
pub const MAX_ARRAY_COUNT: u32 = 512;

/// A warning attributed to the node that provoked it.
#[derive(Clone, Debug)]
pub struct NodeWarning {
    pub node: String,
    pub warning: Warning,
}

/// The root of a subtree the solve never reached: a node with no edge, and
/// everything hanging off it.
///
/// Two things produce one — a fixture nobody has dragged out of the patch tray,
/// and `detach`. Both are legitimate; what is not legitimate is silence. A
/// detached run used to vanish from the solve with no pose, no warning and no
/// mention, so a builder could delete a wing by dragging it and see only that
/// it was gone.
///
/// Only the **root** is listed: the subtree below it is unplaced for exactly
/// one reason, and repeating that reason per descendant would bury it.
/// [`Self::descendants`] is how big the branch is, so a caller can say "and 6
/// more" without walking the graph itself.
#[derive(Clone, Debug)]
pub struct UnplacedNode {
    pub node: String,
    pub kind: NodeKind,
    pub label: Option<String>,
    /// How many nodes hang off it, not counting itself.
    pub descendants: usize,
}

/// The whole venue, solved.
#[derive(Clone, Debug, Default)]
pub struct ResolvedVenue {
    poses: Vec<NodePose>,
    index: BTreeMap<String, usize>,
    constraints: Vec<ConstraintReport>,
    dangling: Vec<DanglingSocket>,
    unplaced: Vec<UnplacedNode>,
    warnings: Vec<NodeWarning>,
}

impl ResolvedVenue {
    /// Every placed node, depth-first with children in id order. An array
    /// node is followed by its derived members, which name it as their parent.
    pub fn poses(&self) -> impl Iterator<Item = &NodePose> {
        self.poses.iter()
    }

    /// One node's pose, by id.
    #[must_use]
    pub fn pose(&self, node: &str) -> Option<&NodePose> {
        self.index.get(node).map(|i| &self.poses[*i])
    }

    /// Every far-end check, in insertion order.
    #[must_use]
    pub fn constraints(&self) -> &[ConstraintReport] {
        &self.constraints
    }

    /// Every open structural socket, in solve order.
    #[must_use]
    pub fn dangling(&self) -> &[DanglingSocket] {
        &self.dangling
    }

    /// Every subtree the walk never reached, by its root, in node-id order.
    #[must_use]
    pub fn unplaced(&self) -> &[UnplacedNode] {
        &self.unplaced
    }

    /// Everything the solve decided for the caller.
    #[must_use]
    pub fn warnings(&self) -> &[NodeWarning] {
        &self.warnings
    }

    /// The report a mutating call returns: what happened to one node, and
    /// everything about the venue that the change might have broken.
    ///
    /// Reports **suggest fixes, they do not refuse** — the two hard errors are
    /// [`EdgeError`]s, raised before a write ever happens.
    #[must_use]
    pub fn placement(&self, node: &str) -> Placement {
        let pose = self.pose(node);
        Placement {
            node: node.to_string(),
            ok: pose.is_some(),
            parent: pose.and_then(|p| p.parent.clone()),
            warnings: self
                .warnings
                .iter()
                .filter(|w| w.node == node)
                .map(|w| w.warning.clone())
                .collect(),
            // An array's open ends are reported per member, so the report for
            // the node the caller named has to gather its members' too.
            dangling: self
                .dangling
                .iter()
                .filter(|d| d.node == node || member_of(&d.node) == Some(node))
                .cloned()
                .collect(),
            constraints: self
                .constraints
                .iter()
                .filter(|c| c.node == node || c.target_node == node)
                .cloned()
                .collect(),
        }
    }
}

/// The array a derived id belongs to: `"wall#3"` is `wall`'s.
///
/// The `#` spelling is [`NodePose::node`]'s contract, written in exactly one
/// other place ([`place`]); a member id is otherwise indistinguishable from any
/// other node id.
fn member_of(id: &str) -> Option<&str> {
    id.split_once('#').map(|(anchor, _)| anchor)
}

/// What one mutating call did, and what it left unresolved.
///
/// The design doc also lists `collisions` (OBB against neighbours) and
/// `span_exceeds`. Neither is here: both need a piece's *bounds*, which the
/// socket supply does not carry, and shipping them as fields that are always
/// empty would be a promise the type does not keep. They arrive with the
/// builder that can draw them.
#[derive(Clone, Debug)]
pub struct Placement {
    pub node: String,
    /// Whether the node came out of the solve with a pose at all.
    pub ok: bool,
    pub parent: Option<String>,
    pub warnings: Vec<Warning>,
    pub dangling: Vec<DanglingSocket>,
    pub constraints: Vec<ConstraintReport>,
}

/// Solve one venue: every node's pose, arrays expanded, checks evaluated.
///
/// Depth-first from the root with children in id order, so the output is
/// byte-identical across runs. Unplaced nodes — a patched fixture nobody has
/// dragged out of the tray — are absent rather than placed at the origin.
#[must_use]
pub fn resolve<S: NodeSockets + ?Sized>(graph: &VenueGraph, sockets: &S) -> ResolvedVenue {
    // Children by parent, each list already in id order because `edges` is a
    // `BTreeMap`. Built once: the alternative is a scan per node, which is the
    // difference between linear and quadratic on a 500-node rig.
    let mut children: BTreeMap<&str, Vec<(&str, &Edge)>> = BTreeMap::new();
    for (child, edge) in &graph.edges {
        children
            .entry(edge.parent.as_str())
            .or_default()
            .push((child.as_str(), edge));
    }

    let mut out = ResolvedVenue::default();
    let Some(root) = graph.node(&graph.root) else {
        return out;
    };
    // Which sockets are spoken for, computed once over the whole graph: a
    // socket is open or it is not, and that is a property of the relations,
    // not of the order the walk reaches them in.
    let claimed = claimed_sockets(graph);

    push_pose(
        &mut out,
        NodePose {
            node: root.id.clone(),
            kind: root.kind,
            catalog_ref: root.catalog_ref.clone(),
            label: root.label.clone(),
            parent: None,
            world: DMat4::IDENTITY,
            array_index: None,
            params: root.params.clone(),
        },
    );
    dangling_of(&mut out, root, 0, sockets, &claimed);

    // Explicit stack rather than recursion: a corrupted graph must not blow the
    // real one, and the depth bound is the node count.
    //
    // Each entry carries what placing it needs — its edge, its parent's node
    // and its parent's world frame — rather than looking any of them up again.
    // A child is queued only by the step that placed its parent, so "the parent
    // is placed and its frame is known" is a property of the queue rather than
    // three fallible lookups the body has to cope with.
    let mut queue = Vec::new();
    push_children(&mut queue, &children, root, DMat4::IDENTITY);

    while let Some(step) = queue.pop() {
        let (id, edge, parent_node, parent_world) =
            (step.id, step.edge, step.parent, step.parent_world);
        // The one lookup that can genuinely fail: `VenueGraphRows::to_graph`
        // drops a row whose `kind` is outside the alphabet but keeps the edge
        // that named it, so an edge can point at a child with no node.
        let Some(node) = graph.node(id) else {
            continue;
        };

        let host = graph.host_socket(parent_node, &edge.their_socket, sockets);
        let mine = sockets.sockets(node);
        let held = find_socket(&mine, &edge.my_socket);

        let (world, members) = match (host, held) {
            (Some(host), Some(held)) => place(&mut out, node, edge, parent_world, &host, &held),
            (host, _) => {
                // The relation names a socket the geometry does not have: the
                // catalog dropped an entry, or a generator's parameters moved a
                // face. Land the node on its parent's origin and say so — a
                // piece in the wrong place is debuggable, one that vanished is
                // not.
                let missing = if host.is_none() {
                    edge.their_socket.clone()
                } else {
                    edge.my_socket.clone()
                };
                out.warnings.push(NodeWarning {
                    node: id.to_string(),
                    warning: match &node.catalog_ref {
                        Some(catalog_ref) if sockets.sockets(node).is_empty() => {
                            Warning::UnknownCatalogRef(catalog_ref.clone())
                        }
                        _ => Warning::MissingSocket(missing),
                    },
                });
                push_pose(
                    &mut out,
                    NodePose {
                        node: id.to_string(),
                        kind: node.kind,
                        catalog_ref: node.catalog_ref.clone(),
                        label: node.label.clone(),
                        parent: Some(edge.parent.clone()),
                        world: parent_world,
                        array_index: None,
                        params: node.params.clone(),
                    },
                );
                (parent_world, 0)
            }
        };

        dangling_of(&mut out, node, members, sockets, &claimed);
        if !sockets.is_known(node) {
            out.warnings.push(NodeWarning {
                node: node.id.clone(),
                warning: Warning::UnknownCatalogRef(node.catalog_ref.clone().unwrap_or_default()),
            });
        }

        // Depth-first: this node's children go on next, in id order.
        push_children(&mut queue, &children, node, world);
    }

    out.constraints = graph
        .constraints
        .iter()
        .map(|c| evaluate_constraint(c, &out, graph, sockets))
        .collect();
    out.unplaced = unplaced_subtrees(graph, &out);
    out
}

/// One queued step of the walk: a child, the relation that places it, and the
/// parent frame that relation is measured against.
struct Walk<'a> {
    id: &'a str,
    edge: &'a Edge,
    parent: &'a Node,
    parent_world: DMat4,
}

/// Queue `parent`'s children, in reverse id order so the stack pops them
/// forwards. The parent is placed by the time this is called, which is what
/// lets the frame travel with the child instead of being looked up.
fn push_children<'a>(
    queue: &mut Vec<Walk<'a>>,
    children: &BTreeMap<&'a str, Vec<(&'a str, &'a Edge)>>,
    parent: &'a Node,
    parent_world: DMat4,
) {
    let Some(kids) = children.get(parent.id.as_str()) else {
        return;
    };
    queue.extend(kids.iter().rev().map(|(id, edge)| Walk {
        id,
        edge,
        parent,
        parent_world,
    }));
}

/// Every subtree the walk never reached, by its root.
///
/// A node with no pose is unplaced. Its *root* is the one whose parent is not
/// itself unplaced — a node with no edge at all, or one whose edge names a
/// parent that is gone. Loaded rows can also hold a cycle ([`insert_edge`] does
/// not re-check what [`attach`] admitted), and a cycle has no such root; those
/// nodes are reported individually rather than dropped, which is the whole
/// point of this list.
///
/// [`insert_edge`]: VenueGraph::insert_edge
/// [`attach`]: VenueGraph::attach
fn unplaced_subtrees(graph: &VenueGraph, out: &ResolvedVenue) -> Vec<UnplacedNode> {
    let missing: BTreeSet<&str> = graph
        .nodes()
        .map(|n| n.id.as_str())
        .filter(|id| !out.index.contains_key(*id))
        .collect();
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    let mut roots: Vec<UnplacedNode> = Vec::new();
    for id in &missing {
        let rooted = match graph.edge(id) {
            None => true,
            Some(edge) => !missing.contains(edge.parent.as_str()),
        };
        if rooted {
            report_unplaced(graph, id, &mut covered, &mut roots);
        }
    }
    // A cycle: every member's parent is unplaced too, so none of them looked
    // like a root above. Each is its own entry — there is no branch point to
    // name, and silence is what this list exists to prevent.
    for id in &missing {
        if !covered.contains(id) {
            report_unplaced(graph, id, &mut covered, &mut roots);
        }
    }
    roots.sort_by(|a, b| a.node.cmp(&b.node));
    roots
}

/// Record one unplaced subtree and mark every node it covers.
fn report_unplaced<'a>(
    graph: &'a VenueGraph,
    id: &str,
    covered: &mut BTreeSet<&'a str>,
    roots: &mut Vec<UnplacedNode>,
) {
    let mut descendants = 0;
    for member in graph.subtree(id) {
        // An edge can name a child with no node row, and a phantom is not a
        // piece anyone can go and find.
        let Some(node) = graph.node(&member) else {
            continue;
        };
        covered.insert(node.id.as_str());
        if node.id != id {
            descendants += 1;
        }
    }
    if let Some(node) = graph.node(id) {
        roots.push(UnplacedNode {
            node: node.id.clone(),
            kind: node.kind,
            label: node.label.clone(),
            descendants,
        });
    }
}

fn push_pose(out: &mut ResolvedVenue, pose: NodePose) {
    out.index.insert(pose.node.clone(), out.poses.len());
    out.poses.push(pose);
}

/// One node placed: clamp the roll its joint admits, then mate through
/// [`place_on`]. An array node is placed at its anchor and then produces its
/// members, which follow it in the walk.
///
/// Returns the node's own world frame — what its children mate against — and
/// how many members it derived (0 for anything but an array).
fn place(
    out: &mut ResolvedVenue,
    node: &Node,
    edge: &Edge,
    parent_world: DMat4,
    host: &ResolvedSocket,
    held: &ResolvedSocket,
) -> (DMat4, u32) {
    let applied_roll = clamp_roll(edge.roll, host.roll);
    if (applied_roll - edge.roll).abs() > f64::EPSILON {
        out.warnings.push(NodeWarning {
            node: node.id.clone(),
            warning: Warning::RollClamped {
                requested: edge.roll,
                applied: applied_roll,
            },
        });
    }

    let placement = SurfacePlacement {
        u: node.params.get("u", 0.0),
        v: node.params.get("v", 0.0),
        yaw: applied_roll,
        trim: node.params.get("trim", 0.0),
    };

    // Every node gets its mate point, arrays included. For an array that point
    // is its **anchor**: the seat its `span` is centred on, which is where a
    // single member would sit. Without it a successful `array(...)` reported
    // `ok: false, parent: null` — the caller's node had no pose at all — and a
    // child naming the array as its parent silently vanished for want of a
    // frame.
    let world = place_on(parent_world, host, held, node.kind, placement);
    push_pose(
        out,
        NodePose {
            node: node.id.clone(),
            kind: node.kind,
            catalog_ref: node.catalog_ref.clone(),
            label: node.label.clone(),
            parent: Some(edge.parent.clone()),
            world,
            array_index: None,
            params: node.params.clone(),
        },
    );

    if node.kind != NodeKind::Array {
        return (world, 0);
    }
    let count = array_count(out, node);
    let span = node.params.get("span", 0.0);
    for i in 0..count {
        let offset = if count <= 1 {
            0.0
        } else {
            -span / 2.0 + span * f64::from(i) / f64::from(count - 1)
        };
        let member = place_on(
            parent_world,
            host,
            held,
            node.kind,
            SurfacePlacement {
                u: placement.u + offset,
                ..placement
            },
        );
        push_pose(
            out,
            NodePose {
                node: format!("{}#{i}", node.id),
                kind: node.kind,
                catalog_ref: node.catalog_ref.clone(),
                label: node.label.clone(),
                // The array node, not the array's own parent: a member is
                // derived from the generator, and the generator now has a
                // pose to be derived from.
                parent: Some(node.id.clone()),
                world: member,
                array_index: Some(i),
                params: node.params.clone(),
            },
        );
    }
    (world, count)
}

/// The world pose a [`SurfacePlacement`] produces against one host socket.
///
/// The forward half of the pair whose inverse is [`invert_placement`], and the
/// only place the mate is written:
///
/// `parent_world . host_frame . T(u, v) . lift(trim) . Rz(yaw) . mate_suffix`
///
/// `(u, v)` run in the host surface's own plane and `trim` runs along world up
/// (see `trim_axis`), so the offset and the twist sit *between* the two
/// frames — turning about the joint rather than about the piece's own origin.
/// [`crate::snap`] calls this with [`SurfacePlacement::FLUSH`] for the bare
/// mate it scores candidates over, so there is one copy of the arithmetic and
/// the golden snap vectors pin it.
#[must_use]
pub fn place_on(
    parent_world: DMat4,
    host: &ResolvedSocket,
    held: &ResolvedSocket,
    kind: NodeKind,
    placement: SurfacePlacement,
) -> DMat4 {
    let host_world = parent_world * socket_frame(host);
    let lift = trim_axis(host_world) * placement.trim;
    host_world
        * DMat4::from_translation(DVec3::new(placement.u, placement.v, 0.0) + lift)
        * DMat4::from_rotation_z(placement.yaw)
        * mate_suffix(kind, held)
}

/// Half a turn about the host socket's tangent, or nothing.
///
/// [`SocketMode::Face`] opposes the two normals (face-to-face contact);
/// [`SocketMode::Edge`] keeps the host's orientation and only translates —
/// two decks butted along an edge both stay upright.
fn flip_for(mode: SocketMode) -> DMat4 {
    match mode {
        SocketMode::Edge => DMat4::IDENTITY,
        SocketMode::Face => DMat4::from_rotation_x(std::f64::consts::PI),
    }
}

/// A socket's local frame: origin at the socket, `+Z` its normal, `+X` its
/// tangent re-orthogonalized against the normal.
fn socket_frame(socket: &ResolvedSocket) -> DMat4 {
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

/// World up, expressed in a host socket's own frame — the axis `trim` runs
/// along.
///
/// `trim` is **how high it flies**, so it is world up wherever the surface
/// faces: a light on the grid rises the same 6 m a light on the deck does, and
/// nobody has to remember which way a host's normal points. `(u, v)` stay in
/// the surface's own plane, which is what makes them continuous over it.
///
/// A perfectly vertical surface has no "higher"; there `trim` is inert, which
/// is the same thing as saying a bolted joint has no lift.
fn trim_axis(host_world: DMat4) -> DVec3 {
    let local = glam::DMat3::from_mat4(host_world).transpose() * crate::snap::WORLD_UP;
    if local.z.abs() < VERTICAL_HOST_EPSILON {
        DVec3::ZERO
    } else {
        local
    }
}

/// Below this |cosine| between a host surface and world up, the surface is
/// vertical and `trim` has nothing to lift along.
const VERTICAL_HOST_EPSILON: f64 = 1e-9;

/// What turns the seat — the mate point, twisted and offset — into the node's
/// own pose. The fixed half of the mate, and the one place the two conventions
/// a node can have are written down.
///
/// A **piece** lands mesh-first: its held socket goes onto the host socket, the
/// two normals opposing (or not, in edge mode).
///
/// A **fixture** lands beam-first — *beam = mount normal*. It has no mesh pose
/// of its own: its rest direction is the outward normal of the socket it hangs
/// from, and its stored convention (`Mount`, `scene_desc::Fixture`) is a frame
/// whose `-Y` here — `-Z` once in data space — is that direction. `Rx(-90°)` is
/// the turn that takes the host socket frame's `+Z` onto it, and it is the
/// whole of what makes a floor light point up and an under-truss light point
/// down with no per-fixture-type rest axis.
///
/// The two differ by exactly a half turn, which is the sign error the design
/// doc's audit found three copies of.
fn mate_suffix(kind: NodeKind, held: &ResolvedSocket) -> DMat4 {
    if kind == NodeKind::Fixture {
        DMat4::from_rotation_x(-std::f64::consts::FRAC_PI_2)
    } else {
        flip_for(held.mode) * socket_frame(held).inverse()
    }
}

/// The roll a joint actually admits.
///
/// Returning the clamped value rather than an error is the point: a request for
/// a freedom a bolted joint does not have is not a reason to refuse the
/// placement, it is a number the joint answers differently.
fn clamp_roll(requested: f64, freedom: RollFreedom) -> f64 {
    match freedom {
        RollFreedom::Fixed => 0.0,
        RollFreedom::Free => requested,
        RollFreedom::Steps(degrees) if degrees > 0.0 => {
            let step = degrees.to_radians();
            (requested / step).round() * step
        }
        RollFreedom::Steps(_) => 0.0,
    }
}

fn array_count(out: &mut ResolvedVenue, node: &Node) -> u32 {
    let requested = node.params.get("count", 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let applied = requested.round().clamp(1.0, f64::from(MAX_ARRAY_COUNT)) as u32;
    if (f64::from(applied) - requested).abs() > 0.5 {
        out.warnings.push(NodeWarning {
            node: node.id.clone(),
            warning: Warning::ArrayCountClamped { requested, applied },
        });
    }
    applied
}

/// Every socket some relation already accounts for, as `(node, socket)`.
///
/// A joint claims **both** its halves — the child's `my_socket` and the host's
/// `their_socket` — and a far end claims both of its, because a constraint is
/// what the builder writes down instead of a second parent. Everything else a
/// piece offers is open.
fn claimed_sockets(graph: &VenueGraph) -> BTreeSet<(&str, &str)> {
    let mut claimed = BTreeSet::new();
    for (child, edge) in &graph.edges {
        claimed.insert((child.as_str(), edge.my_socket.as_str()));
        claimed.insert((edge.parent.as_str(), edge.their_socket.as_str()));
    }
    for constraint in &graph.constraints {
        claimed.insert((constraint.node.as_str(), constraint.my_socket.as_str()));
        claimed.insert((
            constraint.target_node.as_str(),
            constraint.target_socket.as_str(),
        ));
    }
    claimed
}

/// One node's open sockets — or, for an array, **every member's**.
///
/// An array of three trusses has three sets of ends standing in the room, and
/// the anchor is a seat with no geometry: reporting the anchor once under-counts
/// by `members - 1` and names a node the builder cannot walk up to. Each member
/// inherits the anchor's claims — the edge that seats the array seats every copy
/// through the same socket, and a far end written against the array means the
/// same socket on each — so the claim set is keyed by the anchor and the report
/// by the member.
fn dangling_of<S: NodeSockets + ?Sized>(
    out: &mut ResolvedVenue,
    node: &Node,
    members: u32,
    sockets: &S,
    claimed: &BTreeSet<(&str, &str)>,
) {
    if node.kind == NodeKind::Array {
        for i in 0..members {
            collect_dangling(out, node, &format!("{}#{i}", node.id), sockets, claimed);
        }
        return;
    }
    collect_dangling(out, node, &node.id, sockets, claimed);
}

/// Every self-mating socket on `node` that no edge and no constraint claims,
/// reported against `as_node` (the node itself, or one derived array member).
///
/// Only [`crate::sockets::Polarity::Neutral`] joints count: a deck's four
/// corners are not open ends for having nothing standing on them.
fn collect_dangling<S: NodeSockets + ?Sized>(
    out: &mut ResolvedVenue,
    node: &Node,
    as_node: &str,
    sockets: &S,
    claimed: &BTreeSet<(&str, &str)>,
) {
    for socket in sockets.sockets(node) {
        if socket.socket_type.polarity() != crate::sockets::Polarity::Neutral {
            continue;
        }
        if claimed.contains(&(node.id.as_str(), socket.name.as_str())) {
            continue;
        }
        out.dangling.push(DanglingSocket {
            node: as_node.to_string(),
            socket: socket.name.clone(),
            socket_type: socket.socket_type,
        });
    }
}

fn evaluate_constraint<S: NodeSockets + ?Sized>(
    constraint: &Constraint,
    resolved: &ResolvedVenue,
    graph: &VenueGraph,
    sockets: &S,
) -> ConstraintReport {
    let world_point = |node_id: &str, socket_name: &str| -> Option<DVec3> {
        let pose = resolved.pose(node_id)?;
        let node = graph.node(node_id)?;
        let socket = match root_socket(socket_name) {
            Some(socket) if node.kind == NodeKind::Venue => socket,
            _ => find_socket(&sockets.sockets(node), socket_name)?,
        };
        Some(pose.world.transform_point3(socket.position))
    };
    let status = match (
        world_point(&constraint.node, &constraint.my_socket),
        world_point(&constraint.target_node, &constraint.target_socket),
    ) {
        (Some(a), Some(b)) => {
            let gap = a.distance(b);
            if gap <= CONSTRAINT_TOLERANCE_M {
                ConstraintStatus::Satisfied
            } else {
                ConstraintStatus::Violated { gap_m: gap }
            }
        }
        _ => ConstraintStatus::Dangling,
    };
    ConstraintReport {
        node: constraint.node.clone(),
        my_socket: constraint.my_socket.clone(),
        target_node: constraint.target_node.clone(),
        target_socket: constraint.target_socket.clone(),
        status,
    }
}

// ---------------------------------------------------------------------------
// Placing what used to be a stored pose
// ---------------------------------------------------------------------------

/// The `(u, v, yaw, trim)` that reproduces a world pose as a free placement on
/// a surface.
///
/// The inverse of [`place_on`], and the whole of what the
/// migration off `pos_*`/`rot_*` needs: an old row's world pose goes in, the
/// placement that lands the piece in the same spot comes out.
///
/// `held` is the socket on the piece that meets the surface, `host` the surface
/// socket in its own parent's frame; `parent_world` places that parent, and
/// `kind` picks which of the two mate conventions the pose is in (see
/// `mate_suffix`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePlacement {
    /// Metres along the host socket's tangent.
    pub u: f64,
    /// Metres along the host socket's bitangent.
    pub v: f64,
    /// Radians about the host socket's normal — the edge's `roll`.
    pub yaw: f64,
    /// Metres along **world up**, wherever the host surface faces (see
    /// `trim_axis`). `0` sits on the deck, `6.0` flies six metres above it,
    /// from the floor and from a down-facing grid alike.
    pub trim: f64,
}

impl SurfacePlacement {
    /// The two sockets seated on each other: no offset in the surface, no
    /// twist, no lift. [`place_on`] with this is the bare mate — the primitive
    /// [`crate::snap::solve_snap`] scores candidates over.
    pub const FLUSH: SurfacePlacement = SurfacePlacement {
        u: 0.0,
        v: 0.0,
        yaw: 0.0,
        trim: 0.0,
    };
}

/// Solve-and-invert: the placement whose [`resolve`] output is `world`.
///
/// Exact by construction for any pose the mate can produce, and the closest
/// placement — the twist about the normal, the offset in the plane — for one it
/// cannot. Read the residual by re-solving; the migration test does.
#[must_use]
pub fn invert_placement(
    world: DMat4,
    parent_world: DMat4,
    host: &ResolvedSocket,
    held: &ResolvedSocket,
    kind: NodeKind,
) -> SurfacePlacement {
    // `world = parent_world · host_frame · T · Rz · mate`, so
    // `T · Rz = (parent_world · host_frame)⁻¹ · world · mate⁻¹`.
    let seat =
        (parent_world * socket_frame(host)).inverse() * world * mate_suffix(kind, held).inverse();
    let offset = seat.w_axis.truncate();
    // The offset is `u·x̂ + v·ŷ + trim·up`, with `up` the host frame's view of
    // world up. Its `z` component is trim's alone, because `x̂` and `ŷ` have
    // none — so the split is one division, not a least-squares fit.
    let up = trim_axis(parent_world * socket_frame(host));
    let trim = if up.z.abs() < VERTICAL_HOST_EPSILON {
        0.0
    } else {
        offset.z / up.z
    };
    // `Rz(θ)` puts `(cos θ, sin θ, 0)` in its first column; reading the angle
    // off that column is exact for a pure twist and is the least-squares answer
    // for anything else.
    let yaw = seat.x_axis.y.atan2(seat.x_axis.x);
    SurfacePlacement {
        u: offset.x - trim * up.x,
        v: offset.y - trim * up.y,
        yaw,
        trim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A socket table keyed by catalog ref, which is all the resolver needs of
    /// the geometry — no GLB, no generator, no GPU.
    #[derive(Default)]
    struct Table(HashMap<String, Vec<ResolvedSocket>>);

    impl Table {
        fn with(mut self, id: &str, sockets: Vec<ResolvedSocket>) -> Self {
            self.0.insert(id.to_string(), sockets);
            self
        }
    }

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

    /// A deck: a bottom mount, a top surface, and two self-mating edges.
    fn deck() -> Vec<ResolvedSocket> {
        vec![
            socket(
                "bottom",
                SocketType::BottomMount,
                DVec3::new(0.0, -0.5, 0.0),
                DVec3::NEG_Y,
            ),
            socket(
                "top",
                SocketType::FloorTop,
                DVec3::new(0.0, 0.5, 0.0),
                DVec3::Y,
            ),
            socket(
                "edge_left",
                SocketType::FloorEdge,
                DVec3::new(-0.5, 0.5, 0.0),
                DVec3::NEG_X,
            ),
            socket(
                "edge_right",
                SocketType::FloorEdge,
                DVec3::new(0.5, 0.5, 0.0),
                DVec3::X,
            ),
        ]
    }

    /// A stick with two truss ends a metre apart along X, plus the underside
    /// that lets it sit on a surface.
    fn truss() -> Vec<ResolvedSocket> {
        vec![
            socket(
                "base",
                SocketType::BottomMount,
                DVec3::new(0.0, -0.15, 0.0),
                DVec3::NEG_Y,
            ),
            socket(
                "end_a",
                SocketType::TrussEnd,
                DVec3::new(-0.5, 0.0, 0.0),
                DVec3::NEG_X,
            ),
            socket(
                "end_b",
                SocketType::TrussEnd,
                DVec3::new(0.5, 0.0, 0.0),
                DVec3::X,
            ),
        ]
    }

    fn mover() -> Vec<ResolvedSocket> {
        vec![socket(
            "clamp",
            SocketType::EquipmentMount,
            DVec3::new(0.0, -0.1, 0.0),
            DVec3::NEG_Y,
        )]
    }

    fn table() -> Table {
        Table::default()
            .with("deck", deck())
            .with("truss", truss())
            .with("mover", mover())
    }

    fn node(id: &str, kind: NodeKind, catalog_ref: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            catalog_ref: Some(catalog_ref.to_string()),
            label: None,
            params: Params::default(),
        }
    }

    fn root() -> Node {
        Node {
            id: "venue".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        }
    }

    fn on_floor(u: f64, v: f64, trim: f64) -> Params {
        let mut p = Params::default();
        p.set("u", u);
        p.set("v", v);
        p.set("trim", trim);
        p
    }

    fn floor_edge(yaw: f64) -> Edge {
        Edge {
            parent: "venue".into(),
            my_socket: "bottom".into(),
            their_socket: FLOOR_SOCKET.into(),
            roll: yaw,
        }
    }

    // -----------------------------------------------------------------------

    #[test]
    fn kind_names_round_trip() {
        for kind in NodeKind::ALL {
            assert_eq!(NodeKind::from_name(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn the_root_resolves_to_the_identity() {
        let graph = VenueGraph::new(root());
        let resolved = resolve(&graph, &table());
        assert_eq!(resolved.poses().count(), 1);
        assert_eq!(resolved.pose("venue").unwrap().world, DMat4::IDENTITY);
    }

    #[test]
    fn a_free_placement_lands_where_its_params_say() {
        let mut graph = VenueGraph::new(root());
        let mut deck_node = node("deck1", NodeKind::Stage, "deck");
        deck_node.params = on_floor(2.0, -3.0, 0.0);
        graph.insert(deck_node);
        graph
            .attach("deck1", floor_edge(0.0), &table())
            .expect("a deck bottom mates the floor");

        let resolved = resolve(&graph, &table());
        let pose = resolved.pose("deck1").expect("placed");
        // The bottom socket sits at y = -0.5 in the deck's own frame, so the
        // deck's origin lifts by half its height to put that socket on the
        // floor. `v` runs along the floor socket's bitangent, which is three
        // space `-Z` — see `floor_socket`.
        let origin = pose.world.transform_point3(DVec3::ZERO);
        assert!(
            origin.abs_diff_eq(DVec3::new(2.0, 0.5, 3.0), 1e-12),
            "landed at {origin:?}"
        );
    }

    /// `(u, v, yaw, trim)` in, the same four out, through the full solve.
    #[test]
    fn free_placement_round_trips_through_the_solve() {
        for (u, v, yaw, trim) in [
            (0.0, 0.0, 0.0, 0.0),
            (2.5, -1.25, 0.7, 0.0),
            (-4.0, 6.0, -2.1, 3.5),
        ] {
            let mut graph = VenueGraph::new(root());
            let mut deck_node = node("deck1", NodeKind::Stage, "deck");
            deck_node.params = on_floor(u, v, trim);
            graph.insert(deck_node);
            graph.attach("deck1", floor_edge(yaw), &table()).unwrap();

            let resolved = resolve(&graph, &table());
            let world = resolved.pose("deck1").unwrap().world;
            let back = invert_placement(
                world,
                DMat4::IDENTITY,
                &floor_socket(),
                &find_socket(&deck(), "bottom").unwrap(),
                NodeKind::Stage,
            );
            assert!((back.u - u).abs() < 1e-9, "u: {back:?}");
            assert!((back.v - v).abs() < 1e-9, "v: {back:?}");
            assert!((back.trim - trim).abs() < 1e-9, "trim: {back:?}");
            assert!((back.yaw - yaw).abs() < 1e-9, "yaw: {back:?} wanted {yaw}");
        }
    }

    #[test]
    fn a_child_moves_with_its_parent() {
        let mut graph = VenueGraph::new(root());
        let mut deck_node = node("deck1", NodeKind::Stage, "deck");
        deck_node.params = on_floor(1.0, 0.0, 0.0);
        graph.insert(deck_node);
        graph.attach("deck1", floor_edge(0.0), &table()).unwrap();
        graph.insert(node("deck2", NodeKind::Stage, "deck"));
        graph
            .attach(
                "deck2",
                Edge {
                    parent: "deck1".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();

        let before = resolve(&graph, &table())
            .pose("deck2")
            .unwrap()
            .world
            .transform_point3(DVec3::ZERO);

        // Slide the parent 5 m and the child comes with it, exactly.
        let mut moved = graph.clone();
        moved.nodes.get_mut("deck1").unwrap().params = on_floor(6.0, 0.0, 0.0);
        let after = resolve(&moved, &table())
            .pose("deck2")
            .unwrap()
            .world
            .transform_point3(DVec3::ZERO);
        assert!(
            (after - before).abs_diff_eq(DVec3::new(5.0, 0.0, 0.0), 1e-12),
            "{before:?} -> {after:?}"
        );
    }

    #[test]
    fn the_solve_is_deterministic() {
        let mut graph = VenueGraph::new(root());
        for id in ["c", "a", "b"] {
            let mut n = node(id, NodeKind::Stage, "deck");
            n.params = on_floor(1.0, 2.0, 0.0);
            graph.insert(n);
            graph.attach(id, floor_edge(0.0), &table()).unwrap();
        }
        let a = resolve(&graph, &table());
        let b = resolve(&graph, &table());
        let names =
            |r: &ResolvedVenue| -> Vec<String> { r.poses().map(|p| p.node.clone()).collect() };
        assert_eq!(names(&a), names(&b));
        assert_eq!(names(&a), ["venue", "a", "b", "c"]);
        for (x, y) in a.poses().zip(b.poses()) {
            assert_eq!(x.world.to_cols_array(), y.world.to_cols_array());
        }
    }

    #[test]
    fn depth_first_with_children_in_id_order() {
        let mut graph = VenueGraph::new(root());
        let mut z = node("z", NodeKind::Stage, "deck");
        z.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(z);
        graph.attach("z", floor_edge(0.0), &table()).unwrap();
        for id in ["z_b", "z_a"] {
            graph.insert(node(id, NodeKind::Stage, "deck"));
            graph
                .attach(
                    id,
                    Edge {
                        parent: "z".into(),
                        my_socket: "edge_left".into(),
                        their_socket: "edge_right".into(),
                        roll: 0.0,
                    },
                    &table(),
                )
                .unwrap();
        }
        let mut a = node("a", NodeKind::Stage, "deck");
        a.params = on_floor(9.0, 0.0, 0.0);
        graph.insert(a);
        graph.attach("a", floor_edge(0.0), &table()).unwrap();

        let order: Vec<String> = resolve(&graph, &table())
            .poses()
            .map(|p| p.node.clone())
            .collect();
        assert_eq!(order, ["venue", "a", "z", "z_a", "z_b"]);
    }

    #[test]
    fn a_cycle_is_refused_at_insert() {
        let mut graph = VenueGraph::new(root());
        for id in ["a", "b"] {
            graph.insert(node(id, NodeKind::Stage, "deck"));
        }
        graph.attach("a", floor_edge(0.0), &table()).unwrap();
        graph
            .attach(
                "b",
                Edge {
                    parent: "a".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        let err = graph
            .attach(
                "a",
                Edge {
                    parent: "b".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .expect_err("a inside b inside a");
        assert_eq!(
            err,
            EdgeError::Cycle {
                child: "a".into(),
                parent: "b".into()
            }
        );
    }

    #[test]
    fn attaching_a_node_to_itself_is_a_cycle() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("a", NodeKind::Stage, "deck"));
        assert!(matches!(
            graph.attach(
                "a",
                Edge {
                    parent: "a".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            ),
            Err(EdgeError::Cycle { .. })
        ));
    }

    #[test]
    fn the_root_cannot_be_attached() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("a", NodeKind::Stage, "deck"));
        assert_eq!(
            graph.attach("venue", floor_edge(0.0), &table()),
            Err(EdgeError::RootHasNoParent)
        );
    }

    #[test]
    fn polarity_refuses_two_receptacles() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("deck1", NodeKind::Stage, "deck"));
        // `top` is a `FloorTop`: female, a host only. It cannot be held.
        let err = graph
            .attach(
                "deck1",
                Edge {
                    parent: "venue".into(),
                    my_socket: "top".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .expect_err("two female sockets");
        assert_eq!(
            err,
            EdgeError::Polarity {
                held: SocketType::FloorTop,
                host: SocketType::Ground
            }
        );
    }

    #[test]
    fn polarity_refuses_two_different_joints() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("deck1", NodeKind::Stage, "deck"));
        graph.attach("deck1", floor_edge(0.0), &table()).unwrap();
        graph.insert(node("t", NodeKind::Run, "truss"));
        // A truss end is a `TrussEnd`; a deck edge is an `Edge`. Neutral both,
        // but not the same joint.
        assert_eq!(
            graph.attach(
                "t",
                Edge {
                    parent: "deck1".into(),
                    my_socket: "end_a".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            ),
            Err(EdgeError::Polarity {
                held: SocketType::TrussEnd,
                host: SocketType::FloorEdge
            })
        );
    }

    #[test]
    fn an_unknown_socket_is_refused_by_name() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("deck1", NodeKind::Stage, "deck"));
        assert_eq!(
            graph.attach(
                "deck1",
                Edge {
                    parent: "venue".into(),
                    my_socket: "nope".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            ),
            Err(EdgeError::MissingSocket {
                node: "deck1".into(),
                socket: "nope".into()
            })
        );
    }

    #[test]
    fn an_array_expands_at_solve() {
        let mut graph = VenueGraph::new(root());
        let mut deck_node = node("deck1", NodeKind::Stage, "deck");
        deck_node.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(deck_node);
        graph.attach("deck1", floor_edge(0.0), &table()).unwrap();

        let mut array = node("bar", NodeKind::Array, "mover");
        array.params.set("count", 5.0);
        array.params.set("span", 4.0);
        graph.insert(array);
        graph
            .attach(
                "bar",
                Edge {
                    parent: "deck1".into(),
                    my_socket: "clamp".into(),
                    their_socket: "top".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();

        let resolved = resolve(&graph, &table());
        let members: Vec<&NodePose> = resolved
            .poses()
            .filter(|p| p.array_index.is_some())
            .collect();
        assert_eq!(members.len(), 5);
        assert_eq!(
            members.iter().map(|m| m.node.as_str()).collect::<Vec<_>>(),
            ["bar#0", "bar#1", "bar#2", "bar#3", "bar#4"]
        );
        // Spread evenly over the span, centred on the mate point.
        let xs: Vec<f64> = members
            .iter()
            .map(|m| m.world.transform_point3(DVec3::ZERO).x)
            .collect();
        for (i, x) in xs.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let want = -2.0 + i as f64;
            assert!((x - want).abs() < 1e-12, "{i}: {x} wanted {want}");
        }
        // The array node itself is placed at its anchor — the seat the span
        // is centred on — so a successful array reports `ok` with a parent.
        let anchor = resolved.pose("bar").expect("the array node is placed");
        assert_eq!(anchor.array_index, None);
        assert!(anchor.world.transform_point3(DVec3::ZERO).x.abs() < 1e-12);
        let placement = resolved.placement("bar");
        assert!(placement.ok);
        assert_eq!(placement.parent.as_deref(), Some("deck1"));
        // Members are derived from the generator, so they name it as parent.
        assert!(members.iter().all(|m| m.parent.as_deref() == Some("bar")));
    }

    #[test]
    fn a_single_member_array_sits_on_the_mate_point() {
        let mut graph = VenueGraph::new(root());
        let mut deck_node = node("deck1", NodeKind::Stage, "deck");
        deck_node.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(deck_node);
        graph.attach("deck1", floor_edge(0.0), &table()).unwrap();
        let mut array = node("bar", NodeKind::Array, "mover");
        array.params.set("count", 1.0);
        array.params.set("span", 4.0);
        graph.insert(array);
        graph
            .attach(
                "bar",
                Edge {
                    parent: "deck1".into(),
                    my_socket: "clamp".into(),
                    their_socket: "top".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        let resolved = resolve(&graph, &table());
        let origin = |id: &str| {
            resolved
                .pose(id)
                .unwrap_or_else(|| panic!("{id} is placed"))
                .world
                .transform_point3(DVec3::ZERO)
        };
        // The lone member sits on the anchor, which is the whole meaning of
        // "centred on the mate point".
        assert!(origin("bar#0").abs_diff_eq(origin("bar"), 1e-12));
        assert!(origin("bar#0").x.abs() < 1e-12);
    }

    /// A child hung off an array lands on the generator's anchor rather than
    /// silently dropping out of the solve for want of a frame.
    #[test]
    fn a_child_of_an_array_hangs_off_its_anchor() {
        let mut graph = VenueGraph::new(root());
        let mut deck_node = node("deck1", NodeKind::Stage, "deck");
        deck_node.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(deck_node);
        graph.attach("deck1", floor_edge(0.0), &table()).unwrap();
        let mut array = node("bar", NodeKind::Array, "deck");
        array.params.set("count", 3.0);
        array.params.set("span", 4.0);
        graph.insert(array);
        graph
            .attach(
                "bar",
                Edge {
                    parent: "deck1".into(),
                    my_socket: "bottom".into(),
                    their_socket: "top".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        graph.insert(node("hanger", NodeKind::Stage, "deck"));
        graph
            .attach(
                "hanger",
                Edge {
                    parent: "bar".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        let resolved = resolve(&graph, &table());
        assert!(resolved.placement("hanger").ok);
    }

    /// Beam = mount normal: a fixture on the floor points up, one under a
    /// surface facing down points down, with no per-fixture rest axis.
    #[test]
    fn a_fixture_faces_out_of_its_mount() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("light", NodeKind::Fixture, "mover"));
        graph
            .attach(
                "light",
                Edge {
                    parent: "venue".into(),
                    my_socket: "clamp".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        let resolved = resolve(&graph, &table());
        let pose = resolved.pose("light").unwrap();
        let (_, rot) = pose.data_basis();
        // `Mount::normal()` is `rotation * REST_AXIS`, and `REST_AXIS` is -Z.
        let normal = rot * DVec3::NEG_Z;
        assert!(
            normal.abs_diff_eq(DVec3::Z, 1e-12),
            "a floor light points {normal:?}, wanted +Z (up)"
        );
    }

    /// The grid is the floor's opposite: the same `(u, v, trim)`, a beam that
    /// points down.
    #[test]
    fn a_flown_fixture_points_down() {
        let mut graph = VenueGraph::new(root());
        let mut light = node("light", NodeKind::Fixture, "mover");
        light.params = on_floor(1.0, 2.0, 6.0);
        graph.insert(light);
        graph
            .attach(
                "light",
                Edge {
                    parent: "venue".into(),
                    my_socket: "clamp".into(),
                    their_socket: RIG_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        let resolved = resolve(&graph, &table());
        let pose = resolved.pose("light").unwrap();
        let (position, rot) = pose.data_basis();
        assert!(
            (rot * DVec3::NEG_Z).abs_diff_eq(DVec3::NEG_Z, 1e-12),
            "a flown light points {:?}",
            rot * DVec3::NEG_Z
        );
        // `trim` still lifts: the grid's normal is down, so the mount rises.
        assert!((position.z - 6.0).abs() < 1e-12, "at {position:?}");
    }

    #[test]
    fn a_constraint_reports_satisfied_violated_and_dangling() {
        let mut graph = VenueGraph::new(root());
        let mut a = node("a", NodeKind::Run, "truss");
        a.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(a);
        graph
            .attach(
                "a",
                Edge {
                    parent: "venue".into(),
                    my_socket: "base".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        graph.insert(node("b", NodeKind::Run, "truss"));
        graph
            .attach(
                "b",
                Edge {
                    parent: "a".into(),
                    my_socket: "end_a".into(),
                    their_socket: "end_b".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();

        // b's far end meets its own near end's host — trivially satisfied by
        // pointing the check at the socket it is already bolted to.
        graph.constrain(Constraint {
            node: "b".into(),
            my_socket: "end_a".into(),
            target_node: "a".into(),
            target_socket: "end_b".into(),
        });
        // b's other end is a metre away from a's other end.
        graph.constrain(Constraint {
            node: "b".into(),
            my_socket: "end_b".into(),
            target_node: "a".into(),
            target_socket: "end_a".into(),
        });
        // Nothing called `ghost` exists.
        graph.constrain(Constraint {
            node: "b".into(),
            my_socket: "end_b".into(),
            target_node: "ghost".into(),
            target_socket: "end_a".into(),
        });

        let resolved = resolve(&graph, &table());
        let statuses: Vec<ConstraintStatus> =
            resolved.constraints().iter().map(|c| c.status).collect();
        assert_eq!(statuses[0], ConstraintStatus::Satisfied);
        assert!(matches!(statuses[1], ConstraintStatus::Violated { .. }));
        assert_eq!(statuses[2], ConstraintStatus::Dangling);
    }

    #[test]
    fn edge_mode_is_identity_not_a_flip() {
        // The TS docstrings claimed a 180° rotation about the host normal;
        // `flipFor()` returned identity. The code is what shipped.
        assert_eq!(flip_for(SocketMode::Edge), DMat4::IDENTITY);
        let face = flip_for(SocketMode::Face);
        assert!((face.y_axis.y + 1.0).abs() < 1e-12);
        assert!((face.z_axis.z + 1.0).abs() < 1e-12);
    }

    /// Both halves of a joint are spoken for. Two decks butted edge to edge
    /// leave four open edges between them, not six.
    #[test]
    fn neither_half_of_a_mated_edge_is_dangling() {
        let mut graph = VenueGraph::new(root());
        let mut deck_a = node("deck_a", NodeKind::Stage, "deck");
        deck_a.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(deck_a);
        graph.attach("deck_a", floor_edge(0.0), &table()).unwrap();
        graph.insert(node("deck_b", NodeKind::Stage, "deck"));
        graph
            .attach(
                "deck_b",
                Edge {
                    parent: "deck_a".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();

        let resolved = resolve(&graph, &table());
        let open: Vec<(&str, &str)> = resolved
            .dangling()
            .iter()
            .map(|d| (d.node.as_str(), d.socket.as_str()))
            .collect();
        assert_eq!(
            open,
            [("deck_a", "edge_left"), ("deck_b", "edge_right")],
            "a bolted edge is not an open one"
        );
    }

    /// A far end is what the builder writes down instead of a second parent,
    /// so the socket it checks is not open either.
    #[test]
    fn a_constrained_far_end_is_not_dangling() {
        let mut graph = VenueGraph::new(root());
        let mut a = node("a", NodeKind::Run, "truss");
        a.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(a);
        graph
            .attach(
                "a",
                Edge {
                    parent: "venue".into(),
                    my_socket: "base".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        assert_eq!(
            resolve(&graph, &table())
                .dangling()
                .iter()
                .filter(|d| d.node == "a")
                .count(),
            2,
            "both ends start open"
        );

        graph.constrain(Constraint {
            node: "a".into(),
            my_socket: "end_b".into(),
            target_node: "ghost".into(),
            target_socket: "end_a".into(),
        });
        let resolved = resolve(&graph, &table());
        let open: Vec<&str> = resolved
            .dangling()
            .iter()
            .filter(|d| d.node == "a")
            .map(|d| d.socket.as_str())
            .collect();
        assert_eq!(open, ["end_a"], "the checked end is accounted for");
    }

    #[test]
    fn an_open_truss_end_is_dangling() {
        let mut graph = VenueGraph::new(root());
        let mut a = node("a", NodeKind::Run, "truss");
        a.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(a);
        graph
            .attach(
                "a",
                Edge {
                    parent: "venue".into(),
                    my_socket: "base".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        let resolved = resolve(&graph, &table());
        let names: Vec<&str> = resolved
            .dangling()
            .iter()
            .filter(|d| d.node == "a")
            .map(|d| d.socket.as_str())
            .collect();
        assert_eq!(names, ["end_a", "end_b"]);
        // The deck's `FloorTop` is female, so an empty deck top is not
        // "dangling" — nobody promised anything would stand on it.
        assert!(resolved.dangling().iter().all(|d| d.node != "venue"));
    }

    #[test]
    fn an_unplaced_node_gets_no_pose_and_is_named() {
        let mut graph = VenueGraph::new(root());
        graph.insert(node("tray", NodeKind::Fixture, "mover"));
        let resolved = resolve(&graph, &table());
        assert!(resolved.pose("tray").is_none());
        assert!(!resolved.placement("tray").ok);
        // No pose, but not silence: a fixture in the tray is the legitimate
        // case, and the caller has to be able to tell it from a lost one.
        let unplaced = resolved.unplaced();
        assert_eq!(unplaced.len(), 1);
        assert_eq!(unplaced[0].node, "tray");
        assert_eq!(unplaced[0].kind, NodeKind::Fixture);
        assert_eq!(unplaced[0].descendants, 0);
    }

    /// `detach` leaves the whole branch without a parent. Only its root is
    /// listed — one reason, said once — with the branch's size alongside.
    #[test]
    fn a_detached_subtree_is_reported_by_its_root() {
        let mut graph = VenueGraph::new(root());
        let mut deck_node = node("deck1", NodeKind::Stage, "deck");
        deck_node.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(deck_node);
        graph.attach("deck1", floor_edge(0.0), &table()).unwrap();
        graph.insert(node("deck2", NodeKind::Stage, "deck"));
        graph
            .attach(
                "deck2",
                Edge {
                    parent: "deck1".into(),
                    my_socket: "edge_left".into(),
                    their_socket: "edge_right".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        graph.insert(node("head", NodeKind::Fixture, "mover"));
        graph
            .attach(
                "head",
                Edge {
                    parent: "deck2".into(),
                    my_socket: "clamp".into(),
                    their_socket: "top".into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        assert!(resolve(&graph, &table()).unplaced().is_empty());

        graph.detach("deck2");
        let resolved = resolve(&graph, &table());
        assert!(resolved.pose("deck2").is_none());
        assert!(resolved.pose("head").is_none(), "the branch came along");
        let unplaced = resolved.unplaced();
        assert_eq!(
            unplaced.iter().map(|u| u.node.as_str()).collect::<Vec<_>>(),
            ["deck2"],
            "the root of the branch, not every node in it"
        );
        assert_eq!(unplaced[0].descendants, 1);
    }

    /// Every member is a real truss standing in the room; the anchor is a seat
    /// with no geometry. Reporting the anchor once under-counts the open ends
    /// by `members - 1` and names a node nobody can walk up to.
    #[test]
    fn an_array_reports_every_members_open_ends() {
        let mut graph = VenueGraph::new(root());
        let mut array = node("wall", NodeKind::Array, "truss");
        array.params = on_floor(0.0, 0.0, 0.0);
        array.params.set("count", 3.0);
        array.params.set("span", 4.0);
        graph.insert(array);
        graph
            .attach(
                "wall",
                Edge {
                    parent: "venue".into(),
                    my_socket: "base".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();

        let resolved = resolve(&graph, &table());
        let open: Vec<(&str, &str)> = resolved
            .dangling()
            .iter()
            .map(|d| (d.node.as_str(), d.socket.as_str()))
            .collect();
        assert_eq!(
            open,
            [
                ("wall#0", "end_a"),
                ("wall#0", "end_b"),
                ("wall#1", "end_a"),
                ("wall#1", "end_b"),
                ("wall#2", "end_a"),
                ("wall#2", "end_b"),
            ],
            "three trusses have three pairs of ends"
        );
        // The report for the call that made them gathers its members'.
        assert_eq!(resolved.placement("wall").dangling.len(), 6);
    }

    #[test]
    fn an_array_anchor_is_not_a_set_piece() {
        let mut graph = VenueGraph::new(root());
        let mut array = node("wall", NodeKind::Array, "truss");
        array.params = on_floor(0.0, 0.0, 0.0);
        array.params.set("count", 3.0);
        array.params.set("span", 4.0);
        graph.insert(array);
        graph
            .attach(
                "wall",
                Edge {
                    parent: "venue".into(),
                    my_socket: "base".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        graph.insert(node("tray", NodeKind::Fixture, "mover"));

        let resolved = resolve(&graph, &table());
        let drawn: Vec<&str> = resolved
            .poses()
            .filter(|p| p.is_set_piece())
            .map(|p| p.node.as_str())
            .collect();
        assert_eq!(
            drawn,
            ["wall#0", "wall#1", "wall#2"],
            "three trusses, not four; the room and the light are not set pieces"
        );
    }

    #[test]
    fn a_bolted_joint_ignores_a_requested_roll() {
        let mut graph = VenueGraph::new(root());
        let mut a = node("a", NodeKind::Run, "truss");
        a.params = on_floor(0.0, 0.0, 0.0);
        graph.insert(a);
        graph
            .attach(
                "a",
                Edge {
                    parent: "venue".into(),
                    my_socket: "base".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                &table(),
            )
            .unwrap();
        graph.insert(node("b", NodeKind::Run, "truss"));
        graph
            .attach(
                "b",
                Edge {
                    parent: "a".into(),
                    my_socket: "end_a".into(),
                    their_socket: "end_b".into(),
                    roll: 1.0,
                },
                &table(),
            )
            .unwrap();
        let resolved = resolve(&graph, &table());
        assert!(matches!(
            resolved.placement("b").warnings.as_slice(),
            [Warning::RollClamped { applied, .. }] if applied.abs() < f64::EPSILON
        ));
    }

    #[test]
    fn subtree_is_the_node_and_everything_under_it() {
        let mut graph = VenueGraph::new(root());
        for id in ["a", "b", "c"] {
            graph.insert(node(id, NodeKind::Stage, "deck"));
        }
        graph.attach("a", floor_edge(0.0), &table()).unwrap();
        for (child, parent) in [("b", "a"), ("c", "b")] {
            graph
                .attach(
                    child,
                    Edge {
                        parent: parent.into(),
                        my_socket: "edge_left".into(),
                        their_socket: "edge_right".into(),
                        roll: 0.0,
                    },
                    &table(),
                )
                .unwrap();
        }
        assert_eq!(graph.subtree("a"), ["a", "b", "c"]);
        assert_eq!(graph.subtree("c"), ["c"]);
    }
}
