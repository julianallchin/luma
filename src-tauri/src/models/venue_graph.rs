//! The venue graph across the host boundary.
//!
//! [`luma_scene::venue`] owns the model; these are its rows and its solved
//! output in the shapes `ts-rs` and `serde` can carry. They are a *projection*,
//! not a second declaration: every field here is read off a
//! [`luma_scene::venue::Node`] / [`luma_scene::venue::NodePose`], and the
//! vocabularies (`kind`, socket names, param keys) are the crate's strings.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

use luma_scene::venue::{
    ConstraintStatus, DanglingSocket, NodePose, NodeWarning, Outcome, ResolvedVenue as Solved,
    UnplacedNode, VenueGraph, Warning,
};

/// One `venue_nodes` row.
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct VenueNode {
    pub id: String,
    pub venue_id: String,
    /// `venue` | `stage` | `run` | `tower` | `piece` | `fixture` | `array`.
    pub kind: String,
    /// A catalog piece id, or a `fixtures` row id for a fixture node.
    pub catalog_ref: Option<String>,
    pub label: Option<String>,
}

/// One `venue_edges` row: the relation that produces a pose.
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct VenueEdge {
    pub child_id: String,
    pub parent_id: String,
    pub my_socket: String,
    pub their_socket: String,
    /// Radians about the shared normal — *yaw* on a surface placement.
    pub roll: f64,
}

/// One `venue_constraints` row: a far end, checked after the solve.
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct VenueConstraint {
    pub node_id: String,
    pub my_socket: String,
    pub target_node: String,
    pub target_socket: String,
}

/// A whole venue's rows, as `get_venue_graph` returns them.
///
/// Params come as one map per node rather than a flat row list: a caller that
/// wants a node's trim should not have to filter, and the database's shape is
/// its own business.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct VenueGraphRows {
    pub nodes: Vec<VenueNode>,
    pub edges: Vec<VenueEdge>,
    pub params: BTreeMap<String, BTreeMap<String, f64>>,
    pub constraints: Vec<VenueConstraint>,
}

impl VenueGraphRows {
    /// The rows as the model the resolver walks.
    ///
    /// A row naming an unknown `kind` becomes no node: the alphabet is closed,
    /// and inventing a kind for it would put something in the tree that no code
    /// below knows how to draw. It is reported as a
    /// [`Warning::UnknownKind`] on the solved venue rather than refused,
    /// because the rest of the room is still a room — the same reason a piece
    /// whose catalog entry is gone is drawn at its parent's origin instead of
    /// failing the load. Its edge is not a relation this graph holds,
    /// so whatever hung off it is reported unplaced.
    #[must_use]
    pub fn to_graph(&self) -> Option<VenueGraph> {
        use luma_scene::venue::{Constraint, Edge, Node, NodeKind};

        let node_of = |row: &VenueNode| -> Option<Node> {
            Some(Node {
                id: row.id.clone(),
                kind: NodeKind::from_name(&row.kind)?,
                catalog_ref: row.catalog_ref.clone(),
                label: row.label.clone(),
                params: self
                    .params
                    .get(&row.id)
                    .map(|p| p.iter().map(|(k, v)| (k.clone(), *v)).collect())
                    .unwrap_or_default(),
            })
        };

        let root = self
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Venue.as_str())
            .and_then(node_of)?;
        // The two tables are joined here rather than loaded in sequence: a
        // node arrives with the edge that places it, so an edge belonging to a
        // row that became no node is not a relation this graph ever holds.
        let placements: BTreeMap<&str, Edge> = self
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.child_id.as_str(),
                    Edge {
                        parent: edge.parent_id.clone(),
                        my_socket: edge.my_socket.clone(),
                        their_socket: edge.their_socket.clone(),
                        roll: edge.roll,
                    },
                )
            })
            .collect();

        // Rows go in with `insert_placed`, not `attach`: they were already
        // admitted when they were written, and re-checking them here would mean
        // a venue whose catalog entry has since been dropped stops loading at
        // all rather than reporting one dangling piece.
        let mut graph = VenueGraph::new(root);
        for row in &self.nodes {
            let Some(node) = node_of(row) else {
                graph.warn(&row.id, Warning::UnknownKind(row.kind.clone()));
                continue;
            };
            if node.id == graph.root() {
                continue;
            }
            match placements.get(row.id.as_str()) {
                Some(edge) => graph.insert_placed(node, edge.clone()),
                None => graph.insert(node),
            }
        }
        // Constraints go in with `load_constraint` for `insert_placed`'s
        // reason: they were admitted when they were written, and a check whose
        // target has since been deleted reports `Dangling` and claims nothing
        // rather than costing the venue the rest of its paperwork.
        for c in &self.constraints {
            graph.load_constraint(Constraint {
                node: c.node_id.clone(),
                my_socket: c.my_socket.clone(),
                target_node: c.target_node.clone(),
                target_socket: c.target_socket.clone(),
            });
        }
        Some(graph)
    }
}

/// One node's solved pose, in the stored convention: metres and radians, data
/// space (Z-up).
///
/// This is what `stage_pieces.pos_*`/`rot_*` and `fixtures.pos_*`/`rot_*` used
/// to hold, and it is derived on every read rather than stored — see the design
/// doc's "Live graph, no baked poses, no hybrid."
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct ResolvedNode {
    /// The node id, or `"<array id>#<index>"` for a derived array member.
    pub id: String,
    pub kind: String,
    pub catalog_ref: Option<String>,
    pub label: Option<String>,
    pub parent_id: Option<String>,
    pub position: [f64; 3],
    /// The stored Euler triple, radians.
    pub rotation: [f64; 3],
    /// Unit vector, data space, that a parked head at this node emits along.
    /// Meaningful for a fixture; for a piece it is the mount normal its own
    /// frame implies, and nothing reads it.
    pub facing: [f64; 3],
    /// Which member of an array this is.
    pub array_index: Option<u32>,
    /// Whether this pose stands for one physical object the set-piece layer
    /// draws — [`NodePose::is_set_piece`]. Carried rather than re-derived, so
    /// the renderer, the agent binding and the React store cannot disagree
    /// about what is in the room.
    pub set_piece: bool,
    pub params: BTreeMap<String, f64>,
}

