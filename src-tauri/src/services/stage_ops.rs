//! The venue-graph verbs, once.
//!
//! Every way a rig gets built — the gpui stage page through
//! [`crate::dispatch::handlers::stage`], and an agent's Python cell through
//! [`crate::agent_execution::venue_host`] — bottoms out here. The two callers
//! differ only in what they hold: the page has an [`crate::dispatch::AppServices`],
//! the cell has a pool and a fixtures root. Neither has a verb of its own, which
//! is what makes "the agent calls the same resolver as the page" a fact about
//! the code rather than a promise in a doc.
//!
//! # What is a verb and what is a composition
//!
//! `attach`, `reattach`, `place_free`, `detach`, `constrain`, `set_params` and
//! `delete_subtree` each write one relation and re-solve. [`Stage::extend`] and
//! [`Stage::duplicate`] are **compositions** of those — an extend is an attach
//! plus, when the run bridges a measured gap, the far-end check that says so;
//! a duplicate is one attach per node of a subtree, parents rewritten to the
//! copies. They live here rather than in either caller because a composition
//! written twice is two rigs that differ in the case nobody tested.
//!
//! # The two hard errors
//!
//! [`StageError::Refused`] is the design doc's short list: a socket pair the
//! catalog forbids, and an extend longer than the ray-measured gap. Everything
//! else is a warning on the [`PlacementReport`]. Refusals carry the resolver's
//! own message verbatim, because the caller's job is to show it, not to
//! rewrite it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use glam::{DMat4, DVec3};
use luma_render::catalog::VenueSockets;
use luma_render::venue_tiles::TileMap;
use luma_scene::sockets::{ResolvedSocket, SocketType};
use luma_scene::venue::{
    root_socket, Constraint, Edge, Node, NodeKind, NodeSockets as _, Params,
    ResolvedVenue as Solved, VenueGraph, FLOOR_SOCKET,
};
use sqlx::SqlitePool;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venue_graph as venue_graph_db;
use crate::models::distribute::{DistributeLayout, DistributeReport};
use crate::models::venue_graph::{
    PlacementReport, Reach, ResolvedVenue, StageCatalog, VenueGraphRows,
};
use crate::services::{distribute as distribute_service, fixture_create};
use crate::venue_graph;

// ---------------------------------------------------------------------------
// Vocabulary shared with the builder
// ---------------------------------------------------------------------------

/// The step a truss span is quantized to, in metres. The generator quantizes to
/// whole panels of its own; this is the step the *builder* offers, and the
/// design doc names it.
pub const LENGTH_STEP_M: f64 = 0.5;

/// What an extend whose ray met nothing puts in front of the socket.
pub const STUB_LENGTH_M: f64 = 0.5;

/// How far off the ray's line a socket may sit and still count as on the way:
/// the truss section's own half-width. A truss that would miss by more than its
/// own width is not on the way to anywhere.
const RAY_HALF_WIDTH_M: f64 = 0.15;

/// The catalog id of the straight generator — what an extend runs out of a
/// socket, and what a tower is made of.
pub const TRUSS_STRAIGHT: &str = "truss/straight";

/// The two ends of a generated stick, in the generator's own order.
const RUN_NEAR_END: &str = "end_a";
const RUN_FAR_END: &str = "end_b";

/// The largest buildable length no greater than `metres`.
#[must_use]
pub fn quantize_down(metres: f64) -> f64 {
    (metres / LENGTH_STEP_M).floor() * LENGTH_STEP_M
}

/// The nearest buildable length, never shorter than one step.
#[must_use]
pub fn quantize(metres: f64) -> f64 {
    (metres / LENGTH_STEP_M).round().max(1.0) * LENGTH_STEP_M
}

/// The socket on the other hand.
///
/// A **mirror**, spelled in the catalog's own names. Flipping a wing means
/// every relation inside it meets the room's other side, and the catalog
/// already says which side each socket is on: a deck's `corner_fl` faces
/// `corner_fr`, its `edge_left` faces `edge_right`, and a stick's `face_-z`
/// faces `face_+z`. A name with no side is its own mirror — a truss end and a
/// deck top are the same joint whichever way round the wing is.
///
/// The design doc forbids `mirror` as a *node kind* and as an *op*, and this is
/// neither: it is how `duplicate(flip=true)` spells itself in the one
/// vocabulary the graph already has, so the copy is ordinary rows that any
/// other verb can edit afterwards.
#[must_use]
pub fn mirror_socket(name: &str) -> String {
    const PAIRS: [(&str, &str); 4] = [
        ("_fl", "_fr"),
        ("_bl", "_br"),
        ("left", "right"),
        ("-z", "+z"),
    ];
    for (a, b) in PAIRS {
        if name.contains(a) {
            return name.replace(a, b);
        }
        if name.contains(b) {
            return name.replace(b, a);
        }
    }
    name.to_string()
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a verb produced no report.
///
/// Three variants, and only one of them is a *design* error: [`Self::Refused`]
/// is the pair of hard errors the design doc admits. The other two are the
/// caller naming something that is not there and the database saying no.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StageError {
    /// The graph would not accept the edit: a socket pair its polarity forbids,
    /// a cycle, an array asked to host, or an extend past the measured gap.
    /// The message is the resolver's own.
    #[error("{0}")]
    Refused(String),
    /// A node, socket or venue the caller named is not there.
    #[error("{0}")]
    NotFound(String),
    /// The venue could not be read, written, or resolved.
    #[error("{0}")]
    Internal(String),
}

impl From<String> for StageError {
    fn from(message: String) -> Self {
        StageError::Internal(message)
    }
}

/// Deleting a fixture goes through the patch service, which has its own
/// taxonomy. Mapped rather than wrapped: the message on the wire is the patch
/// layer's own, and only the variant becomes this module's.
impl From<crate::services::patch::PatchError> for StageError {
    fn from(error: crate::services::patch::PatchError) -> Self {
        use crate::services::patch::PatchError;
        let message = error.to_string();
        match error {
            PatchError::OutOfRange { .. } | PatchError::Collision { .. } => {
                StageError::Refused(message)
            }
            PatchError::Database(_) => StageError::Internal(message),
        }
    }
}

