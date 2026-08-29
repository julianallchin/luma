//! The venue graph across the host boundary.
//!
//! [`luma_scene::venue`] owns the model; these are its rows and its solved
//! output in the shapes `ts-rs` and `serde` can carry. They are a *projection*,
//! not a second declaration: every field here is read off a
//! [`luma_scene::venue::Node`] / [`luma_scene::venue::NodePose`], and the
//! vocabularies (`kind`, socket names, param keys) are the crate's strings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

use luma_scene::venue::{ConstraintStatus, NodePose, ResolvedVenue as Solved, VenueGraph, Warning};

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
    /// A row naming an unknown `kind` is dropped rather than guessed at: the
    /// alphabet is closed, and inventing a kind for it would put a node in the
    /// tree that no code below knows how to draw.
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
        let mut graph = VenueGraph::new(root);
        for row in &self.nodes {
            if let Some(node) = node_of(row) {
                if node.id != graph.root() {
                    graph.insert(node);
                }
            }
        }
        // Edges go in with `insert_edge`, not `attach`: these rows were already
        // admitted when they were written, and re-checking them here would mean
        // a venue whose catalog entry has since been dropped stops loading at
        // all rather than reporting one dangling piece.
        for edge in &self.edges {
            graph.insert_edge(
                &edge.child_id,
                Edge {
                    parent: edge.parent_id.clone(),
                    my_socket: edge.my_socket.clone(),
                    their_socket: edge.their_socket.clone(),
                    roll: edge.roll,
                },
            );
        }
        for c in &self.constraints {
            graph.constrain(Constraint {
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
    /// draws — [`NodePose::is_set_piece`]. Carried rather than re-derived:
    /// three consumers used to work it out from `kind`/`arrayIndex`/
    /// `catalogRef` and one of them forgot the array anchor, drawing N+1
    /// pieces for an array of N.
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
                .map(|d| ResolvedDangling {
                    node_id: d.node.clone(),
                    socket: d.socket.clone(),
                    socket_type: d.socket_type.as_str().to_string(),
                })
                .collect(),
            unplaced: solved
                .unplaced()
                .iter()
                .map(|u| ResolvedUnplaced {
                    node_id: u.node.clone(),
                    kind: u.kind.as_str().to_string(),
                    label: u.label.clone(),
                    descendants: u32::try_from(u.descendants).unwrap_or(u32::MAX),
                })
                .collect(),
            warnings: solved
                .warnings()
                .iter()
                .map(|w| format!("{}: {}", w.node, describe(&w.warning)))
                .collect(),
        }
    }
}

fn describe(warning: &Warning) -> String {
    match warning {
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
    /// Whether the node came out of the solve with a pose at all.
    pub ok: bool,
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
            ok: placement.ok,
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