impl From<&NodePose> for ResolvedNode {
    fn from(pose: &NodePose) -> Self {
        let (position, rotation) = pose.data_pose();
        let (_, basis) = pose.data_basis();
        Self {
            id: pose.node.clone(),
            kind: pose.kind.as_str().to_string(),
            catalog_ref: pose.catalog_ref.clone(),
            label: pose.label.clone(),
            parent_id: pose.parent.clone(),
            position,
            rotation,
            // `REST_AXIS` is `-Z`; the basis takes it to the beam.
            facing: (basis * glam::DVec3::NEG_Z).to_array(),
            array_index: pose.array_index,
            set_piece: pose.is_set_piece(),
            params: pose
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }
}

/// A far end, evaluated.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct ResolvedConstraint {
    pub node_id: String,
    pub my_socket: String,
    pub target_node: String,
    pub target_socket: String,
    /// `satisfied` | `violated` | `dangling`.
    pub status: String,
    /// How far apart the two ends are, when both resolved.
    pub gap_m: Option<f64>,
}

/// A subtree the solve never reached, by its root — the patch tray, and what
/// `detach` leaves behind. See [`luma_scene::venue::UnplacedNode`].
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct ResolvedUnplaced {
    pub node_id: String,
    pub kind: String,
    pub label: Option<String>,
    /// How many nodes hang off it, not counting itself.
    pub descendants: u32,
}

impl From<&UnplacedNode> for ResolvedUnplaced {
    fn from(unplaced: &UnplacedNode) -> Self {
        ResolvedUnplaced {
            node_id: unplaced.node.clone(),
            kind: unplaced.kind.as_str().to_string(),
            label: unplaced.label.clone(),
            descendants: u32::try_from(unplaced.descendants).unwrap_or(u32::MAX),
        }
    }
}

/// An open structural socket.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct ResolvedDangling {
    pub node_id: String,
    pub socket: String,
    pub socket_type: String,
}

impl From<&DanglingSocket> for ResolvedDangling {
    fn from(dangling: &DanglingSocket) -> Self {
        ResolvedDangling {
            node_id: dangling.node.clone(),
            socket: dangling.socket.clone(),
            socket_type: dangling.socket_type.as_str().to_string(),
        }
    }
}

/// The whole venue, solved — what `get_resolved_venue` returns and what every
/// consumer draws from.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct ResolvedVenue {
    /// Depth-first from the root, children in id order. Deterministic.
    pub nodes: Vec<ResolvedNode>,
    pub constraints: Vec<ResolvedConstraint>,
    pub dangling: Vec<ResolvedDangling>,
    /// Every node the solve could not reach, by the root of its branch. A
    /// fixture in the patch tray is here, and so is anything just detached.
    pub unplaced: Vec<ResolvedUnplaced>,
    /// One line per thing the solve had to decide for the caller.
    pub warnings: Vec<String>,
}

impl From<&Solved> for ResolvedVenue {
    fn from(solved: &Solved) -> Self {
        Self {
            nodes: solved.poses().map(ResolvedNode::from).collect(),
            constraints: solved
                .constraints()
                .iter()
                .map(|c| ResolvedConstraint {
                    node_id: c.node.clone(),
                    my_socket: c.my_socket.clone(),
                    target_node: c.target_node.clone(),
                    target_socket: c.target_socket.clone(),
                    status: match c.status {
                        ConstraintStatus::Satisfied => "satisfied",
                        ConstraintStatus::Violated { .. } => "violated",
                        ConstraintStatus::Dangling => "dangling",
                    }
                    .to_string(),
                    gap_m: match c.status {
                        ConstraintStatus::Violated { gap_m } => Some(gap_m),
                        _ => None,
                    },
                })
                .collect(),
            dangling: solved
                .dangling()
                .iter()
                .map(ResolvedDangling::from)
                .collect(),
            unplaced: solved
                .unplaced()
                .iter()
                .map(ResolvedUnplaced::from)
                .collect(),
            warnings: solved.warnings().iter().map(warning_line).collect(),
        }
    }
}

/// One warning as a line: which node, and what the solve decided for it.
///
/// The single rendering of a [`NodeWarning`]. Every surface that shows one —
/// the resolved venue, a placement report, a distribution — goes through here,
/// so a warning reads the same wherever it surfaces and none of them falls back
/// to `{:?}`.
pub(crate) fn warning_line(warning: &NodeWarning) -> String {
    format!("{}: {}", warning.node, describe(&warning.warning))
}

fn describe(warning: &Warning) -> String {
    match warning {
        Warning::UnknownKind(kind) => format!("`{kind}` is not a node kind"),
        Warning::UnknownCatalogRef(id) => format!("`{id}` is not in the catalog"),
        Warning::MissingSocket(name) => format!("no socket `{name}`"),
        Warning::RollClamped { requested, applied } => {
            format!("this joint does not turn: {requested} rad became {applied}")
        }
        Warning::ArrayCountClamped { requested, applied } => {
            format!("an array of {requested} became {applied}")
        }
    }
}

/// What a mutating call reports back.
///
/// [`luma_scene::venue::Placement`] with the whole solved venue alongside it,
/// because every caller that changes one node then wants to redraw all of them
/// — and a second round trip to fetch that would be a second solve of the same
/// graph.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct PlacementReport {
    pub node_id: String,
    /// What the graph now says about the node. **Not** whether the call
    /// worked — a refusal is this call's `Err`, and never reaches here.
    pub outcome: PlacementOutcome,
    pub parent_id: Option<String>,
    pub warnings: Vec<String>,
    pub dangling: Vec<ResolvedDangling>,
    pub constraints: Vec<ResolvedConstraint>,
    pub venue: ResolvedVenue,
}

/// [`luma_scene::venue::Outcome`] on the wire.
#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub enum PlacementOutcome {
    /// The solve reached it: it has a pose, and it is in the room.
    Placed,
    /// No edge leads to it — a patched-but-unplaced fixture, or a detached
    /// branch. Its rows are still there.
    Unplaced,
}

