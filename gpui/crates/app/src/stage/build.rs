//! [`Build`], the builder's whole state, and the read-only questions it answers
//! about the room: labels, sockets, freedoms, the selection, the undo stacks.

use std::collections::HashMap;

use luma_lib::models::venue_graph::ResolvedVenue;
use luma_scene::catalog::VenueSockets;
use luma_scene::coords;
use luma_scene::venue::{NodeKind, NodeSockets as _, VenueGraph};

use crate::library::Rig;

use super::hand::{self, Hand, Holding, Room};
use super::scene::params_of;

/// The builder's whole state.
pub(crate) struct Build {
    pub(crate) venue_id: String,
    /// The graph as rows, so a socket has a name to be clicked by.
    pub(super) graph: VenueGraph,
    /// The graph solved, so a socket has a place to be clicked at.
    pub(super) solved: ResolvedVenue,
    /// Every node's frame and sockets, in the shape the snap search walks.
    pub(super) room: Room,
    /// What the cursor is holding. The one state machine.
    pub(crate) hand: Hand,
    /// The catalog, resolved once against the shipped meshes.
    pub(super) sockets: VenueSockets,
    /// The selected node — a venue-graph id, not a render object.
    pub(crate) selected: Option<String>,
    /// The distribution the selection belongs to: every fixture of the same
    /// model chained on the same host face, ordered along it. Filled by the
    /// click that selected a member, emptied with the selection — the card
    /// edits the row as one thing when there is more than one of them.
    pub(crate) distribution: Vec<String>,
    /// The layout the row's controls re-run `distribute` with. Client state,
    /// not read back from the graph: the graph stores stations, not the rule
    /// that produced them.
    pub(crate) distribution_layout: luma_scene::distribute::Layout,
    /// The graph as the library last answered it — the undo stack's currency.
    /// Kept beside `graph` (its parsed form) because restoring means sending
    /// rows back, and re-deriving rows from a parsed graph would be a second
    /// spelling of the same table.
    pub(super) rows: luma_lib::models::venue_graph::VenueGraphRows,
    /// The patch as the library last answered it, beside `rows`: a verb that
    /// deletes lights is undone by bringing their patch rows back too.
    pub(super) patch: Vec<luma_lib::models::fixtures::PatchedFixture>,
    /// Snapshots a structural verb replaced, oldest first.
    pub(super) undo: Vec<Snapshot>,
    /// What undo stepped back over, for redo. Cleared by any new verb.
    pub(super) redo: Vec<Snapshot>,
    /// A verb that has not come back yet. Placement is idempotent per gesture,
    /// so a second release while one is in flight is dropped rather than
    /// queued.
    pub(crate) committing: bool,
    /// A scrubbed parameter: what the control shows while its writes catch up.
    pub(super) param_scrub: Option<ParamScrub>,
}

/// One control being scrubbed. A scrub fires on every pointer move, faster
/// than a write and re-read return. Writes go one at a time, each after the
/// last one's re-read, and only the newest value waits. Sent in parallel,
/// they landed out of order and the control jumped between old values.
pub(super) struct ParamScrub {
    pub(super) node: String,
    pub(super) key: String,
    /// The newest value asked for, shown in place of the graph's.
    pub(super) value: f64,
    /// A value arrived while a write was in flight.
    pub(super) queued: bool,
    /// The graph before the first write: the whole scrub is one undo step.
    pub(super) snapshot: Snapshot,
    /// At least one write succeeded, so there is a step to remember.
    pub(super) wrote: bool,
}

/// One undo step: the graph rows and the patch they were read beside.
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(super) rows: luma_lib::models::venue_graph::VenueGraphRows,
    pub(super) patch: Vec<luma_lib::models::fixtures::PatchedFixture>,
}

impl Build {
    /// Project a loaded rig into the builder's own view of it.
    ///
    /// Returns `None` for a venue whose rows will not form a graph — a state
    /// the resolver already refuses to produce, so it is a corrupt read rather
    /// than an empty room.
    pub(crate) fn new(venue_id: &str, rig: &Rig, sockets: VenueSockets) -> Option<Self> {
        let graph = rig.rows.to_graph()?;
        let room = Room::new(&graph, &sockets, poses(&rig.venue));
        Some(Self {
            venue_id: venue_id.to_string(),
            graph,
            solved: rig.venue.clone(),
            room,
            hand: Hand::default(),
            sockets,
            selected: None,
            distribution: Vec::new(),
            distribution_layout: luma_scene::distribute::Layout::Even,
            rows: rig.rows.clone(),
            patch: rig.fixtures.clone(),
            undo: Vec::new(),
            redo: Vec::new(),
            committing: false,
            param_scrub: None,
        })
    }

