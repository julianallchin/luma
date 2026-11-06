//! The venue graph across the host boundary.
//!
//! [`luma_scene::venue`] owns the model; these are its rows and its solved
//! output in the shapes `serde` can carry. They are a *projection*,
//! not a second declaration: every field here is read off a
//! [`luma_scene::venue::Node`] / [`luma_scene::venue::NodePose`], and the
//! vocabularies (`kind`, socket names, param keys) are the crate's strings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use luma_scene::venue::{
    ConstraintStatus, DanglingSocket, Edge, NodePose, NodeWarning, ResolvedVenue as Solved,
    VenueGraph, Warning,
};

/// One `venue_nodes` row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VenueNode {
    pub id: String,
    pub venue_id: String,
    /// `venue` | `stage` | `run` | `tower` | `piece` | `fixture` | `array`.
    pub kind: String,
    /// What says what shape this node is: a catalog piece id, or a fixture's
    /// bundle path. Read back off the patch for a fixture node written before
    /// that was true — see `get_graph`.
    pub catalog_ref: Option<String>,
    pub label: Option<String>,
    /// Where it hangs. `None` on the root, and only there: the schema refuses
    /// any other node without one.
    pub placement: Option<NodePlacement>,
}

/// Where a node hangs: whose socket it is bolted to, by which of its own, and
/// how far it is turned about the joint.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NodePlacement {
    pub parent: String,
    pub my_socket: String,
    pub their_socket: String,
    /// Radians about the shared normal — *yaw* on a surface placement.
    pub roll: f64,
}

impl NodePlacement {
    /// The placement as the resolver's relation.
    #[must_use]
    pub fn edge(&self) -> Edge {
        Edge {
            parent: self.parent.clone(),
            my_socket: self.my_socket.clone(),
            their_socket: self.their_socket.clone(),
            roll: self.roll,
        }
    }
}

impl From<&Edge> for NodePlacement {
    fn from(edge: &Edge) -> Self {
        Self {
            parent: edge.parent.clone(),
            my_socket: edge.my_socket.clone(),
            their_socket: edge.their_socket.clone(),
            roll: edge.roll,
        }
    }
}

/// One `venue_constraints` row: a far end, checked after the solve.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, FromRow)]
#[serde(rename_all = "camelCase")]
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
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct VenueGraphRows {
    pub nodes: Vec<VenueNode>,
    pub params: BTreeMap<String, BTreeMap<String, f64>>,
    pub constraints: Vec<VenueConstraint>,
}