impl PlacementOutcome {
    /// Whether the node is in the room.
    #[must_use]
    pub fn is_placed(self) -> bool {
        matches!(self, Self::Placed)
    }
}

impl From<Outcome> for PlacementOutcome {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Placed => Self::Placed,
            Outcome::Unplaced => Self::Unplaced,
        }
    }
}

impl PlacementReport {
    /// The report for one node out of a solved venue.
    #[must_use]
    pub fn of(node_id: &str, solved: &Solved) -> Self {
        let placement = solved.placement(node_id);
        let venue = ResolvedVenue::from(solved);
        Self {
            node_id: node_id.to_string(),
            outcome: placement.outcome.into(),
            parent_id: placement.parent,
            warnings: placement.warnings.iter().map(describe).collect(),
            dangling: venue
                .dangling
                .iter()
                .filter(|d| d.node_id == node_id)
                .cloned()
                .collect(),
            constraints: venue
                .constraints
                .iter()
                .filter(|c| c.node_id == node_id || c.target_node == node_id)
                .cloned()
                .collect(),
            venue,
        }
    }
}

/// What an extend ray met: the nearest compatible socket ahead of an open one,
/// and how far it is in a buildable span.
///
/// A measurement, not a placement — the builder shows it while a length is
/// being typed and [`crate::services::stage_ops::Stage::extend`] refuses
/// anything longer than it.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct Reach {
    pub node_id: String,
    pub socket: String,
    /// Centre-to-centre distance between the two socket points, quantized down
    /// to a buildable span.
    pub gap_m: f64,
}

/// The placeable vocabulary: what a caller may name in `attach`, `place` and
/// `extend`.
///
/// Derived from [`luma_scene::catalog`] and the same socket supply the resolver
/// mates against, so a socket listed here is a socket a verb will accept. There
/// is no hand-written table anywhere for this to drift from.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct StageCatalog {
    /// The node-kind alphabet a caller may place, root excluded — the root is
    /// made with the venue.
    pub kinds: Vec<String>,
    /// The venue root's own two synthesized surfaces: `floor` faces up, `rig`
    /// is the same plane facing down. Same origin, same `(u, v)`, same `trim`.
    pub root_sockets: Vec<String>,
    /// The step every generated span and every extend length is quantized to,
    /// in metres.
    pub length_step_m: f64,
    pub pieces: Vec<CatalogPiece>,
}

/// One catalog entry, with the sockets it actually resolves to.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct CatalogPiece {
    /// What `catalog_ref` holds.
    pub catalog_ref: String,
    pub name: String,
    /// Palette section: `Stage`, `Trusses`, `Speakers`, ...
    pub group: String,
    /// Snap taxonomy: `floor`, `truss`, `speaker`, `cdj`, ...
    pub piece_kind: String,
    /// Whether the shape comes from a generator, in which case its sockets are
    /// a function of its params and `span` is the one that moves them.
    pub procedural: bool,
    /// Resolved against this piece's **default** parameters. A generated piece
    /// keeps these socket names at every span; only where they are moves.
    pub sockets: Vec<CatalogSocket>,
}

/// One socket, in the vocabulary `attach` checks against.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venue-graph.ts")]
#[ts(rename_all = "camelCase")]
pub struct CatalogSocket {
    pub name: String,
    /// The named point's own type, e.g. `truss_end`, `floor_top`.
    pub socket_type: String,
    /// The equivalence class two sockets must share to mate at all:
    /// `surface`, `truss_end`, `edge`, `cable_end`, `grab`.
    pub joint: String,
    /// `male` (only ever held), `female` (only ever a host), `neutral`
    /// (self-mating, either).
    pub polarity: String,
    /// Whether a row of fixtures can be spread along it — a face with a length
    /// and a normal, as opposed to a bolt circle that takes one piece.
    pub feature: bool,
}

impl StageCatalog {
    /// Every piece the palette offers, resolved against `supply`.
    ///
    /// Sockets come from the supply rather than from [`luma_scene::catalog`]'s
    /// authored `SocketDef`s, because a procedural piece has none of those —
    /// its ends are the generator's frames, and the supply is the one place
    /// both cases are already answered.
    #[must_use]
    pub fn build(supply: &luma_render::catalog::VenueSockets) -> Self {
        use luma_scene::sockets::SocketKind;
        use luma_scene::venue::{Node, NodeKind, NodeSockets as _, Params};

        let pieces = luma_scene::catalog::pieces()
            .iter()
            .map(|piece| {
                let probe = Node {
                    id: String::new(),
                    kind: NodeKind::Piece,
                    catalog_ref: Some(piece.id.to_string()),
                    label: None,
                    params: Params::default(),
                };
                CatalogPiece {
                    catalog_ref: piece.id.to_string(),
                    name: piece.display_name.to_string(),
                    group: piece.palette_group.as_str().to_string(),
                    piece_kind: piece.kind.as_str().to_string(),
                    procedural: piece.geometry.is_procedural(),
                    sockets: supply
                        .sockets(&probe)
                        .iter()
                        .map(|socket| CatalogSocket {
                            name: socket.name.clone(),
                            socket_type: socket.socket_type.as_str().to_string(),
                            joint: socket.socket_type.kind().as_str().to_string(),
                            polarity: socket.socket_type.polarity().as_str().to_string(),
                            feature: socket.socket_type.polarity().can_host()
                                && matches!(
                                    socket.socket_type.kind(),
                                    SocketKind::Surface | SocketKind::Edge
                                ),
                        })
                        .collect(),
                }
            })
            .collect();
        Self {
            kinds: NodeKind::ALL
                .iter()
                .filter(|kind| **kind != NodeKind::Venue)
                .map(|kind| kind.as_str().to_string())
                .collect(),
            root_sockets: vec![
                luma_scene::venue::FLOOR_SOCKET.to_string(),
                luma_scene::venue::RIG_SOCKET.to_string(),
            ],
            length_step_m: crate::services::stage_ops::LENGTH_STEP_M,
            pieces,
        }
    }
}