type Result<T> = std::result::Result<T, StageError>;

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/// One venue, and the two things every verb on it needs.
///
/// Constructed per call rather than held: a verb is a transaction, and the
/// only state between two of them is the rows.
pub struct Stage<'a> {
    pool: &'a SqlitePool,
    /// The fixtures root; the mesh root the catalog resolves against is derived
    /// from it.
    fixtures_root: &'a Path,
    venue_id: &'a str,
}

impl<'a> Stage<'a> {
    #[must_use]
    pub fn new(pool: &'a SqlitePool, fixtures_root: &'a Path, venue_id: &'a str) -> Self {
        Self {
            pool,
            fixtures_root,
            venue_id,
        }
    }

    // -- reads ----------------------------------------------------------

    /// The rows themselves — what the builder edits.
    ///
    /// # Errors
    /// Fails if the venue is not readable, or if converting the old schema
    /// fails.
    pub async fn rows(&self) -> Result<VenueGraphRows> {
        let mut access = self.read().await?;
        Ok(venue_graph_db::get_graph(&mut access).await?)
    }

    /// The venue solved — what every consumer draws.
    ///
    /// # Errors
    /// As [`Self::rows`], plus a catalog that will not resolve.
    pub async fn resolved(&self) -> Result<ResolvedVenue> {
        let mut access = self.read().await?;
        let solved = venue_graph::resolved(&mut access, self.fixtures_root).await?;
        Ok(ResolvedVenue::from(&solved))
    }

    /// The venue as a top-down text map — the "Gauntlet view".
    ///
    /// # Errors
    /// As [`Self::resolved`].
    pub async fn tiles(&self, cell_m: Option<f64>) -> Result<String> {
        let mut access = self.read().await?;
        let options = TileMap {
            cell_m: cell_m.unwrap_or(TileMap::default().cell_m),
            ..TileMap::default()
        };
        Ok(venue_graph::tiles(&mut access, self.fixtures_root, options).await?)
    }

    /// The tree as text: parent, socket pair, params, and everything the solve
    /// left open.
    ///
    /// The channel an agent reads after every mutation. It is the *relations*,
    /// not the metres — the sentence "the mover is on the downstage truss" that
    /// [`Self::tiles`] can only draw and a pose list cannot say at all.
    ///
    /// # Errors
    /// As [`Self::resolved`].
    pub async fn describe(&self) -> Result<String> {
        let mut access = self.read().await?;
        let graph = venue_graph::graph(&mut access).await?;
        let solved = venue_graph::resolved(&mut access, self.fixtures_root).await?;
        Ok(describe(&graph, &ResolvedVenue::from(&solved)))
    }

    /// Cast along an open socket's outward normal and report the first
    /// compatible socket it reaches.
    ///
    /// A *socket* search, not a mesh raycast: what an extend wants to know is
    /// where the run could end, and the answer is a joint, not a triangle.
    ///
    /// # Errors
    /// As [`Self::resolved`]; a socket the node does not have measures `None`
    /// rather than failing, because "nothing is in the way" is the answer for
    /// every direction nothing is in the way of.
    pub async fn reach(&self, node_id: &str, socket: &str) -> Result<Option<Reach>> {
        let mut access = self.read().await?;
        let graph = venue_graph::graph(&mut access).await?;
        let solved = venue_graph::resolved(&mut access, self.fixtures_root).await?;
        let sockets = venue_graph::sockets(self.fixtures_root)?;
        Ok(cast(&graph, &solved, sockets, node_id, socket))
    }

    // -- writes ---------------------------------------------------------

    /// Place a new node by mating two sockets.
    ///
    /// # Errors
    /// Refuses a socket pair that does not exist or whose polarity forbids the
    /// joint, and a parent that cannot host: one that would close a cycle, or
    /// an array, whose members are derived and have no row to bolt to.
    #[allow(clippy::too_many_arguments)]
    pub async fn attach(
        &self,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        parent_id: &str,
        my_socket: Option<&str>,
        their_socket: &str,
        yaw: f64,
        params: BTreeMap<String, f64>,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_kind(kind)?;
        self.require_catalog_ref(kind, catalog_ref)?;
        require_in_venue(&mut access, &[parent_id]).await?;
        // `derived` is whether the catalog picked the caller's half of the
        // joint rather than the caller naming it.
        let (my_socket, derived) = match my_socket {
            Some(name) => (name.to_string(), false),
            None => {
                let graph = venue_graph::graph(&mut access).await?;
                let socket = self.mating_socket(
                    &graph,
                    parent_id,
                    their_socket,
                    kind,
                    catalog_ref,
                    &params,
                )?;
                (socket, true)
            }
        };
        let node_id = self
            .insert(&mut access, kind, catalog_ref, label, params)
            .await?;
        let edge = Edge {
            parent: parent_id.to_string(),
            my_socket: my_socket.clone(),
            their_socket: their_socket.to_string(),
            roll: yaw,
        };
        self.check_and_write(&mut access, &node_id, edge).await?;
        let mut report = self.report(access, &node_id).await?;
        // The choice, said out loud. Picking the caller's half of the joint is
        // a convenience — `attach` scores socket *types*, so a piece with four
        // mating candidates gets whichever comes first, and a deck bolted on a
        // quarter turn round is indistinguishable in the report from the one
        // that was meant. Naming it is what lets a caller notice, without
        // making them name a socket they usually do not care about.
        if derived {
            report.warnings.push(format!(
                "`{node_id}` was bolted by its `{my_socket}` socket, the first that mates `{parent_id}.{their_socket}` — name `my_socket` to pick a different half of the joint"
            ));
        }
        Ok(report)
    }