impl VenueGraphRows {
    /// The rows as the model the resolver walks.
    ///
    /// `None` unless there is exactly one root and every other node has a
    /// placement: rows that break either are not a venue, and a caller that
    /// was handed them (a restore, a file) refuses them whole.
    ///
    /// A row naming an unknown `kind` becomes no node: the alphabet is closed,
    /// and inventing a kind for it would put something in the tree that no code
    /// below knows how to draw. It is reported as a
    /// [`Warning::UnknownKind`] on the solved venue rather than refused,
    /// because the rest of the room is still a room — the same reason a piece
    /// whose catalog entry is gone is drawn at its parent's origin instead of
    /// failing the load. Whatever hung off it is reported
    /// [`Warning::Unreachable`].
    #[must_use]
    pub fn to_graph(&self) -> Option<VenueGraph> {
        use luma_scene::venue::{Constraint, Node, NodeKind};

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

        let is_root = |row: &VenueNode| row.kind == NodeKind::Venue.as_str();
        if self.nodes.iter().filter(|row| is_root(row)).count() != 1
            || self
                .nodes
                .iter()
                .any(|row| is_root(row) == row.placement.is_some())
        {
            return None;
        }
        let root = self
            .nodes
            .iter()
            .find(|row| is_root(row))
            .and_then(node_of)?;

        // Rows go in with `insert_placed`, not `attach`: they were already
        // admitted when they were written, and re-checking them here would mean
        // a venue whose catalog entry has since been dropped stops loading at
        // all rather than reporting one dangling piece.
        let mut graph = VenueGraph::new(root);
        for row in &self.nodes {
            let Some(placement) = &row.placement else {
                continue;
            };
            match node_of(row) {
                Some(node) => graph.insert_placed(node, placement.edge()),
                None => graph.warn(&row.id, Warning::UnknownKind(row.kind.clone())),
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
/// Derived on every read rather than stored — see the design doc's "Live
/// graph, no baked poses, no hybrid."
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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
    /// the renderer and the agent binding cannot disagree about what is in the
    /// room.
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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

/// An open structural socket.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedVenue {
    /// Depth-first from the root, children in id order. Deterministic.
    pub nodes: Vec<ResolvedNode>,
    pub constraints: Vec<ResolvedConstraint>,
    pub dangling: Vec<ResolvedDangling>,
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
        Warning::Unreachable => "its parents never reach the room, so it has no pose".into(),
    }
}

/// What a mutating call reports back.
///
/// [`luma_scene::venue::Placement`] with the whole solved venue alongside it,
/// because every caller that changes one node then wants to redraw all of them
/// — and a second round trip to fetch that would be a second solve of the same
/// graph.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlacementReport {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub warnings: Vec<String>,
    pub dangling: Vec<ResolvedDangling>,
    pub constraints: Vec<ResolvedConstraint>,
    pub venue: ResolvedVenue,
}

impl PlacementReport {
    /// The report for one node out of a solved venue.
    #[must_use]
    pub fn of(node_id: &str, solved: &Solved) -> Self {
        let placement = solved.placement(node_id);
        let venue = ResolvedVenue::from(solved);
        Self {
            node_id: node_id.to_string(),
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPiece {
    /// The short name the authoring surface names this piece by — `truss`,
    /// `guardrail`, `deck`. The **primary** id a caller types.
    pub short: String,
    /// What `catalog_ref` holds — the storage key, and an alias a caller may
    /// still type.
    pub catalog_ref: String,
    pub name: String,
    /// The box the piece fills **as `place` lays it with no `direction=`**, in
    /// facade axes: `(u, v, z)`, metres. For a generated piece this is its
    /// default; `length=` changes the span only for the truss generator.
    ///
    /// Not the piece's own `(x, y, z)`: a mesh is modelled in whatever frame
    /// its author chose and the seat turns it, so a catalog that printed the
    /// asset's box told three agents a deck was 1 m across the stage when
    /// `place` gives it 2.
    pub size: [f64; 3],
    /// Palette section: `Stage`, `Trusses`, `Speakers`, ...
    pub group: String,
    /// Snap taxonomy: `floor`, `truss`, `speaker`, `cdj`, ...
    pub piece_kind: String,
    /// Canonical generator family (`truss`, `corner`, `hinge`), or no generator
    /// for fixed geometry. Families expose different construction parameters.
    pub generator: Option<String>,
    /// Resolved against this piece's **default** parameters. A generated piece
    /// can move its sockets as its geometry parameters change.
    pub sockets: Vec<CatalogSocket>,
}

/// One socket, in the vocabulary `attach` checks against.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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

/// The box one piece fills in facade axes, seated on the floor the way a bare
/// `place` seats it.
///
/// Solved by the authoring layer itself — see
/// [`luma_scene::build::resting_footprint`] — because the printed dimensions
/// and the placed ones have to be one computation. A piece with no resting pose
/// (a truss stands on its ends) reads its own box out along its own axes.
fn placed_size(
    supply: &luma_render::catalog::VenueSockets,
    probe: &luma_scene::venue::Node,
) -> [f64; 3] {
    use luma_scene::venue::NodeSockets as _;

    if let Some(print) = luma_scene::build::resting_footprint(supply, probe) {
        return print.size;
    }
    supply.bounds(probe).map_or([0.0; 3], |b| {
        let s = b.size();
        [s.x, s.z, s.y]
    })
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
                    short: piece.short.to_string(),
                    catalog_ref: piece.id.to_string(),
                    name: piece.display_name.to_string(),
                    size: placed_size(supply, &probe),
                    group: piece.palette_group.as_str().to_string(),
                    piece_kind: piece.kind.as_str().to_string(),
                    generator: match piece.geometry {
                        luma_scene::catalog::Geometry::Procedural(family) => {
                            Some(family.as_str().to_string())
                        }
                        luma_scene::catalog::Geometry::Mesh { .. }
                        | luma_scene::catalog::Geometry::Assembly(_) => None,
                    },
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
            length_step_m: luma_scene::build::MODULE_M,
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
    placement: Option<NodePlacement>,
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
                    placement: node.placement.clone(),
                })
                .collect(),
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
                    placement: node.placement,
                })
                .collect(),
            params: self.params,
            constraints: self.constraints,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    use luma_scene::sockets::{ResolvedSocket, SocketType};
    use luma_scene::venue::{resolve, Node, NodeKind, NodeSockets, FLOOR_SOCKET};