// ---------------------------------------------------------------------------
// The venue as an authored document
// ---------------------------------------------------------------------------

/// The version this build writes. A file numbered higher is refused rather
/// than guessed at, as `graph.json` refuses one.
const VENUE_FILE_SCHEMA_VERSION: u32 = 1;

/// A ceiling on what will be parsed, so a corrupt blob is a message rather
/// than an allocation. A room of a thousand fixtures is tens of kilobytes.
const MAX_VENUE_JSON_BYTES: usize = 8 * 1024 * 1024;

/// The canonical file, version 1.
///
/// `venueId` is hoisted off the rows: every row of one venue's graph carries
/// the same owner, and a document should state its address once rather than
/// once per node. Round-tripping restores it to every row.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VenueFileV1 {
    schema_version: u32,
    venue_id: String,
    nodes: Vec<FileNode>,
    edges: Vec<VenueEdge>,
    params: BTreeMap<String, BTreeMap<String, f64>>,
    constraints: Vec<VenueConstraint>,
}

/// A node row without the owner column.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileNode {
    id: String,
    kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
}

impl VenueGraphRows {
    /// The rows as the exact bytes one revision of this venue is.
    ///
    /// Deterministic in the two ways a revision store needs: the same rows in
    /// any insertion order produce the same bytes, and the bytes are stable
    /// across builds. Rows are sorted by their identity, object keys are
    /// sorted recursively, and numbers are spelled by
    /// `crate::canonical_json` — the one encoding this codebase hashes,
    /// merges and diffs by, so the same store machinery that carries a
    /// `graph.json` carries a venue.
    ///
    /// A non-finite `roll` or param is not a value the graph can hold; were
    /// one to arrive it would encode as `null` and
    /// [`Self::from_canonical_json`] would refuse the file, which is louder
    /// than rounding it away here.
    #[must_use]
    pub fn to_canonical_json(&self) -> Vec<u8> {
        let value = serde_json::to_value(VenueFileV1::from_rows(self))
            .expect("the venue file's maps are keyed by strings");
        let mut json = crate::canonical_json::to_string(&value);
        json.push('\n');
        json.into_bytes()
    }

    /// The rows a canonical file holds.
    ///
    /// # Errors
    /// Fails if the bytes are not JSON, are longer than the parser accepts,
    /// carry a schema version this build does not know, or do not match the
    /// file's shape.
    pub fn from_canonical_json(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_VENUE_JSON_BYTES {
            return Err(format!("venue.json exceeds {MAX_VENUE_JSON_BYTES} bytes"));
        }
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| format!("venue.json: {e}"))?;
        // The version is read before the shape, so a file from a later build
        // says so rather than reporting whichever field moved first.
        let version = value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "venue.json.schemaVersion: missing or not a number".to_string())?;
        if version != u64::from(VENUE_FILE_SCHEMA_VERSION) {
            return Err(format!(
                "venue.json.schemaVersion: unsupported version {version}; this build writes {VENUE_FILE_SCHEMA_VERSION}"
            ));
        }
        let file: VenueFileV1 =
            serde_json::from_value(value).map_err(|e| format!("venue.json: {e}"))?;
        Ok(file.into_rows())
    }
}

impl VenueFileV1 {
    fn from_rows(rows: &VenueGraphRows) -> Self {
        let mut nodes: Vec<&VenueNode> = rows.nodes.iter().collect();
        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        let venue_id = nodes
            .first()
            .map(|n| n.venue_id.clone())
            .unwrap_or_default();

        let mut edges = rows.edges.clone();
        edges.sort_by(|left, right| edge_key(left).cmp(&edge_key(right)));
        let mut constraints = rows.constraints.clone();
        constraints.sort_by(|left, right| constraint_key(left).cmp(&constraint_key(right)));

        Self {
            schema_version: VENUE_FILE_SCHEMA_VERSION,
            venue_id,
            nodes: nodes
                .into_iter()
                .map(|node| FileNode {
                    id: node.id.clone(),
                    kind: node.kind.clone(),
                    catalog_ref: node.catalog_ref.clone(),
                    label: node.label.clone(),
                })
                .collect(),
            edges,
            params: rows.params.clone(),
            constraints,
        }
    }

    fn into_rows(self) -> VenueGraphRows {
        VenueGraphRows {
            nodes: self
                .nodes
                .into_iter()
                .map(|node| VenueNode {
                    id: node.id,
                    venue_id: self.venue_id.clone(),
                    kind: node.kind,
                    catalog_ref: node.catalog_ref,
                    label: node.label,
                })
                .collect(),
            edges: self.edges,
            params: self.params,
            constraints: self.constraints,
        }
    }
}

/// An edge's identity: one placement per child, spelled in full so the order
/// is total even if a venue ever holds two.
fn edge_key(edge: &VenueEdge) -> (&str, &str, &str, &str) {
    (
        &edge.child_id,
        &edge.parent_id,
        &edge.my_socket,
        &edge.their_socket,
    )
}

/// A constraint's identity — the whole row, which is what makes two of them
/// the same check.
fn constraint_key(constraint: &VenueConstraint) -> (&str, &str, &str, &str) {
    (
        &constraint.node_id,
        &constraint.my_socket,
        &constraint.target_node,
        &constraint.target_socket,
    )
}

// ---------------------------------------------------------------------------
// The semantic diff between two revisions
// ---------------------------------------------------------------------------

/// A node as a change names it: the id it is matched by, and the label a
/// human reads it by.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NodeRef {
    pub id: String,
    pub label: Option<String>,
}

impl fmt::Display for NodeRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label.as_deref().unwrap_or(&self.id))
    }
}

/// Where a node hangs: whose socket it is bolted to, by which of its own, and
/// how far it is turned about the joint.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NodePlacement {
    pub parent: String,
    pub my_socket: String,
    pub their_socket: String,
    pub roll: f64,
}

impl fmt::Display for NodePlacement {
    /// The socket a rigger would name it by — `tower_a.top`. `my_socket` is
    /// the node's own end and reads as noise in a summary; a change to it
    /// still shows, because a placement is compared whole.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.parent, self.their_socket)
    }
}