    /// Place a node that already exists somewhere else — a re-attach, or a
    /// fixture dragged out of the patch tray.
    ///
    /// # Errors
    /// As [`Self::attach`].
    pub async fn reattach(
        &self,
        node_id: &str,
        parent_id: &str,
        my_socket: &str,
        their_socket: &str,
        yaw: f64,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id, parent_id]).await?;
        let edge = Edge {
            parent: parent_id.to_string(),
            my_socket: my_socket.to_string(),
            their_socket: their_socket.to_string(),
            roll: yaw,
        };
        self.check_and_write(&mut access, node_id, edge).await?;
        self.report(access, node_id).await
    }

    /// Free placement: put a node on a surface at `(u, v, yaw, trim)`.
    ///
    /// The floor and the grid are the venue root's own two surfaces, so a piece
    /// on the deck and a light in the air take the same path as a piece on a
    /// stage — there is no "unparented" branch to get wrong.
    ///
    /// # Errors
    /// As [`Self::attach`].
    #[allow(clippy::too_many_arguments)]
    pub async fn place_free(
        &self,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        surface_node_id: Option<&str>,
        surface_socket: Option<&str>,
        my_socket: Option<&str>,
        u: f64,
        v: f64,
        yaw: f64,
        trim: f64,
        extra: BTreeMap<String, f64>,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_kind(kind)?;
        self.require_catalog_ref(kind, catalog_ref)?;
        let parent_id = match surface_node_id {
            Some(id) => {
                require_in_venue(&mut access, &[id]).await?;
                id.to_string()
            }
            None => venue_graph_db::root_id(&mut access)
                .await?
                .ok_or_else(|| StageError::NotFound("this venue has no graph root".into()))?,
        };
        let mut params = extra;
        params.insert("u".to_string(), u);
        params.insert("v".to_string(), v);
        params.insert("trim".to_string(), trim);
        let surface = surface_socket.unwrap_or(FLOOR_SOCKET);
        let my_socket = match my_socket {
            Some(name) => name.to_string(),
            None => self.seat_socket(kind, catalog_ref, &params)?,
        };
        let node_id = self
            .insert(&mut access, kind, catalog_ref, label, params)
            .await?;
        let edge = Edge {
            parent: parent_id,
            my_socket,
            their_socket: surface.to_string(),
            roll: yaw,
        };
        self.check_and_write(&mut access, &node_id, edge).await?;
        self.report(access, &node_id).await
    }

    /// Unplace a node. It and its subtree drop out of the solve; the rows stay,
    /// so re-attaching restores the whole branch.
    ///
    /// # Errors
    /// Fails if the venue is not writable or the node is not in it.
    pub async fn detach(&self, node_id: &str) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id]).await?;
        venue_graph_db::delete_edge(&mut access, node_id).await?;
        self.report(access, node_id).await
    }

    /// Write down a far end: this socket meets that one.
    ///
    /// A **check**, not an edge — it is evaluated after the solve and never
    /// takes part in it, which is how a bridging piece has one parent and still
    /// says where its other end belongs.
    ///
    /// # Errors
    /// Refuses a socket pair that does not exist or whose polarity forbids the
    /// joint, and an array at either end. Whether the ends actually *meet* is
    /// not an error — that is the satisfied / violated / dangling the report
    /// carries.
    pub async fn constrain(
        &self,
        node_id: &str,
        my_socket: &str,
        target_node: &str,
        target_socket: &str,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id, target_node]).await?;
        let mut graph = venue_graph::graph(&mut access).await?;
        let sockets = venue_graph::sockets(self.fixtures_root)?;
        graph
            .constrain(
                Constraint {
                    node: node_id.to_string(),
                    my_socket: my_socket.to_string(),
                    target_node: target_node.to_string(),
                    target_socket: target_socket.to_string(),
                },
                sockets,
            )
            .map_err(|e| StageError::Refused(e.to_string()))?;
        venue_graph_db::upsert_constraint(
            &mut access,
            node_id,
            my_socket,
            target_node,
            target_socket,
        )
        .await?;
        self.report(access, node_id).await
    }

    /// Merge parameters into a node, and optionally rename it.
    ///
    /// `yaw` is spelled as itself and lands on the edge, which is where the
    /// mate's turn about the shared normal lives; every other key is a param.
    ///
    /// # Errors
    /// Fails if the venue is not writable or the node is not in it; refuses a
    /// `yaw` on a node no edge places.
    pub async fn set_params(
        &self,
        node_id: &str,
        mut params: BTreeMap<String, f64>,
        label: Option<&str>,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id]).await?;
        if let Some(yaw) = params.remove("yaw") {
            let graph = venue_graph::graph(&mut access).await?;
            let Some(edge) = graph.edge(node_id).cloned() else {
                return Err(StageError::Refused(
                    "an unplaced node has no yaw to set".into(),
                ));
            };
            venue_graph_db::upsert_edge(
                &mut access,
                node_id,
                &edge.parent,
                &edge.my_socket,
                &edge.their_socket,
                yaw,
            )
            .await?;
        }
        venue_graph_db::set_params(&mut access, node_id, &keep(params)).await?;
        if label.is_some() {
            venue_graph_db::set_label(&mut access, node_id, label).await?;
        }
        self.report(access, node_id).await
    }

    /// Delete a node and everything structural hanging off it.
    ///
    /// # A fixture is inventory, not structure
    ///
    /// Pulling a truss down loses the rig its shape, not its lights: every
    /// fixture under the deleted node is **trayed** — its edge cascades away
    /// with its parent, so the solve reports it unplaced and the tray can hang
    /// it somewhere else. Only a fixture the caller names *directly* is
    /// deleted, and then through [`fixture_create::delete`], which is the one
    /// door that takes the patch row with the node.
    ///
    /// # Errors
    /// Fails if the venue is not writable or the node is not in it; refuses the
    /// root.
    pub async fn delete_subtree(&self, node_id: &str) -> Result<ResolvedVenue> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id]).await?;
        let graph = venue_graph::graph(&mut access).await?;
        if node_id == graph.root() {
            return Err(StageError::Refused(
                "the venue root is the room itself and cannot be deleted".into(),
            ));
        }
        let (fixtures, structure): (Vec<String>, Vec<String>) = graph
            .subtree(node_id)
            .into_iter()
            .partition(|id| graph.node(id).is_some_and(|n| n.kind == NodeKind::Fixture));
        venue_graph_db::delete_nodes(&mut access, &structure).await?;
        if fixtures.iter().any(|id| id == node_id) {
            fixture_create::delete(&mut access, node_id).await?;
        }
        let solved = venue_graph::resolved(&mut access, self.fixtures_root).await?;
        let out = ResolvedVenue::from(&solved);
        venue_graph::commit_graph(access).await?;
        Ok(out)
    }

    /// Run a stick out of an open socket, along its outward normal.
    ///
    /// `length_m` of `None` means "to whatever the ray found", which is the gap
    /// when it found something and [`STUB_LENGTH_M`] when it did not. A length
    /// **equal** to the gap bridges it: the run hangs off the socket it grew
    /// from and the socket it reaches is written down as a far-end
    /// [`Self::constrain`], so `dangling()` reports the joint rather than an
    /// open end. **Less** than the gap is a stub. **Greater** is the design's
    /// second and last hard error — it is what stops structure growing through
    /// structure.
    ///
    /// # Errors
    /// [`StageError::Refused`] for a length past the measured gap, and as
    /// [`Self::attach`] otherwise.
    pub async fn extend(
        &self,
        node_id: &str,
        socket: &str,
        length_m: Option<f64>,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id]).await?;
        let graph = venue_graph::graph(&mut access).await?;
        let solved = venue_graph::resolved(&mut access, self.fixtures_root).await?;
        let sockets = venue_graph::sockets(self.fixtures_root)?;
        let reach = cast(&graph, &solved, sockets, node_id, socket);
        let length = match length_m {
            Some(metres) => quantize(metres),
            None => reach.as_ref().map_or(STUB_LENGTH_M, |r| r.gap_m),
        };
        if let Some(reach) = reach.as_ref() {
            if length > reach.gap_m + f64::EPSILON {
                return Err(StageError::Refused(format!(
                    "{length:.2} m is longer than the {:.2} m gap to {}.{} — extend to \
                     {:.2} m, or move what is in the way",
                    reach.gap_m, reach.node_id, reach.socket, reach.gap_m
                )));
            }
        }
        // Exactly the gap, and the far end is a **check**, not a second parent.
        let bridged = reach
            .filter(|r| (length - r.gap_m).abs() <= f64::EPSILON.max(1e-6))
            .map(|r| (r.node_id, r.socket));

        let node = self
            .insert(
                &mut access,
                NodeKind::Run.as_str(),
                Some(TRUSS_STRAIGHT),
                None,
                BTreeMap::from([("span".to_string(), length)]),
            )
            .await?;
        let edge = Edge {
            parent: node_id.to_string(),
            my_socket: RUN_NEAR_END.to_string(),
            their_socket: socket.to_string(),
            roll: 0.0,
        };
        self.check_and_write(&mut access, &node, edge).await?;
        if let Some((target_node, target_socket)) = bridged {
            let mut graph = venue_graph::graph(&mut access).await?;
            let constraint = Constraint {
                node: node.clone(),
                my_socket: RUN_FAR_END.to_string(),
                target_node: target_node.clone(),
                target_socket: target_socket.clone(),
            };
            graph
                .constrain(constraint, sockets)
                .map_err(|e| StageError::Refused(e.to_string()))?;
            venue_graph_db::upsert_constraint(
                &mut access,
                &node,
                RUN_FAR_END,
                &target_node,
                &target_socket,
            )
            .await?;
        }
        self.report(access, &node).await
    }

    /// Copy a subtree onto another socket, optionally mirrored.
    ///
    /// One [`Self::attach`] per node, root first, with each descendant's parent
    /// rewritten to whatever the root's copy became — so a wing arrives as
    /// ordinary rows that every other verb can edit afterwards. `flip` mirrors
    /// the copy's handedness: `u` changes sign (the only handed number in the
    /// vocabulary — `v` is across, `trim` is up, a span has no side), the roll
    /// negates, and **both** halves of every inner joint take their
    /// [`mirror_socket`] name, because a reflection turns the child over as
    /// well as the host.
    ///
    /// # Errors
    /// As [`Self::attach`], for the root's joint. A descendant whose own copy
    /// is refused leaves the copies already made — the transaction is one edit,
    /// and a wing that arrived half-built is visible in `describe()`, where a
    /// silent rollback would not be.
    pub async fn duplicate(
        &self,
        node_id: &str,
        parent_id: &str,
        their_socket: &str,
        flip: bool,
    ) -> Result<PlacementReport> {
        let mut access = self.write().await?;
        require_in_venue(&mut access, &[node_id, parent_id]).await?;
        let graph = venue_graph::graph(&mut access).await?;
        let steps = duplicate_plan(&graph, node_id, parent_id, their_socket, flip)
            .ok_or_else(|| StageError::NotFound(format!("`{node_id}` is not in this venue")))?;
        drop(graph);

        let mut minted: HashMap<String, String> = HashMap::new();
        let mut root_copy = None;
        for step in steps {
            let parent = match minted.get(&step.parent) {
                Some(copy) => copy.clone(),
                // The root's parent is the landing host, which is not a copy.
                None if root_copy.is_none() => step.parent.clone(),
                // A descendant whose parent's copy was skipped has nowhere to
                // hang; it is reported unplaced by the solve below.
                None => continue,
            };
            let copy = self
                .insert(
                    &mut access,
                    step.kind.as_str(),
                    step.catalog_ref.as_deref(),
                    step.label.as_deref(),
                    step.params,
                )
                .await?;
            let edge = Edge {
                parent,
                my_socket: step.my_socket,
                their_socket: step.their_socket,
                roll: step.yaw,
            };
            self.check_and_write(&mut access, &copy, edge).await?;
            minted.insert(step.source, copy.clone());
            root_copy.get_or_insert(copy);
        }
        let root_copy =
            root_copy.ok_or_else(|| StageError::NotFound("nothing to duplicate".into()))?;
        self.report(access, &root_copy).await
    }

    /// Patch, name, place and group `count` fixtures along one host face — the
    /// only fixture constructor besides the patch page's non-placed add.
    ///
    /// One verb, one transaction: the rows, the nodes, the edges and the
    /// addresses either all land or none do.
    ///
    /// # Errors
    /// A face the host does not have, a joint its polarity forbids, a mode the
    /// definition does not declare. A row that does not fit is **not** an
    /// error: it comes back as a report with `ok: false` and the length that
    /// would make it fit.
    #[allow(clippy::too_many_arguments)]
    pub async fn distribute(
        &self,
        host_node_id: Option<&str>,
        host_socket: Option<&str>,
        fixture_path: &str,
        mode_name: &str,
        count: usize,
        layout: DistributeLayout,
        label_prefix: Option<&str>,
    ) -> Result<Distributed> {
        let relative = confine(fixture_path)?;
        let mut access = self.write().await?;
        let report = distribute_service::distribute(
            &mut access,
            self.fixtures_root,
            distribute_service::Request {
                host_node: host_node_id,
                host_socket,
                fixture_path: relative,
                mode_name,
                count,
                layout: layout.into(),
                label_prefix,
            },
        )
        .await
        .map_err(StageError::Refused)?;

        // A refusal never reaches here with rows behind it, but it still opened
        // a transaction; committing an empty one is cheaper than branching.
        let patch = crate::database::local::fixtures::get_patched_fixtures(&mut access).await?;
        venue_graph::commit_graph(access).await?;
        crate::services::groups::invalidate_venue_fixture_cache();
        Ok(Distributed {
            report: report.into(),
            patch,
        })
    }

    /// A `catalog_ref` the catalog actually has.
    ///
    /// Without this a typo is *placed*: the node has no geometry, the solve
    /// warns and carries on — which is right for a venue whose catalog entry
    /// went away — and the mistake surfaces two layers later as a renderer
    /// failing to open a mesh. A ref that was never in the catalog is not that
    /// case; it is the caller naming a piece that does not exist, and the only
    /// moment anyone can say so usefully is the call that made it.
    ///
    /// A fixture is exempt: its `catalog_ref` is a patch-row id, not a piece.
    ///
    /// # Errors
    /// [`StageError::Refused`], naming the nearest entries the catalog does
    /// have, so the fix is in the message.
    fn require_catalog_ref(&self, kind: &str, catalog_ref: Option<&str>) -> Result<()> {
        let (Some(wanted), Some(kind)) = (catalog_ref, NodeKind::from_name(kind)) else {
            return Ok(());
        };
        if kind == NodeKind::Fixture {
            return Ok(());
        }
        let catalog = catalog(self.fixtures_root)?;
        if catalog.pieces.iter().any(|p| p.catalog_ref == wanted) {
            return Ok(());
        }
        let stem = wanted.rsplit('/').next().unwrap_or(wanted).to_lowercase();
        let near: Vec<&str> = catalog
            .pieces
            .iter()
            .filter(|p| {
                p.catalog_ref.to_lowercase().contains(&stem)
                    || p.name.to_lowercase().contains(&stem)
            })
            .map(|p| p.catalog_ref.as_str())
            .take(3)
            .collect();
        Err(StageError::Refused(if near.is_empty() {
            format!("`{wanted}` is not in the catalog — read `catalog()` for what is")
        } else {
            format!(
                "`{wanted}` is not in the catalog; did you mean {}?",
                near.join(", ")
            )
        }))
    }

    // -- which socket the piece is held by ------------------------------

    /// The socket a piece is *put down* on when the caller named none: the
    /// holdable socket facing furthest from local up.
    ///
    /// A rule rather than a table. Every piece that can rest on something has
    /// an underside, and an underside is the socket whose outward normal points
    /// down in the piece's own frame — which is exactly what the catalog
    /// authors with `normal(NEG_Y)` on every `bottom`, `mount`, `base` and
    /// `seat` it declares. Deriving it is what keeps "put a deck on the floor"
    /// from requiring the caller to know that a deck's underside is spelled
    /// `bottom` and a stick's is spelled `seat`.
    ///
    /// The same answer serves the `rig` plane, which faces down: a piece hangs
    /// *under* a down-facing host rather than turning over
    /// (`luma_scene::venue`'s `hangs_under`), so its underside is the footing
    /// there too and a flown truss keeps the underside it had on the floor.
    fn seat_socket(
        &self,
        kind: &str,
        catalog_ref: Option<&str>,
        params: &BTreeMap<String, f64>,
    ) -> Result<String> {
        let supply = venue_graph::sockets(self.fixtures_root)?;
        let held = supply.sockets(&probe(kind, catalog_ref, params));
        held.iter()
            .filter(|s| s.socket_type.polarity().can_be_held())
            .min_by(|a, b| {
                a.normal
                    .dot(luma_scene::snap::WORLD_UP)
                    .total_cmp(&b.normal.dot(luma_scene::snap::WORLD_UP))
            })
            .map(|s| s.name.clone())
            .ok_or_else(|| {
                StageError::Refused(format!(
                    "nothing on `{}` can rest on a surface",
                    catalog_ref.unwrap_or(kind)
                ))
            })
    }

    /// The socket a piece is *bolted by* when the caller named none: its first
    /// holdable socket whose type mates the host's.
    ///
    /// The same predicate the snap search scores candidates with, so a joint
    /// the builder would have made by dragging is the joint this makes by
    /// naming — and a pair the catalog forbids is refused here with the pair
    /// named, rather than at `attach` with an empty socket name.
    fn mating_socket(
        &self,
        graph: &VenueGraph,
        parent_id: &str,
        their_socket: &str,
        kind: &str,
        catalog_ref: Option<&str>,
        params: &BTreeMap<String, f64>,
    ) -> Result<String> {
        let supply = venue_graph::sockets(self.fixtures_root)?;
        let host = if parent_id == graph.root() {
            root_socket(their_socket)
        } else {
            let node = graph.node(parent_id).ok_or_else(|| {
                StageError::NotFound(format!("`{parent_id}` is not in this venue"))
            })?;
            supply
                .sockets(node)
                .into_iter()
                .find(|s| s.name == their_socket)
        };
        let host = host.ok_or_else(|| {
            StageError::Refused(format!("`{parent_id}` has no socket `{their_socket}`"))
        })?;
        supply
            .sockets(&probe(kind, catalog_ref, params))
            .iter()
            .find(|s| {
                s.socket_type.polarity().can_be_held() && s.socket_type.mates(host.socket_type)
            })
            .map(|s| s.name.clone())
            .ok_or_else(|| {
                StageError::Refused(format!(
                    "nothing on `{}` mates a `{}` socket like {parent_id}.{their_socket}",
                    catalog_ref.unwrap_or(kind),
                    host.socket_type.as_str()
                ))
            })
    }

    // -- plumbing -------------------------------------------------------

    /// A read snapshot, with the old schema converted first if it has not been.
    ///
    /// The conversion needs a write, so a read that finds an unconverted venue
    /// takes one, commits it, and then opens the snapshot. Ordinary reads —
    /// every one after the first — never take a write lock.
    async fn read(&self) -> Result<VenueAccess<'a, Read>> {
        self.ensure_migrated().await?;
        Ok(VenueAccess::<Read>::read(self.pool, VenueResource::Venue(self.venue_id)).await?)
    }

    async fn write(&self) -> Result<VenueAccess<'a, Write>> {
        self.ensure_migrated().await?;
        Ok(VenueAccess::<Write>::write(self.pool, VenueResource::Venue(self.venue_id)).await?)
    }

    async fn ensure_migrated(&self) -> Result<()> {
        Ok(venue_graph::ensure_migrated(self.pool, self.venue_id, self.fixtures_root).await?)
    }

    /// Mint a row and its params. Every node this module creates comes through
    /// here, so "a node exists" and "its params were written" are one step.
    async fn insert(
        &self,
        access: &mut VenueAccess<'_, Write>,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        params: BTreeMap<String, f64>,
    ) -> Result<String> {
        let node_id = venue_graph_db::insert_node(access, kind, catalog_ref, label).await?;
        if !params.is_empty() {
            venue_graph_db::set_params(access, &node_id, &keep(params)).await?;
        }
        Ok(node_id)
    }

    /// Check the edge against the graph's invariants, then write it.
    ///
    /// The check is `luma_scene`'s, not a copy: acyclic, both sockets present
    /// on their catalog entries, polarities compatible.
    async fn check_and_write(
        &self,
        access: &mut VenueAccess<'_, Write>,
        node_id: &str,
        edge: Edge,
    ) -> Result<()> {
        let mut graph = venue_graph::graph(access).await?;
        let sockets = venue_graph::sockets(self.fixtures_root)?;
        graph
            .attach(node_id, edge.clone(), sockets)
            .map_err(|e| StageError::Refused(e.to_string()))?;
        venue_graph_db::upsert_edge(
            access,
            node_id,
            &edge.parent,
            &edge.my_socket,
            &edge.their_socket,
            edge.roll,
        )
        .await?;
        Ok(())
    }

    /// Solve, commit, and report — the tail of every verb.
    async fn report(
        &self,
        mut access: VenueAccess<'_, Write>,
        node_id: &str,
    ) -> Result<PlacementReport> {
        let solved = venue_graph::resolved(&mut access, self.fixtures_root).await?;
        let report = PlacementReport::of(node_id, &solved);
        venue_graph::commit_graph(access).await?;
        Ok(report)
    }
}