    #[test]
    fn catalog_reports_canonical_generator_families() {
        let supply = luma_render::catalog::VenueSockets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/meshes"),
            std::sync::Arc::new(luma_render::catalog::NoFixtures),
        )
        .unwrap();
        let catalog = StageCatalog::build(&supply);
        for (short, family) in [
            ("truss", Some("truss")),
            ("corner", Some("corner")),
            ("hinge", Some("hinge")),
            ("deck", None),
        ] {
            let piece = catalog
                .pieces
                .iter()
                .find(|piece| piece.short == short)
                .unwrap();
            assert_eq!(piece.generator.as_deref(), family);
            let json = serde_json::to_value(piece).unwrap();
            assert!(json.get("procedural").is_none());
            assert_eq!(json["generator"], serde_json::to_value(family).unwrap());
        }
    }

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

    fn root() -> VenueNode {
        VenueNode {
            id: "venue".into(),
            venue_id: "v".into(),
            kind: "venue".into(),
            catalog_ref: None,
            label: None,
            placement: None,
        }
    }

    /// A node bolted by its `bottom` to `parent.their_socket`.
    fn on(id: &str, kind: &str, parent: &str, their_socket: &str) -> VenueNode {
        VenueNode {
            id: id.to_string(),
            venue_id: "v".into(),
            kind: kind.to_string(),
            catalog_ref: Some("deck".into()),
            label: None,
            placement: Some(NodePlacement {
                parent: parent.to_string(),
                my_socket: "bottom".into(),
                their_socket: their_socket.to_string(),
                roll: 0.0,
            }),
        }
    }

    /// Resolving a venue never loses a row in silence. A row outside the
    /// `kind` alphabet becomes no node — there is no variant for it to be —
    /// and is reported as itself, along with whatever was standing on it.
    #[test]
    fn a_row_with_an_unknown_kind_is_warned_about_not_swallowed() {
        let rows = rows(vec![
            root(),
            on("blob1", "blob", "venue", FLOOR_SOCKET),
            on("deck1", "stage", "blob1", "top"),
        ]);

        let graph = rows.to_graph().expect("the venue has a root");
        let solved = ResolvedVenue::from(&resolve(&graph, &Mount));

        assert_eq!(
            solved.warnings,
            [
                "blob1: `blob` is not a node kind",
                "deck1: its parents never reach the room, so it has no pose"
            ],
            "the warnings name the row, the kind it claimed, and what stood on it"
        );
        assert!(
            !solved
                .nodes
                .iter()
                .any(|n| n.id == "blob1" || n.id == "deck1"),
            "an unknown kind is no node, and nothing stands on it"
        );
    }

    /// A node with no placement is not a venue row: the rows are refused
    /// whole rather than solved with a light missing.
    #[test]
    fn a_node_without_a_placement_is_not_a_graph() {
        let mut stray = on("light", "fixture", "venue", FLOOR_SOCKET);
        stray.placement = None;
        assert!(rows(vec![root(), stray]).to_graph().is_none());
        let mut placed_root = root();
        placed_root.placement = on("x", "stage", "venue", FLOOR_SOCKET).placement;
        assert!(rows(vec![placed_root]).to_graph().is_none());
    }

    fn rows(nodes: Vec<VenueNode>) -> VenueGraphRows {
        VenueGraphRows {
            nodes,
            params: BTreeMap::new(),
            constraints: Vec::new(),
        }
    }

    /// A room with something of every table in it.
    fn sample() -> VenueGraphRows {
        VenueGraphRows {
            nodes: vec![
                root(),
                on("tower_a", "tower", "venue", "floor"),
                on("run_3", "run", "tower_a", "top"),
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
        let by_id = |mut nodes: Vec<VenueNode>| {
            nodes.sort_by(|a, b| a.id.cmp(&b.id));
            nodes
        };
        assert_eq!(
            by_id(back.nodes.clone()),
            by_id(sample().nodes),
            "placements travel with their nodes"
        );
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
}