impl From<&VenueEdge> for NodePlacement {
    fn from(edge: &VenueEdge) -> Self {
        Self {
            parent: edge.parent_id.clone(),
            my_socket: edge.my_socket.clone(),
            their_socket: edge.their_socket.clone(),
            roll: edge.roll,
        }
    }
}

/// One difference between two revisions of a venue, in the vocabulary of the
/// room rather than of the rows.
///
/// A node that changed its `kind` or its `catalogRef` is not the same object
/// with a new attribute — a truss did not become a fixture — so it reads as a
/// [`Self::Removed`] and an [`Self::Added`] rather than as a mutation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "change",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VenueChange {
    Added {
        node: NodeRef,
        kind: String,
        catalog_ref: Option<String>,
        placement: Option<NodePlacement>,
        params: BTreeMap<String, f64>,
    },
    Removed {
        node: NodeRef,
        kind: String,
        catalog_ref: Option<String>,
        placement: Option<NodePlacement>,
        params: BTreeMap<String, f64>,
    },
    Reparented {
        node: NodeRef,
        from: Option<NodePlacement>,
        to: Option<NodePlacement>,
    },
    ParamChanged {
        node: NodeRef,
        key: String,
        from: Option<f64>,
        to: Option<f64>,
    },
    Relabelled {
        node: NodeRef,
        from: Option<String>,
        to: Option<String>,
    },
    ConstraintAdded {
        node: NodeRef,
        my_socket: String,
        target_node: String,
        target_socket: String,
    },
    ConstraintRemoved {
        node: NodeRef,
        my_socket: String,
        target_node: String,
        target_socket: String,
    },
}

/// What changed between two revisions of one venue, matching nodes by id.
///
/// Ordered so it reads as a report and not as a walk: what arrived, what
/// left, what moved, then the far ends. Within each section the order is by
/// node id, which is also what lets [`summarize`] collapse a rack of
/// identical fixtures into one line.
#[must_use]
pub fn diff(before: &VenueGraphRows, after: &VenueGraphRows) -> Vec<VenueChange> {
    let index = |rows: &VenueGraphRows| -> BTreeMap<String, NodeSnapshot> {
        let placements: BTreeMap<&str, &VenueEdge> = rows
            .edges
            .iter()
            .map(|edge| (edge.child_id.as_str(), edge))
            .collect();
        rows.nodes
            .iter()
            .map(|node| {
                (
                    node.id.clone(),
                    NodeSnapshot {
                        node: node.clone(),
                        placement: placements.get(node.id.as_str()).map(|e| (*e).into()),
                        params: rows.params.get(&node.id).cloned().unwrap_or_default(),
                    },
                )
            })
            .collect()
    };
    let (before_nodes, after_nodes) = (index(before), index(after));

    // A node whose kind or catalog entry changed is two events, not one, so
    // it is classified before anything is emitted.
    let same_thing = |id: &String| match (before_nodes.get(id), after_nodes.get(id)) {
        (Some(old), Some(new)) => {
            old.node.kind == new.node.kind && old.node.catalog_ref == new.node.catalog_ref
        }
        _ => false,
    };

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();

    for (id, snapshot) in &after_nodes {
        if !before_nodes.contains_key(id) || !same_thing(id) {
            added.push(snapshot.as_change(true));
        }
    }
    for (id, snapshot) in &before_nodes {
        if !after_nodes.contains_key(id) || !same_thing(id) {
            removed.push(snapshot.as_change(false));
        }
    }

    for (id, new) in &after_nodes {
        if !same_thing(id) {
            continue;
        }
        let old = &before_nodes[id];
        let node = new.node_ref();
        if old.node.label != new.node.label {
            modified.push(VenueChange::Relabelled {
                node: NodeRef {
                    id: id.clone(),
                    label: old.node.label.clone(),
                },
                from: old.node.label.clone(),
                to: new.node.label.clone(),
            });
        }
        if old.placement != new.placement {
            modified.push(VenueChange::Reparented {
                node: node.clone(),
                from: old.placement.clone(),
                to: new.placement.clone(),
            });
        }
        let keys: BTreeSet<&String> = old.params.keys().chain(new.params.keys()).collect();
        for key in keys {
            let (from, to) = (old.params.get(key).copied(), new.params.get(key).copied());
            if from != to {
                modified.push(VenueChange::ParamChanged {
                    node: node.clone(),
                    key: key.clone(),
                    from,
                    to,
                });
            }
        }
    }

    // A constraint has no identity apart from the four names that make it, so
    // an edited check is a removal and an addition — which is also how it
    // reads: the far end you were holding to is not the one you are now.
    let label_of = |id: &str| -> Option<String> {
        after_nodes
            .get(id)
            .or_else(|| before_nodes.get(id))
            .and_then(|s| s.node.label.clone())
    };
    let keys = |rows: &VenueGraphRows| -> BTreeSet<(String, String, String, String)> {
        rows.constraints
            .iter()
            .map(|c| {
                (
                    c.node_id.clone(),
                    c.my_socket.clone(),
                    c.target_node.clone(),
                    c.target_socket.clone(),
                )
            })
            .collect()
    };
    let (before_checks, after_checks) = (keys(before), keys(after));
    let check =
        |(node_id, my_socket, target_node, target_socket): &(String, String, String, String)| {
            (
                NodeRef {
                    id: node_id.clone(),
                    label: label_of(node_id),
                },
                my_socket.clone(),
                target_node.clone(),
                target_socket.clone(),
            )
        };

    let mut changes = added;
    changes.extend(removed);
    changes.extend(modified);
    for key in after_checks.difference(&before_checks) {
        let (node, my_socket, target_node, target_socket) = check(key);
        changes.push(VenueChange::ConstraintAdded {
            node,
            my_socket,
            target_node,
            target_socket,
        });
    }
    for key in before_checks.difference(&after_checks) {
        let (node, my_socket, target_node, target_socket) = check(key);
        changes.push(VenueChange::ConstraintRemoved {
            node,
            my_socket,
            target_node,
            target_socket,
        });
    }
    changes
}