/// What a distribution left behind: the report the caller shows, and the patch
/// as it now stands.
///
/// The patch rides along because a host with live output has to republish it
/// and only the caller knows whether it has one — pushing the decision down
/// here would make this module depend on Art-Net to place a light.
pub struct Distributed {
    pub report: DistributeReport,
    pub patch: Vec<crate::models::fixtures::PatchedFixture>,
}

/// The placeable vocabulary: node kinds, and every catalog piece with the
/// sockets it actually resolves to.
///
/// Derived from [`luma_scene::catalog`] and the same [`VenueSockets`] the
/// resolver mates against, so a piece's socket list here is the list `attach`
/// will accept — a hand-written table would be a second answer, and the first
/// one an agent believed.
///
/// # Errors
/// Fails if the catalog's geometry cannot be resolved.
pub fn catalog(fixtures_root: &Path) -> Result<StageCatalog> {
    let sockets = venue_graph::sockets(fixtures_root)?;
    Ok(StageCatalog::build(sockets))
}

// ---------------------------------------------------------------------------
// The ray
// ---------------------------------------------------------------------------

/// Every `attach` a duplicate owes, root first.
///
/// Public because the stage page previews a held copy by inserting exactly this
/// plan into a clone of the graph and resolving it: the ghost and the commit
/// walk one list, so a preview cannot draw a wing the verb would not build.
/// `None` when the node is not in the graph.
///
/// `flip` mirrors handedness about the joint. `u` changes sign — it runs along
/// the host feature's tangent, so its sign is which side of that feature a
/// child sits on, and nothing else in the vocabulary is handed (`v` is across,
/// `trim` is up, a span has no side). Every roll negates, and **both** halves of
/// every inner joint take their [`mirror_socket`] name, because a reflection
/// turns the child over as well as the host — mirroring only the host's side
/// built a wing that met its parent on a different socket than the hand-built
/// opposite does.
#[must_use]
pub fn duplicate_plan(
    graph: &VenueGraph,
    node_id: &str,
    parent_id: &str,
    their_socket: &str,
    flip: bool,
) -> Option<Vec<CopyStep>> {
    let source = graph.node(node_id)?;
    // The copy meets its new host by the same socket the original does, so a
    // wing bolted by a truss end is bolted by a truss end.
    let my_socket = graph.edge(node_id).map_or_else(
        || RUN_NEAR_END.to_string(),
        |edge| {
            if flip {
                mirror_socket(&edge.my_socket)
            } else {
                edge.my_socket.clone()
            }
        },
    );
    let yaw = graph
        .edge(node_id)
        .map_or(0.0, |edge| if flip { -edge.roll } else { edge.roll });
    let mut steps = vec![CopyStep {
        source: node_id.to_string(),
        kind: source.kind,
        catalog_ref: source.catalog_ref.clone(),
        label: source.label.clone(),
        parent: parent_id.to_string(),
        my_socket,
        their_socket: their_socket.to_string(),
        yaw,
        params: mirrored(source, flip),
    }];
    for id in graph
        .subtree(node_id)
        .into_iter()
        .filter(|id| id != node_id)
    {
        let (Some(node), Some(edge)) = (graph.node(&id), graph.edge(&id)) else {
            continue;
        };
        steps.push(CopyStep {
            source: id.clone(),
            kind: node.kind,
            catalog_ref: node.catalog_ref.clone(),
            label: node.label.clone(),
            parent: edge.parent.clone(),
            my_socket: if flip {
                mirror_socket(&edge.my_socket)
            } else {
                edge.my_socket.clone()
            },
            their_socket: if flip {
                mirror_socket(&edge.their_socket)
            } else {
                edge.their_socket.clone()
            },
            yaw: if flip { -edge.roll } else { edge.roll },
            params: mirrored(node, flip),
        });
    }
    Some(steps)
}