    /// Adopt a re-solved venue, keeping the hand and the selection.
    ///
    /// A verb's report carries the whole solved venue for exactly this: an
    /// edit moves everything bolted to what it touched, so the answer to
    /// "where is everything now" is a property of the graph, not of the node
    /// that changed.
    pub(crate) fn adopt(&mut self, rig: &Rig) {
        let Some(graph) = rig.rows.to_graph() else {
            return;
        };
        self.room = Room::new(&graph, &self.sockets, poses(&rig.venue));
        self.graph = graph;
        self.solved = rig.venue.clone();
        self.rows = rig.rows.clone();
        self.patch = rig.fixtures.clone();
        if self
            .selected
            .as_ref()
            .is_some_and(|id| self.graph.node(id).is_none())
        {
            self.selected = None;
        }
        // The row being fitted is measured against the *room* — how long the
        // host face is — so a room that changed under it is a preview that is
        // now about a face that no longer exists at that length. Re-solved
        // here rather than by whoever adopted, because "extend the host, then
        // look again" is one gesture and the caller that had to remember the
        // second half would be the caller that forgot it: the extend the
        // popover offers did exactly that, and the refusal never went away.
        self.repreview();
    }

    /// Record what the fixture library said about the thing in the hand.
    ///
    /// Reaches into whichever state is holding it, because the definition may
    /// land after the operator has already carried the fixture onto a face —
    /// the read is async and the hand does not wait for it.
    pub(crate) fn set_fixture_facts(
        &mut self,
        mode: Option<String>,
        width: f64,
        body: Option<[f64; 3]>,
    ) {
        let what = match &mut self.hand {
            Hand::Placing(held) => &mut held.what,
            Hand::Configuring(row) => &mut row.what,
            Hand::Idle | Hand::Choosing(_) | Hand::Extending(_) => return,
        };
        if let Holding::Fixture {
            mode: slot,
            width_m,
            dims,
            ..
        } = what
        {
            *slot = mode;
            *width_m = width;
            *dims = body;
        }
    }

    /// Re-solve the row being configured, in place.
    ///
    /// The preview and the commit share both halves of the arithmetic —
    /// [`luma_scene::face::host_face`] for how long the face is, and
    /// [`luma_scene::distribute::offsets`] for where the bodies land — so a
    /// row that previews as fitting cannot come back refused, and the
    /// "Extend to X m" the popover offers is the number the backend would
    /// have quoted. Called after every edit to a count or a layout; it is
    /// pure arithmetic over data already in memory, which is what makes
    /// re-solving on every keystroke the cheap option rather than the
    /// careful one.
    pub(crate) fn repreview(&mut self) {
        let Hand::Configuring(row) = &self.hand else {
            return;
        };
        let Some((face, pose)) = self
            .graph
            .node(&row.host_node)
            .and_then(|node| luma_scene::face::host_face(&self.sockets, node, &row.host_socket))
            .zip(self.room.pose(&row.host_node))
        else {
            return;
        };
        let Holding::Fixture { width_m, .. } = row.what else {
            return;
        };
        let solved = luma_scene::distribute::offsets(face.feature, row.layout, row.count, width_m)
            .map(|stations| {
                stations
                    .into_iter()
                    .map(|u| station_pose(&face.socket, pose, u))
                    .collect()
            });
        if let Hand::Configuring(row) = &mut self.hand {
            row.preview = solved;
        }
    }

    /// The label a node answers to in the readouts.
    pub(crate) fn label_of(&self, node: &str) -> String {
        if self
            .graph
            .node(node)
            .is_some_and(|node| node.kind == NodeKind::Venue)
        {
            return "venue".to_string();
        }
        self.solved
            .nodes
            .iter()
            .find(|n| n.id == node)
            .and_then(|n| n.label.clone().or_else(|| n.catalog_ref.clone()))
            .unwrap_or_else(|| node.to_string())
    }