/// One node with everything a change has to say about it, gathered from the
/// three tables that hold it.
struct NodeSnapshot {
    node: VenueNode,
    placement: Option<NodePlacement>,
    params: BTreeMap<String, f64>,
}

impl NodeSnapshot {
    fn node_ref(&self) -> NodeRef {
        NodeRef {
            id: self.node.id.clone(),
            label: self.node.label.clone(),
        }
    }

    fn as_change(&self, added: bool) -> VenueChange {
        let (node, kind, catalog_ref, placement, params) = (
            self.node_ref(),
            self.node.kind.clone(),
            self.node.catalog_ref.clone(),
            self.placement.clone(),
            self.params.clone(),
        );
        if added {
            VenueChange::Added {
                node,
                kind,
                catalog_ref,
                placement,
                params,
            }
        } else {
            VenueChange::Removed {
                node,
                kind,
                catalog_ref,
                placement,
                params,
            }
        }
    }
}

/// A number as this codebase spells numbers — `6`, not `6.0`, and `1.25` at
/// full precision. The same spelling the canonical file uses, so a summary
/// and a diff of the bytes never disagree about what a value is.
fn number(value: f64) -> String {
    crate::canonical_json::to_string(&serde_json::json!(value))
}

/// The size a piece reads by, when it has one: a truss and an array are the
/// length of their span, in metres.
fn span_of(params: &BTreeMap<String, f64>) -> Option<String> {
    params
        .get("span")
        .map(|span| format!("({} m)", number(*span)))
}

impl fmt::Display for VenueChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added {
                node,
                placement,
                params,
                ..
            }
            | Self::Removed {
                node,
                placement,
                params,
                ..
            } => {
                let sign = if matches!(self, Self::Added { .. }) {
                    '+'
                } else {
                    '-'
                };
                write!(formatter, "{sign} {node}")?;
                if let Some(span) = span_of(params) {
                    write!(formatter, " {span}")?;
                }
                match placement {
                    Some(placement) => write!(formatter, " on {placement}"),
                    None => formatter.write_str(" unplaced"),
                }
            }
            Self::Reparented { node, from, to } => {
                let side = |p: &Option<NodePlacement>| {
                    p.as_ref()
                        .map_or_else(|| "unplaced".to_string(), NodePlacement::to_string)
                };
                write!(formatter, "~ {node}: {} → {}", side(from), side(to))
            }
            Self::ParamChanged {
                node,
                key,
                from,
                to,
            } => {
                let side = |v: &Option<f64>| v.map_or_else(|| "unset".to_string(), number);
                write!(formatter, "~ {node}: {key} {} → {}", side(from), side(to))
            }
            Self::Relabelled { node, from, to } => {
                let side = |s: &Option<String>| {
                    s.as_ref()
                        .map_or_else(|| "unnamed".to_string(), |s| format!("\"{s}\""))
                };
                write!(formatter, "~ {node}: label {} → {}", side(from), side(to))
            }
            Self::ConstraintAdded {
                node,
                my_socket,
                target_node,
                target_socket,
            } => write!(
                formatter,
                "+ check {node}.{my_socket} → {target_node}.{target_socket}"
            ),
            Self::ConstraintRemoved {
                node,
                my_socket,
                target_node,
                target_socket,
            } => write!(
                formatter,
                "- check {node}.{my_socket} → {target_node}.{target_socket}"
            ),
        }
    }
}

/// The changes as a rigger would read them out, one line each, with runs of
/// the same kind collapsed.
///
/// Collapsing is what makes a diff of a real rig readable: patching a bar of
/// eight movers is eight [`VenueChange::Added`]s and one fact. A run is
/// collapsed only when every member is the same `kind`, and its names print
/// as a range when they are one prefix and consecutive numbers.
#[must_use]
pub fn summarize(changes: &[VenueChange]) -> String {
    let mut lines = Vec::new();
    let mut rest = changes;
    while let Some((first, _)) = rest.split_first() {
        let run: usize = rest
            .iter()
            .take_while(|change| collapses_with(first, change))
            .count();
        if run > 1 {
            lines.push(collapsed(&rest[..run]));
            rest = &rest[run..];
        } else {
            lines.push(first.to_string());
            rest = &rest[1..];
        }
    }
    lines.join("\n")
}

/// Two changes collapse when they are the same event about the same kind of
/// thing. Only arrivals and departures collapse: a run of moves or of
/// renames is a list of different facts, not one repeated.
fn collapses_with(first: &VenueChange, other: &VenueChange) -> bool {
    match (first, other) {
        (VenueChange::Added { kind: left, .. }, VenueChange::Added { kind: right, .. })
        | (VenueChange::Removed { kind: left, .. }, VenueChange::Removed { kind: right, .. }) => {
            left == right
        }
        _ => false,
    }
}

fn collapsed(run: &[VenueChange]) -> String {
    let (sign, kind) = match &run[0] {
        VenueChange::Added { kind, .. } => ('+', kind),
        VenueChange::Removed { kind, .. } => ('-', kind),
        _ => unreachable!("only arrivals and departures collapse"),
    };
    let names: Vec<String> = run
        .iter()
        .map(|change| match change {
            VenueChange::Added { node, .. } | VenueChange::Removed { node, .. } => node.to_string(),
            _ => unreachable!("only arrivals and departures collapse"),
        })
        .collect();
    format!("{sign} {} {kind}s ({})", run.len(), name_range(&names))
}