/// One `attach` a duplicate owes.
pub struct CopyStep {
    /// The node this copies, so the next step can name its copy as a parent.
    pub source: String,
    pub kind: NodeKind,
    pub catalog_ref: Option<String>,
    pub label: Option<String>,
    /// The *source* parent for a descendant; the landing host for the root.
    pub parent: String,
    pub my_socket: String,
    pub their_socket: String,
    /// Radians about the shared normal.
    pub yaw: f64,
    pub params: BTreeMap<String, f64>,
}

/// A node that has no row yet, for asking the socket supply what it would
/// have. Only ever handed to [`NodeSockets`], which asks a node for its catalog
/// entry and its parameters and never for its identity.
fn probe(kind: &str, catalog_ref: Option<&str>, params: &BTreeMap<String, f64>) -> Node {
    Node {
        id: String::new(),
        kind: NodeKind::from_name(kind).unwrap_or(NodeKind::Piece),
        catalog_ref: catalog_ref.map(str::to_string),
        label: None,
        params: params
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect::<Params>(),
    }
}

/// The two handed parameters: `u` runs along the host feature's tangent, so its
/// sign is which side of that feature a child sits on, and `pan` turns a head
/// about the mount normal, so its sign is which way that head looks. `v` is
/// across, `trim` is up, a span has no side, and a tilt is the same tilt in a
/// mirror.
fn mirrored(node: &Node, flip: bool) -> BTreeMap<String, f64> {
    node.params
        .iter()
        .map(|(key, value)| {
            let value = if flip && matches!(key, "u" | "pan") {
                -value
            } else {
                value
            };
            (key.to_string(), value)
        })
        .collect()
}