    /// The sockets of whatever is currently held, resolved at its own
    /// parameters.
    pub(crate) fn held_sockets(&self) -> Vec<luma_scene::sockets::ResolvedSocket> {
        let Some(held) = self.hand.held() else {
            return Vec::new();
        };
        match &held.what {
            Holding::Piece {
                catalog_ref,
                params,
                kind,
                ..
            } => self.sockets.sockets(&luma_scene::venue::Node {
                id: hand::GHOST_NODE.to_string(),
                kind: *kind,
                catalog_ref: Some(catalog_ref.clone()),
                label: None,
                params: params.clone().into_iter().collect(),
            }),
            Holding::Duplicate { root, .. } => self
                .graph
                .node(root)
                .map(|node| self.sockets.sockets(node))
                .unwrap_or_default(),
            // A light's clamp stands off its housing, so the ghost has to ask
            // the supply for it exactly as the commit will — a preview that
            // hangs by the origin previews a placement that will not happen.
            // A held light is the bundle path it was picked from.
            Holding::Fixture { path, .. } => self.sockets.sockets(&luma_scene::venue::Node {
                id: hand::GHOST_NODE.to_string(),
                kind: luma_scene::venue::NodeKind::Fixture,
                catalog_ref: Some(path.clone()),
                label: None,
                params: luma_scene::venue::Params::default(),
            }),
        }
    }

    /// The held thing's local bounds, for the collision arm of a landing.
    /// `None` — a fixture, an unfetched definition — collides with nothing.
    pub(super) fn held_bounds(&self) -> Option<luma_scene::aabb::DAabb> {
        let held = self.hand.held()?;
        match &held.what {
            Holding::Piece {
                catalog_ref,
                params,
                ..
            } => match luma_scene::catalog::piece(catalog_ref)?.geometry {
                luma_scene::catalog::Geometry::Mesh { .. }
                | luma_scene::catalog::Geometry::Assembly(_) => {
                    self.sockets.catalog().bounds(catalog_ref)
                }
                luma_scene::catalog::Geometry::Procedural(family) => {
                    Some(luma_scene::catalog::procedural_bounds(
                        luma_scene::catalog::node_params(family, &params_of(params)),
                    ))
                }
            },
            Holding::Duplicate { root, .. } => self.room.bounds_of(root),
            // A held light collides by its housing box once the definition
            // has said how big that is — which is what stops a bar being
            // pushed through the gaps in a lattice the mesh raycast sees
            // straight through. Symmetric about the clamp: close enough for a
            // housing, and it needs no axis convention.
            Holding::Fixture {
                dims: Some(dims), ..
            } => {
                let half = glam::DVec3::new(dims[0], dims[1].max(dims[2]), dims[2]) * 0.5;
                Some(luma_scene::aabb::DAabb {
                    min: -half,
                    max: half,
                })
            }
            Holding::Fixture { .. } => None,
        }
    }

    /// Every fixture in the same distribution as `node` — the fixtures of one
    /// model chained on the same host face — ordered along the face.
    ///
    /// Derived, never stored: the graph keeps stations, and "the row" is a
    /// reading of them, so there is no row id to go stale when a fixture is
    /// deleted by hand. `path_of` answers a node's definition key; it is a
    /// closure because the scene that knows it lives beside this state, not
    /// in it.
    pub(crate) fn distribution_of(
        &self,
        node: &str,
        path_of: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<String> {
        let Some(edge) = self.graph.edge(node) else {
            return Vec::new();
        };
        let Some(path) = path_of(node) else {
            return Vec::new();
        };
        let mut out: Vec<(f64, String)> = self
            .graph
            .nodes()
            .filter(|sibling| sibling.kind == luma_scene::venue::NodeKind::Fixture)
            .filter(|sibling| {
                self.graph.edge(&sibling.id).is_some_and(|joint| {
                    joint.parent == edge.parent && joint.their_socket == edge.their_socket
                })
            })
            .filter(|sibling| path_of(&sibling.id).as_deref() == Some(path.as_str()))
            .map(|sibling| (sibling.params.get("u", 0.0), sibling.id.clone()))
            .collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// The room as it stands, as an undo step.
    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            rows: self.rows.clone(),
            patch: self.patch.clone(),
        }
    }