/// A run of names as one phrase: `Rogue R2 Spot 1–4` when they are one prefix
/// and consecutive numbers, and a list otherwise.
fn name_range(names: &[String]) -> String {
    let split = |name: &str| -> Option<(String, u64)> {
        let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        (digits > 0).then(|| {
            let (prefix, number) = name.split_at(name.len() - digits);
            (prefix.to_string(), number.parse().unwrap_or(u64::MAX))
        })
    };
    let parts: Option<Vec<(String, u64)>> = names.iter().map(|name| split(name)).collect();
    if let Some(parts) = parts {
        let (prefix, first) = parts[0].clone();
        let consecutive = parts.iter().enumerate().all(|(index, (p, n))| {
            *p == prefix && u64::try_from(index).is_ok_and(|index| *n == first + index)
        });
        if consecutive {
            let last = first + parts.len() as u64 - 1;
            return format!("{prefix}{first}–{last}");
        }
    }
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use luma_scene::sockets::{ResolvedSocket, SocketType};
    use luma_scene::venue::{resolve, Node, NodeKind, NodeSockets, FLOOR_SOCKET};

    /// One mount on the underside of everything, which is all a placement
    /// needs: this is about which rows become nodes, not about geometry.
    struct Mount;

    impl NodeSockets for Mount {
        fn sockets(&self, node: &Node) -> Vec<ResolvedSocket> {
            if node.kind == NodeKind::Venue {
                return Vec::new();
            }
            vec![
                ResolvedSocket::from_frame(
                    "bottom",
                    SocketType::BottomMount,
                    glam::DVec3::new(0.0, -0.5, 0.0),
                    glam::DVec3::NEG_Y,
                    glam::DVec3::X,
                ),
                ResolvedSocket::from_frame(
                    "top",
                    SocketType::FloorTop,
                    glam::DVec3::new(0.0, 0.5, 0.0),
                    glam::DVec3::Y,
                    glam::DVec3::X,
                ),
            ]
        }
    }

    fn row(id: &str, kind: &str) -> VenueNode {
        VenueNode {
            id: id.to_string(),
            venue_id: "v".into(),
            kind: kind.to_string(),
            catalog_ref: Some("deck".into()),
            label: None,
        }
    }

    fn edge(child: &str, parent: &str, their_socket: &str) -> VenueEdge {
        VenueEdge {
            child_id: child.to_string(),
            parent_id: parent.to_string(),
            my_socket: "bottom".into(),
            their_socket: their_socket.to_string(),
            roll: 0.0,
        }
    }

    /// Resolving a venue never loses a row in silence. A row outside the
    /// `kind` alphabet becomes no node — there is no variant for it to be —
    /// and is reported as itself, along with whatever was standing on it.
    #[test]
    fn a_row_with_an_unknown_kind_is_warned_about_not_swallowed() {
        let rows = VenueGraphRows {
            nodes: vec![
                row("venue", "venue"),
                row("blob1", "blob"),
                row("deck1", "stage"),
            ],
            edges: vec![
                edge("blob1", "venue", FLOOR_SOCKET),
                edge("deck1", "blob1", "top"),
            ],
            params: BTreeMap::new(),
            constraints: Vec::new(),
        };

        let graph = rows.to_graph().expect("the venue has a root");
        let solved = ResolvedVenue::from(&resolve(&graph, &Mount));

        assert_eq!(
            solved.warnings,
            ["blob1: `blob` is not a node kind"],
            "the warning names the row and the kind it claimed"
        );
        assert!(
            !solved.nodes.iter().any(|n| n.id == "blob1"),
            "an unknown kind is no node"
        );
        assert_eq!(
            solved
                .unplaced
                .iter()
                .map(|u| u.node_id.as_str())
                .collect::<Vec<_>>(),
            ["deck1"],
            "what stood on it is reported, not lost"
        );
    }

    fn labelled(id: &str, kind: &str, label: &str) -> VenueNode {
        VenueNode {
            label: Some(label.into()),
            ..row(id, kind)
        }
    }

    fn rows(nodes: Vec<VenueNode>, edges: Vec<VenueEdge>) -> VenueGraphRows {
        VenueGraphRows {
            nodes,
            edges,
            params: BTreeMap::new(),
            constraints: Vec::new(),
        }
    }

    /// A room with something of every table in it.
    fn sample() -> VenueGraphRows {
        VenueGraphRows {
            nodes: vec![
                row("venue", "venue"),
                row("tower_a", "tower"),
                row("run_3", "run"),
            ],
            edges: vec![
                edge("tower_a", "venue", "floor"),
                edge("run_3", "tower_a", "top"),
            ],
            params: [
                ("tower_a".to_string(), [("trim".to_string(), 6.0)].into()),
                (
                    "run_3".to_string(),
                    [("span".to_string(), 6.0), ("u".to_string(), 1.25)].into(),
                ),
            ]
            .into(),
            constraints: vec![VenueConstraint {
                node_id: "run_3".into(),
                my_socket: "end_b".into(),
                target_node: "tower_a".into(),
                target_socket: "top".into(),
            }],
        }
    }

    /// The codec is a codec: nothing the rows hold is spent on the way out and
    /// back, including the owner the file states once instead of per row.
    #[test]
    fn canonical_json_round_trips_the_rows() {
        let bytes = sample().to_canonical_json();
        let back = VenueGraphRows::from_canonical_json(&bytes).expect("the file parses");

        assert_eq!(back.to_canonical_json(), bytes, "the codec is idempotent");
        assert!(
            back.nodes.iter().all(|n| n.venue_id == "v"),
            "the hoisted owner is restored to every row"
        );
        assert_eq!(back.params, sample().params);
        assert_eq!(back.constraints.len(), 1);
        assert_eq!(back.edges.len(), 2);
        assert!(
            bytes.ends_with(b"\n"),
            "a canonical file ends in a newline, as `graph.json` does"
        );
    }

    /// The bytes are a function of the venue, not of the order SQLite handed
    /// the rows over in — which is what lets two writes of the same room hash
    /// to one revision.
    #[test]
    fn canonical_bytes_ignore_row_order() {
        let mut shuffled = sample();
        shuffled.nodes.reverse();
        shuffled.edges.reverse();
        shuffled.constraints.push(VenueConstraint {
            node_id: "tower_a".into(),
            my_socket: "top".into(),
            target_node: "run_3".into(),
            target_socket: "end_a".into(),
        });
        shuffled.constraints.reverse();

        let mut ordered = sample();
        ordered.constraints.insert(
            0,
            VenueConstraint {
                node_id: "tower_a".into(),
                my_socket: "top".into(),
                target_node: "run_3".into(),
                target_socket: "end_a".into(),
            },
        );

        assert_eq!(shuffled.to_canonical_json(), ordered.to_canonical_json());
    }

    /// A file from a later build is refused by version rather than by
    /// whichever field happened to move, so the message names the cause.
    #[test]
    fn a_later_schema_version_is_refused_by_version() {
        let mut file: serde_json::Value =
            serde_json::from_slice(&sample().to_canonical_json()).unwrap();
        file["schemaVersion"] = serde_json::Value::from(99);
        let error = VenueGraphRows::from_canonical_json(file.to_string().as_bytes())
            .expect_err("a later version is refused");
        assert!(error.contains("unsupported version 99"), "{error}");
    }

    #[test]
    fn an_arrival_names_its_kind_its_size_and_the_socket_it_hangs_on() {
        let before = rows(vec![row("venue", "venue"), row("tower_a", "tower")], vec![]);
        let mut after = before.clone();
        after.nodes.push(row("run_3", "run"));
        after.edges.push(edge("run_3", "tower_a", "top"));
        after
            .params
            .insert("run_3".into(), [("span".to_string(), 6.0)].into());

        let changes = diff(&before, &after);
        assert!(
            matches!(&changes[..], [VenueChange::Added { node, kind, .. }] if node.id == "run_3" && kind == "run")
        );
        assert_eq!(summarize(&changes), "+ run_3 (6 m) on tower_a.top");
    }

    #[test]
    fn a_departure_is_the_arrival_read_backwards() {
        let before = rows(
            vec![row("venue", "venue"), row("tower_a", "tower")],
            vec![edge("tower_a", "venue", "floor")],
        );
        let after = rows(vec![row("venue", "venue")], vec![]);

        let changes = diff(&before, &after);
        assert!(
            matches!(&changes[..], [VenueChange::Removed { node, .. }] if node.id == "tower_a")
        );
        assert_eq!(summarize(&changes), "- tower_a on venue.floor");
    }

    /// A truss did not become a fixture. A node that changed what it *is* is
    /// two events, so nothing downstream has to reconcile a kind change.
    #[test]
    fn a_node_that_changed_kind_left_and_a_new_one_arrived() {
        let before = rows(vec![row("venue", "venue"), row("x", "tower")], vec![]);
        let after = rows(vec![row("venue", "venue"), row("x", "run")], vec![]);

        assert!(matches!(
            &diff(&before, &after)[..],
            [VenueChange::Added { kind: added, .. }, VenueChange::Removed { kind: gone, .. }]
                if added == "run" && gone == "tower"
        ));
    }

    #[test]
    fn reparenting_reports_the_socket_it_left_and_the_one_it_reached() {
        let before = rows(
            vec![row("venue", "venue"), row("tower_a", "tower")],
            vec![edge("tower_a", "venue", "floor")],
        );
        let mut after = before.clone();
        after.edges[0].their_socket = "rig".into();

        let changes = diff(&before, &after);
        assert!(matches!(
            &changes[..],
            [VenueChange::Reparented { from: Some(from), to: Some(to), .. }]
                if from.their_socket == "floor" && to.their_socket == "rig"
        ));
        assert_eq!(summarize(&changes), "~ tower_a: venue.floor → venue.rig");
    }

    #[test]
    fn a_changed_param_names_the_key_and_both_values() {
        let before = VenueGraphRows {
            params: [("tower_a".to_string(), [("trim".to_string(), 6.0)].into())].into(),
            ..rows(vec![row("venue", "venue"), row("tower_a", "tower")], vec![])
        };
        let mut after = before.clone();
        after
            .params
            .get_mut("tower_a")
            .unwrap()
            .insert("trim".into(), 7.0);

        let changes = diff(&before, &after);
        assert!(matches!(
            &changes[..],
            [VenueChange::ParamChanged { key, from: Some(from), to: Some(to), .. }]
                if key == "trim" && *from == 6.0 && *to == 7.0
        ));
        assert_eq!(summarize(&changes), "~ tower_a: trim 6 → 7");
    }

    /// The subject of a rename is the name it *had*: a reader who knows the
    /// room by the old name has to be able to find the line.
    #[test]
    fn relabelling_reads_from_the_old_name() {
        let before = rows(
            vec![
                row("venue", "venue"),
                labelled("tower_a", "tower", "SL tower"),
            ],
            vec![],
        );
        let after = rows(
            vec![
                row("venue", "venue"),
                labelled("tower_a", "tower", "SR tower"),
            ],
            vec![],
        );

        let changes = diff(&before, &after);
        assert!(matches!(
            &changes[..],
            [VenueChange::Relabelled { node, .. }] if node.label.as_deref() == Some("SL tower")
        ));
        assert_eq!(
            summarize(&changes),
            "~ SL tower: label \"SL tower\" → \"SR tower\""
        );
    }

    /// A check has no identity apart from its four names, so re-aiming one is
    /// a removal and an addition rather than a mutation nobody can name.
    #[test]
    fn a_re_aimed_check_is_removed_and_added_whole() {
        let before = sample();
        let mut after = sample();
        after.constraints[0].target_socket = "bottom".into();

        let changes = diff(&before, &after);
        assert_eq!(
            summarize(&changes),
            "+ check run_3.end_b → tower_a.bottom\n- check run_3.end_b → tower_a.top"
        );
    }

    /// Patching a bar of movers is one fact, not eight — and the names read as
    /// the range the rigger labelled them with.
    #[test]
    fn a_run_of_same_kind_arrivals_collapses_to_one_line() {
        let before = rows(vec![row("venue", "venue")], vec![]);
        let mut after = before.clone();
        for index in 1..=4 {
            after.nodes.push(labelled(
                &format!("spot_{index}"),
                "fixture",
                &format!("Rogue R2 Spot {index}"),
            ));
        }
        let mut gone = before.clone();
        gone.nodes.push(row("odd", "fixture"));
        gone.nodes.push(row("even", "fixture"));

        assert_eq!(
            summarize(&diff(&before, &after)),
            "+ 4 fixtures (Rogue R2 Spot 1–4)"
        );
        assert_eq!(
            summarize(&diff(&gone, &before)),
            "- 2 fixtures (even, odd)",
            "names that are not a range are listed"
        );
    }
}