/// Cast along `from_socket`'s outward normal and report the nearest compatible
/// socket ahead of it.
///
/// Candidates must lie ahead of the origin along the normal and within
/// [`RAY_HALF_WIDTH_M`] of the line. The gap is quantized **down** to a
/// buildable span, so the answer is always a length something can actually be
/// made in.
fn cast(
    graph: &VenueGraph,
    solved: &Solved,
    supply: &VenueSockets,
    from_node: &str,
    from_socket: &str,
) -> Option<Reach> {
    let by_node: HashMap<&str, (DMat4, Vec<ResolvedSocket>)> = graph
        .nodes()
        .filter_map(|node| {
            let world = solved.pose(&node.id)?.world;
            Some((node.id.as_str(), (world, supply.sockets(node))))
        })
        .collect();

    let (origin_world, origin_sockets) = by_node.get(from_node)?;
    let socket = origin_sockets.iter().find(|s| s.name == from_socket)?;
    let origin = origin_world.transform_point3(socket.position);
    let direction = origin_world
        .transform_vector3(socket.normal)
        .normalize_or_zero();

    let mut best: Option<Reach> = None;
    for (node, (world, candidates)) in &by_node {
        if *node == from_node {
            continue;
        }
        for candidate in candidates {
            if candidate.socket_type == SocketType::Grab
                || !candidate.socket_type.mates(socket.socket_type)
            {
                continue;
            }
            let at: DVec3 = world.transform_point3(candidate.position);
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
                    node_id: (*node).to_string(),
                    socket: candidate.name.clone(),
                    gap_m,
                });
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// The tree as text.
///
/// Two indented spaces per level and one node per line, so a diff of two
/// descriptions localises a change to the line it happened on.
///
/// Primarily the *relation* channel — "the mover is on the downstage truss" —
/// but every line also carries the pose the solve gave that relation: `at=` in
/// metres and `heading=` in degrees, both data space, both world. Relations
/// alone cannot be checked. A caller who hangs a row on the face it believes is
/// the underside has no way to find out it was wrong from a tree of parents,
/// and this is the channel it reads after every verb, so the answer to "where
/// did that actually land" has to be on the line rather than one `tiles()`
/// call away.
///
/// Everything the solve left open is appended in the resolver's own words, via
/// the same [`ResolvedVenue`] projection every other surface reads, so a
/// warning reads identically here and on a placement report.
fn describe(graph: &VenueGraph, solved: &ResolvedVenue) -> String {
    let placed: std::collections::HashSet<&str> =
        solved.nodes.iter().map(|node| node.id.as_str()).collect();
    // Which way each light points, as the one stage word `StageDirection`
    // gives it. Only a fixture has a beam to report; a piece's `facing` is the
    // normal its own frame implies and means nothing to a reader.
    // Where each node ended up, for the half of a verb's effect a tree of
    // relations cannot state. Array members are keyed `<id>#<n>` and never
    // match a graph node, so they are simply absent here.
    let poses: BTreeMap<&str, ([f64; 3], f64)> = solved
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), (node.position, node.rotation[2])))
        .collect();
    let beams: BTreeMap<&str, &'static str> = solved
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Fixture.as_str())
        .map(|node| {
            (
                node.id.as_str(),
                fixture_kinematics::StageDirection::of(glam::DVec3::from(node.facing).as_vec3())
                    .label(),
            )
        })
        .collect();
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (node, edge) in graph.relations() {
        children
            .entry(edge.parent.as_str())
            .or_default()
            .push(node.id.as_str());
    }

    let root = graph.root();
    let mut out = format!("{root}  venue\n");
    write_branch(graph, &placed, &poses, &beams, &children, root, 1, &mut out);

    out.push_str("\nunplaced:");
    if solved.unplaced.is_empty() {
        out.push_str(" none\n");
    } else {
        out.push('\n');
        for node in &solved.unplaced {
            out.push_str(&format!(
                "  {}  {}{}  (+{} below)\n",
                node.node_id,
                node.kind,
                node.label
                    .as_deref()
                    .map(|l| format!("  \"{l}\""))
                    .unwrap_or_default(),
                node.descendants
            ));
        }
    }

    out.push_str("dangling:");
    if solved.dangling.is_empty() {
        out.push_str(" none\n");
    } else {
        // One line per *node*, not per socket. A room of a dozen decks has
        // thirty-odd open edges and only ever three or four nodes worth
        // looking at, so a flat list of ids buries the one joint that was
        // meant to be made under the twenty-six that were never going to be.
        // The label is carried because the graph has it and a bare uuid is
        // unreadable.
        out.push('\n');
        let mut by_node: BTreeMap<&str, Vec<&crate::models::venue_graph::ResolvedDangling>> =
            BTreeMap::new();
        for open in &solved.dangling {
            by_node.entry(open.node_id.as_str()).or_default().push(open);
        }
        for (node_id, opens) in by_node {
            let node = graph.node(node_id);
            let kind = node.map_or("?", |n| n.kind.as_str());
            let label = node
                .and_then(|n| n.label.as_deref())
                .map(|l| format!("  \"{l}\""))
                .unwrap_or_default();
            let sockets = opens
                .iter()
                .map(|open| format!("{} ({})", open.socket, open.socket_type))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "  {node_id}  {kind}{label}  {} open: {sockets}\n",
                opens.len()
            ));
        }
    }

    for check in &solved.constraints {
        out.push_str(&format!(
            "constraint: {}.{} -> {}.{}  {}\n",
            check.node_id, check.my_socket, check.target_node, check.target_socket, check.status
        ));
    }
    for warning in &solved.warnings {
        out.push_str(&format!("warning: {warning}\n"));
    }
    out
}