    /// File a snapshot a verb just replaced, and drop the redo branch — a new
    /// edit after an undo is a new history.
    pub(crate) fn remember(&mut self, snapshot: Snapshot) {
        self.undo.push(snapshot);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Burn both stacks: a restore was refused, so what they hold no longer
    /// matches the venue.
    pub(crate) fn forget_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// The relation one node was placed by, in the graph's own words.
    ///
    /// The claim an `attach` makes, read back off the solved graph rather than
    /// off the gesture that asked for it: a test that asserted the readout it
    /// had just typed in would be restating a constant.
    pub(crate) fn relation_of(&self, node: &str) -> Option<String> {
        let edge = self.graph.edge(node)?;
        Some(format!(
            "Edge: {} {} on {} {}",
            self.label_of(node),
            edge.my_socket,
            self.label_of(&edge.parent),
            edge.their_socket
        ))
    }

    /// What a far-end check on this node currently says.
    pub(crate) fn constraint_of(&self, node: &str) -> Option<String> {
        let check = self
            .solved
            .constraints
            .iter()
            .find(|check| check.node_id == node)?;
        Some(format!(
            "Constraint: {} {} meets {} {} — {}",
            self.label_of(node),
            check.my_socket,
            self.label_of(&check.target_node),
            check.target_socket,
            check.status
        ))
    }

    /// Pick a placed node with a ray in the socket layer's world space.
    pub(crate) fn room_pick(&self, origin: glam::DVec3, dir: glam::DVec3) -> Option<String> {
        self.room.pick(origin, dir)
    }

    /// A placed object's geometry bounds in the camera's world space.
    pub(crate) fn room_bounds(&self, node: &str) -> Option<luma_scene::Aabb> {
        let pose = self.room.pose(node)?;
        let bounds = self.room.bounds_of(node)?;
        Some(luma_scene::Aabb::from_points(
            bounds.as_aabb().corners().map(|corner| {
                luma_scene::coords::world_from_three(
                    pose.transform_point3(corner.as_dvec3()).as_vec3(),
                )
            }),
        ))
    }

    /// Changing the selected node clears drafts and distribution edits.
    pub(crate) fn select(&mut self, node: Option<String>) {
        if self.selected == node {
            return;
        }
        self.selected = node;
        // The row follows the selection: whoever expands it fills it back in.
        self.distribution.clear();
    }

    /// The selected node as the page draws it — label, freedoms, relation.
    /// Shared by the inspector and the in-scene rotation controls.
    pub(crate) fn selected_view(&self) -> Option<SelectedView> {
        let node = self.selected.as_ref()?;

        let freedom = self.freedom_of(node);
        let family = self
            .graph
            .node(node)
            .and_then(|n| n.catalog_ref.as_deref())
            .and_then(luma_scene::catalog::piece)
            .and_then(|piece| match piece.geometry {
                luma_scene::catalog::Geometry::Procedural(family) => Some(family),
                luma_scene::catalog::Geometry::Mesh { .. }
                | luma_scene::catalog::Geometry::Assembly(_) => None,
            });
        // A scrub in flight shows the value asked for, not the graph's.
        let scrubbed = |key: &str| {
            self.param_scrub
                .as_ref()
                .filter(|scrub| &scrub.node == node && scrub.key == key)
                .map(|scrub| scrub.value)
        };
        let param = |key: &str| {
            scrubbed(key).unwrap_or_else(|| {
                self.solved
                    .nodes
                    .iter()
                    .find(|n| &n.id == node)
                    .and_then(|n| n.params.get(key).copied())
                    .unwrap_or(0.0)
            })
        };
        Some(SelectedView {
            node: node.clone(),
            label: self.label_of(node),
            freedom,
            relation: self.relation_of(node),
            constraint: self.constraint_of(node),
            trim: param("trim"),
            // `yaw` is the one freedom that is not a node param: a mate's
            // turn about the shared normal lives on the *edge*, and it is
            // radians there and degrees on this sheet. Reading it out of
            // `params` found nothing and drew every joint at zero.
            param: match freedom.param() {
                Some("yaw") => scrubbed("yaw")
                    .or_else(|| self.graph.edge(node).map(|edge| edge.roll))
                    .map_or(0.0, |roll| roll.to_degrees().rem_euclid(360.0)),
                Some(key) => param(key),
                None => 0.0,
            },
            span: {
                let stored = scrubbed("span").or_else(|| {
                    self.solved
                        .nodes
                        .iter()
                        .find(|n| &n.id == node)
                        .and_then(|n| n.params.get("span").copied())
                });
                // A straight truss placed without an explicit span is at the
                // generator's default — the length is still its to change,
                // so the row shows the default rather than nothing.
                stored.or_else(|| {
                    (family == Some(luma_scene::catalog::Family::Truss))
                        .then(|| f64::from(luma_scene::catalog::DEFAULT_TRUSS_SPAN_M))
                })
            },
            angle: (family == Some(luma_scene::catalog::Family::Hinge)).then(|| {
                scrubbed("angle").unwrap_or_else(|| {
                    self.solved
                        .nodes
                        .iter()
                        .find(|n| &n.id == node)
                        .and_then(|n| n.params.get("angle").copied())
                        .unwrap_or(f64::from(luma_scene::catalog::DEFAULT_HINGE_ANGLE_DEG))
                })
            }),
            roll_step: self
                .graph
                .edge(node)
                .and_then(|edge| self.room.socket(&edge.parent, &edge.their_socket))
                .map_or(5.0, |host| match host.roll {
                    luma_scene::sockets::RollFreedom::Steps(degrees) => degrees,
                    _ => 5.0,
                }),
            joint: self
                .graph
                .edge(node)
                .and_then(|edge| self.room.socket_world(&edge.parent, &edge.their_socket)),
        })
    }

    /// The surface directly below this mount, excluding its moving subtree.
    pub(super) fn surface_below(&self, node: &str) -> Option<(String, f64)> {
        let edge = self.graph.edge(node)?;
        let point = self.room.socket_world(node, &edge.my_socket)?;
        self.room.surface_below(point, &self.graph.subtree(node))
    }

    /// Surface placements translate in U/V and rotate only about the host
    /// normal. Only free rig pieces offer lift; mounted pieces stay seated.
    pub(crate) fn gizmo_space(&self, node: &str) -> Option<luma_scene::gizmo::GizmoSpace> {
        if self.graph.node(node)?.kind == luma_scene::venue::NodeKind::Fixture {
            return None;
        }
        let freedom = self.freedom_of(node);
        if !matches!(freedom, Freedom::Free | Freedom::Slide) {
            return None;
        }
        let edge = self.graph.edge(node)?;
        let host = self.room.socket(&edge.parent, &edge.their_socket)?;
        let parent = self.room.pose(&edge.parent)?;
        let normal =
            coords::world_from_three(parent.transform_vector3(host.normal).as_vec3()).normalize();
        let tangent = coords::world_from_three(parent.transform_vector3(host.tangent).as_vec3());
        let u = (tangent - normal * tangent.dot(normal)).normalize();
        let basis = glam::Quat::from_mat3(&glam::Mat3::from_cols(u, normal.cross(u), normal));
        let procedural = self
            .graph
            .node(node)
            .and_then(|node| node.catalog_ref.as_deref())
            .and_then(luma_scene::catalog::piece)
            .is_some_and(|piece| {
                matches!(piece.geometry, luma_scene::catalog::Geometry::Procedural(_))
            });
        Some(luma_scene::gizmo::GizmoSpace {
            basis,
            translation: [true, true, freedom == Freedom::Free && procedural],
            rotation: [
                false,
                false,
                host.roll != luma_scene::sockets::RollFreedom::Fixed,
            ],
        })
    }

    /// Whether this node's open sockets are *named* on the element layer right
    /// now — the positions a pointer, or a script, may reach a joint at.
    ///
    /// Wider than what is drawn (see [`Build::socket_marked`]): while a piece
    /// is in the air every candidate joint has a pixel, because that is how
    /// aiming works, but only the one the ghost is stuck to is painted.
    pub(crate) fn socket_shown(&self, node: &str) -> bool {
        match &self.hand {
            Hand::Placing(_) | Hand::Extending(_) => true,
            Hand::Choosing(_) | Hand::Configuring(_) => false,
            Hand::Idle => self.selected.as_deref() == Some(node),
        }
    }

    /// Whether the ghost is stuck to exactly this joint.
    pub(crate) fn latched_at(&self, node: &str, socket: &str) -> bool {
        self.hand
            .held()
            .and_then(|held| held.latched.as_ref())
            .is_some_and(|m| m.host_id.as_deref() == Some(node) && m.host_socket == socket)
    }

    /// Whether this socket is *drawn in the picture*.
    ///
    /// One mark while a piece is in the air, and it is the joint the ghost has
    /// taken hold of. The alternative — every open socket lit at once — is a
    /// field of dots the eye has to search and the pointer has to dodge, and
    /// it says nothing the ghost is not already saying. At rest the selected
    /// piece keeps its ends, because that is where a run is begun.
    pub(crate) fn socket_marked(&self, node: &str, socket: &str) -> bool {
        match &self.hand {
            Hand::Placing(_) => self.latched_at(node, socket),
            _ => self.socket_shown(node),
        }
    }

    /// The freedom a placed node actually has, which is what the inspector may
    /// offer and the gizmo may not.
    ///
    /// One reading of the joint, not a table: a socket already carries its own
    /// roll freedom, and a surface joint is the only kind whose child is free
    /// to be picked up and put down somewhere else.
    pub(crate) fn freedom_of(&self, node: &str) -> Freedom {
        // The root, or a joint whose host socket is gone: nothing to move.
        let Some(edge) = self.graph.edge(node) else {
            return Freedom::Bolted;
        };
        let Some(host) = self.room.socket(&edge.parent, &edge.their_socket) else {
            return Freedom::Bolted;
        };
        match (host.socket_type.kind(), host.roll) {
            (luma_scene::sockets::SocketKind::Surface, _) if edge.parent == self.room.root() => {
                Freedom::Free
            }
            (luma_scene::sockets::SocketKind::Surface, _) => Freedom::Slide,
            (_, luma_scene::sockets::RollFreedom::Fixed) => Freedom::Bolted,
            _ => Freedom::Roll,
        }
    }
}

/// What a placed node may be moved by.
///
/// Surface placements get a constrained UVZ gizmo. Other joints expose only
/// their roll control, or no movement at all when bolted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freedom {
    /// Seated on the venue's own floor or grid; rig pieces may also change trim.
    Free,
    /// Bolted onto a face — it slides along it, and nowhere else.
    Slide,
    /// A joint with a roll: a clamp's yaw, a hinge's angle.
    Roll,
    /// A bolt circle with no freedom at all. Dragging it drags the run.
    Bolted,
}