fn write_branch(
    graph: &VenueGraph,
    placed: &std::collections::HashSet<&str>,
    poses: &BTreeMap<&str, ([f64; 3], f64)>,
    beams: &BTreeMap<&str, &'static str>,
    children: &BTreeMap<&str, Vec<&str>>,
    parent: &str,
    depth: usize,
    out: &mut String,
) {
    let Some(ids) = children.get(parent) else {
        return;
    };
    let mut ids = ids.clone();
    ids.sort_unstable();
    for id in ids {
        let Some(node) = graph.node(id) else { continue };
        let indent = "  ".repeat(depth);
        let mut line = format!("{indent}{id}  {}", node.kind.as_str());
        if let Some(catalog_ref) = node.catalog_ref.as_deref() {
            line.push_str(&format!("  {catalog_ref}"));
        }
        if let Some(label) = node.label.as_deref() {
            line.push_str(&format!("  \"{label}\""));
        }
        if let Some(edge) = graph.edge(id) {
            line.push_str(&format!(
                "  by {} on .{}",
                edge.my_socket, edge.their_socket
            ));
            if edge.roll.abs() > f64::EPSILON {
                line.push_str(&format!("  yaw={:.0}deg", edge.roll.to_degrees()));
            }
        }
        for (key, value) in node.params.iter() {
            // Angles are radians in the graph and degrees at every surface —
            // this is one of those surfaces. See `venue::ANGLE_PARAMS`.
            if luma_scene::venue::ANGLE_PARAMS.contains(&key) {
                line.push_str(&format!("  {key}={:.0}deg", value.to_degrees()));
            } else {
                line.push_str(&format!("  {key}={value:.2}"));
            }
        }
        // Which way it points, for the half of a patch a tree of relations
        // cannot otherwise say. A face is only "underneath" for a piece the
        // right way up, and nothing else here would ever tell a reader that
        // the row it just hung is looking at the roof.
        if let Some(([x, y, z], heading)) = poses.get(id) {
            line.push_str(&format!(
                "  at=({x:.2}, {y:.2}, {z:.2})  heading={:.0}deg",
                heading.to_degrees()
            ));
        }
        if let Some(word) = beams.get(id) {
            line.push_str(&format!("  beam={word}"));
        }
        if !placed.contains(id) {
            line.push_str("  [unplaced]");
        }
        out.push_str(&line);
        out.push('\n');
        write_branch(graph, placed, poses, beams, children, id, depth + 1, out);
    }
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

fn require_kind(kind: &str) -> Result<()> {
    match NodeKind::from_name(kind) {
        Some(NodeKind::Venue) => Err(StageError::Refused(
            "a venue has exactly one root, and it is made with the venue".into(),
        )),
        Some(_) => Ok(()),
        None => Err(StageError::Refused(format!("`{kind}` is not a node kind"))),
    }
}

/// Every id a caller names must belong to the venue it was admitted to. Node
/// ids are *not* authorization.
async fn require_in_venue(access: &mut VenueAccess<'_, Write>, ids: &[&str]) -> Result<()> {
    let owned: Vec<String> = ids.iter().map(|id| (*id).to_string()).collect();
    let found = venue_graph_db::nodes_in_venue(access, &owned).await?;
    if found.len() == owned.len() {
        return Ok(());
    }
    // A refusal, not a not-found: which venue a node is in is this call's
    // precondition, and naming another room's node is the caller violating it.
    Err(StageError::Refused("that node is not in this venue".into()))
}

/// Reject a definition path that would escape the fixtures root. The path is
/// joined onto a root directory, so it is constrained where it is joined rather
/// than trusted where it came from.
fn confine(path: &str) -> Result<&str> {
    let escapes = Path::new(path).components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    });
    if escapes {
        return Err(StageError::Refused(format!(
            "fixture path escapes the fixtures root: {path}"
        )));
    }
    Ok(path)
}