impl Freedom {
    /// The parameter this freedom edits, when it edits one.
    pub(crate) fn param(self) -> Option<&'static str> {
        match self {
            Freedom::Slide => Some("u"),
            Freedom::Roll => Some("yaw"),
            Freedom::Free | Freedom::Bolted => None,
        }
    }
}

/// Where one body of a row sits, `u` metres along the face from its middle.
///
/// The face's own frame, in the host's: tangent forward, normal up — which is
/// what makes "hanging under" and "standing on" the same call with a different
/// socket, exactly as [`luma_scene::face::HostFace`] describes.
fn station_pose(
    socket: &luma_scene::sockets::ResolvedSocket,
    host: glam::DMat4,
    u: f64,
) -> glam::DMat4 {
    let tangent = socket.tangent.normalize_or_zero();
    let normal = socket.normal.normalize_or_zero();
    let bitangent = normal.cross(tangent);
    let local = glam::DMat4::from_cols(
        tangent.extend(0.0),
        normal.extend(0.0),
        bitangent.extend(0.0),
        (socket.position + tangent * u).extend(1.0),
    );
    host * local
}

/// Every node's world frame, from the solved venue's stored triples.
fn poses(solved: &ResolvedVenue) -> HashMap<String, glam::DMat4> {
    solved
        .nodes
        .iter()
        .filter(|node| node.array_index.is_none())
        .map(|node| {
            (
                node.id.clone(),
                coords::three_pose_from_data_d(node.position, node.rotation),
            )
        })
        .collect()
}

/// The selected node, and the freedoms it actually has.
pub(crate) struct SelectedView {
    pub(crate) node: String,
    pub(crate) label: String,
    /// The movement admitted by the mounting joint.
    pub(crate) freedom: Freedom,
    pub(crate) relation: Option<String>,
    pub(crate) constraint: Option<String>,
    pub(crate) trim: f64,
    pub(crate) param: f64,
    /// How long the piece itself is, for the pieces that have a length.
    ///
    /// Not a freedom: a joint's freedom is how a piece may *move*, and a run's
    /// span is what it *is*. They are two rows because they answer two
    /// questions — a truss bolted at both ends has no freedom at all and is
    /// still the thing whose length the operator came here to change ("place
    /// it, then configure it inline").
    pub(crate) span: Option<f64>,
    /// A hinge's deflection, in degrees — its own "what it is" row.
    pub(crate) angle: Option<f64>,
    /// The turn this joint's roll lands on, in degrees — `Steps(90)` for a
    /// corner on a bolt circle, `Steps(5)` for a rail at its post, a fine
    /// default for a free clamp. What the rotate buttons step by and the yaw
    /// scrub snaps to, read off the joint rather than restated per control.
    pub(crate) roll_step: f64,
    /// Where the joint is, in the socket layer's world — the host socket's
    /// position. Rotation happens *here*, so this is where the rotate
    /// controls sit; drawn anywhere else they claim a pivot the solver does
    /// not use.
    pub(crate) joint: Option<glam::DVec3>,
}