/// Every key present, every value kept. The `Option` in the DB layer is for
/// clearing a key, which the wire has no spelling for yet.
fn keep(params: BTreeMap<String, f64>) -> BTreeMap<String, Option<f64>> {
    params.into_iter().map(|(k, v)| (k, Some(v))).collect()
}

#[cfg(test)]
mod tests {
    use super::{mirror_socket, quantize, quantize_down};

    #[test]
    fn lengths_are_quantized_to_the_half_metre() {
        assert!((quantize(3.2) - 3.0).abs() < 1e-9);
        assert!((quantize(3.3) - 3.5).abs() < 1e-9);
        assert!((quantize_down(3.4) - 3.0).abs() < 1e-9);
        // Never zero: a run of no length is not a run.
        assert!((quantize(0.1) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_side_is_swapped_and_a_sideless_socket_is_its_own_mirror() {
        assert_eq!(mirror_socket("corner_fl"), "corner_fr");
        assert_eq!(mirror_socket("corner_fr"), "corner_fl");
        assert_eq!(mirror_socket("edge_left"), "edge_right");
        assert_eq!(mirror_socket("face_-z"), "face_+z");
        // A truss end and a deck top have no side to swap.
        assert_eq!(mirror_socket("end_a"), "end_a");
        assert_eq!(mirror_socket("top"), "top");
        // The mirror is its own inverse.
        for name in ["corner_bl", "edge_right", "face_+z", "seat"] {
            assert_eq!(mirror_socket(&mirror_socket(name)), name);
        }
    }
}