/// Maximum retained structure edits for undo.
const UNDO_DEPTH: usize = 64;

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;

    use luma_scene::venue::{Edge, Node, NodeKind, Params, VenueGraph};

    use super::{Build, Room};

    /// A graph with no room around it. `copy_plan` reads the graph and nothing
    /// else — the poses are the *renderer's* half of a duplicate, and the rows
    /// it plans are decided before anything is solved.
    pub(in crate::stage) fn build_of(graph: VenueGraph) -> Build {
        let sockets = luma_scene::catalog::VenueSockets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
            std::sync::Arc::new(luma_scene::catalog::NoFixtures),
        )
        .expect("the shipped meshes");
        Build {
            venue_id: "venue".into(),
            room: Room::new(&graph, &sockets, HashMap::new()),
            rows: luma_lib::models::venue_graph::VenueGraphRows::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            graph,
            solved: luma_lib::models::venue_graph::ResolvedVenue::default(),
            hand: super::Hand::Idle,
            sockets,
            selected: None,
            distribution: Vec::new(),
            distribution_layout: luma_scene::distribute::Layout::Even,
            committing: false,
            param_scrub: None,
            patch: Vec::new(),
        }
    }

    #[test]
    fn a_truss_on_a_table_uses_the_tables_normal_and_cannot_lift_or_tilt() {
        use glam::{DMat4, Vec3};
        let mut graph = VenueGraph::new(Node {
            id: "venue".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        });
        graph.insert_placed(
            Node {
                id: "table".into(),
                kind: NodeKind::Stage,
                catalog_ref: Some("stage_lab/stage_praticavel_2x1x1.glb".into()),
                label: None,
                params: Params::default(),
            },
            edge("venue", "bottom", luma_scene::venue::FLOOR_SOCKET, 0.0),
        );
        graph.insert_placed(piece("truss", 0.0), edge("table", "end_a", "top", 0.0));
        let mut build = build_of(graph);
        let tilted = DMat4::from_rotation_z(0.4);
        build.room = Room::new(
            &build.graph,
            &build.sockets,
            HashMap::from([
                ("venue".into(), DMat4::IDENTITY),
                ("table".into(), tilted),
                ("truss".into(), DMat4::IDENTITY),
            ]),
        );
        let space = build.gizmo_space("truss").unwrap();
        assert_eq!(space.translation, [true, true, false]);
        assert_eq!(space.rotation, [false, false, true]);
        let normal = luma_scene::coords::world_from_three(
            tilted.transform_vector3(glam::DVec3::Y).as_vec3(),
        );
        assert!((space.basis * Vec3::Z).abs_diff_eq(normal, 1e-5));
        let table = build.gizmo_space("table").unwrap();
        assert_eq!(table.translation, [true, true, false]);
        assert_eq!(table.rotation, [false, false, true]);
    }

    fn params(u: f64) -> Params {
        let mut params = Params::default();
        params.set("u", u);
        params
    }

    pub(in crate::stage) fn piece(id: &str, u: f64) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Piece,
            catalog_ref: Some("truss.straight".into()),
            label: None,
            params: params(u),
        }
    }

    pub(in crate::stage) fn edge(parent: &str, mine: &str, theirs: &str, roll: f64) -> Edge {
        Edge {
            parent: parent.into(),
            my_socket: mine.into(),
            their_socket: theirs.into(),
            roll,
        }
    }
}
