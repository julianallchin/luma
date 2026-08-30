//! The stage page: where a rig is **built**.
//!
//! Every other surface in the app reads a venue. This one writes it, and it is
//! the *only* one that writes a position — the patch page owns what exists and
//! how it is addressed, and there is no `set_position` anywhere
//! (`docs/specs/venue-builder-gauntlet.md`, AF8).
//!
//! # Where the state lives
//!
//! One structure, [`Build`], hung off the [`crate::visualizer::Visualizer`]
//! that is already showing the room. That is deliberate: the builder's every
//! gesture is aimed at the picture — a ghost follows the cursor over it,
//! sockets light up on it, a measurement runs across it — and the picture's
//! camera, scene and pointer already live there. A second owner would need the
//! camera copied to project a socket bead, and two cameras that can disagree
//! is how a widget ends up somewhere the pointer cannot find it.
//!
//! Within [`Build`], everything transient is [`hand::Hand`] — one state
//! machine, documented in that module.
//!
//! # Why the chrome is elements and not pixels
//!
//! The palette, the tray, the readouts, the socket beads and the measurement
//! label are all ordinary gpui elements laid over the viewport, not draws
//! inside it. Two reasons, and they are the same reason twice: the harness
//! sees elements and never pixels, and the headless harness has no renderer at
//! all — so a builder that expressed "this truss would snap here" only in the
//! picture would have no automated evidence for any of its transitions. The
//! picture carries the *shape* (`luma_render::overlay`); the element layer
//! carries the *claim*.

pub(crate) mod hand;

use std::collections::{BTreeMap, HashMap};

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, Pixels, Point};
use gpui_component::scroll::ScrollableElement as _;
use luma_lib::models::distribute::{DistributeLayout, DistributeReport};
use luma_lib::models::venue_graph::{PlacementReport, ResolvedVenue};
use luma_render::catalog::VenueSockets;
use luma_scene::catalog::{pieces, PaletteGroup};
use luma_scene::coords;
use luma_scene::venue::{NodeKind, NodeSockets as _, VenueGraph};
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{Enabled, CONTROL_HEIGHT};

use crate::library::{LibraryError, Rig};
use crate::Luma;

use hand::{Extending, Hand, Held, Holding, Landing, Room};

/// One palette row: a catalog entry, plus how this row means to put it down.
///
/// A tower and a stick are the same generator on different footings, so the
/// palette is a list of *rows* rather than a list of pieces — the row is what
/// carries the operator's intent.
pub(crate) struct PaletteRow {
    pub(crate) catalog_ref: &'static str,
    pub(crate) label: String,
    pub(crate) group: PaletteGroup,
    /// `true` for the row that stands a stick on end.
    pub(crate) tower: bool,
}

/// Every palette row, in catalog order within each group.
pub(crate) fn palette_rows() -> Vec<PaletteRow> {
    let mut rows: Vec<PaletteRow> = pieces()
        .iter()
        .map(|piece| PaletteRow {
            catalog_ref: piece.id,
            label: piece.display_name.to_string(),
            group: piece.palette_group,
            tower: false,
        })
        .collect();
    // The tower rides beside the stick it is made of, in the same group.
    let at = rows
        .iter()
        .position(|row| row.catalog_ref == hand::TRUSS_STRAIGHT)
        .map_or(rows.len(), |i| i + 1);
    rows.insert(
        at,
        PaletteRow {
            catalog_ref: hand::TRUSS_STRAIGHT,
            label: "Truss · tower".to_string(),
            group: PaletteGroup::Trusses,
            tower: true,
        },
    );
    rows
}

/// The builder's whole state.
pub(crate) struct Build {
    pub(crate) venue_id: String,
    /// The graph as rows, so a socket has a name to be clicked by.
    graph: VenueGraph,
    /// The graph solved, so a socket has a place to be clicked at.
    solved: ResolvedVenue,
    /// Every node's frame and sockets, in the shape the snap search walks.
    room: Room,
    /// What the cursor is holding. The one state machine.
    pub(crate) hand: Hand,
    /// The catalog, resolved once against the shipped meshes.
    sockets: VenueSockets,
    /// The selected node — a venue-graph id, not a render object.
    pub(crate) selected: Option<String>,
    /// Whether the palette is open.
    pub(crate) palette_open: bool,
    /// The distribution popup, when one is open.
    pub(crate) distribute: Option<Distribute>,
    /// The trim field's draft text. Held as typed so a half-entered number is
    /// not rounded under the operator's caret.
    pub(crate) trim_draft: Option<String>,
    /// The last verb's warnings, and its refusal if it had one.
    pub(crate) report: Vec<String>,
    /// A verb that has not come back yet. Placement is idempotent per gesture,
    /// so a second release while one is in flight is dropped rather than
    /// queued.
    pub(crate) committing: bool,
}

/// The distribution popup's state: a host feature, a fixture, a count, a
/// layout, and whatever the last attempt reported.
pub(crate) struct Distribute {
    pub(crate) host_node: String,
    pub(crate) host_socket: String,
    pub(crate) fixture_path: Option<String>,
    pub(crate) mode_name: Option<String>,
    pub(crate) query: String,
    pub(crate) results: Vec<(String, String)>,
    pub(crate) count: usize,
    pub(crate) layout: DistributeLayout,
    pub(crate) report: Option<DistributeReport>,
}

impl Distribute {
    fn new(host_node: String, host_socket: String) -> Self {
        Self {
            host_node,
            host_socket,
            fixture_path: None,
            mode_name: None,
            query: String::new(),
            results: Vec::new(),
            count: 4,
            layout: DistributeLayout::Even,
            report: None,
        }
    }
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
            palette_open: false,
            distribute: None,
            trim_draft: None,
            report: Vec::new(),
            committing: false,
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
        if self
            .selected
            .as_ref()
            .is_some_and(|id| self.graph.node(id).is_none())
        {
            self.selected = None;
        }
    }

    /// The label a node answers to in the readouts.
    pub(crate) fn label_of(&self, node: &str) -> String {
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
            Holding::Tray { .. } => vec![luma_render::catalog::fixture_clamp()],
        }
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

    /// The freedom a placed node actually has, which is what the inspector may
    /// offer and the gizmo may not.
    ///
    /// One reading of the joint, not a table: a socket already carries its own
    /// roll freedom, and a surface joint is the only kind whose child is free
    /// to be picked up and put down somewhere else.
    pub(crate) fn freedom_of(&self, node: &str) -> Freedom {
        let Some(edge) = self.graph.edge(node) else {
            return Freedom::Unplaced;
        };
        let Some(host) = self.room.socket(&edge.parent, &edge.their_socket) else {
            return Freedom::Unplaced;
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
/// The design's rule as a type: *snapped pieces have no transform gizmo*. Only
/// [`Freedom::Free`] gets one; the rest move in the one freedom their joint
/// admits, and a widget offering three axes to a bolted plate would be a widget
/// that lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freedom {
    /// Nothing has placed it — it is in the tray.
    Unplaced,
    /// Seated on the venue's own floor or grid: the gizmo's one case.
    Free,
    /// Bolted onto a face — it slides along it, and nowhere else.
    Slide,
    /// A joint with a roll: a clamp's yaw, a hinge's angle.
    Roll,
    /// A bolt circle with no freedom at all. Dragging it drags the run.
    Bolted,
}

impl Freedom {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Freedom::Unplaced => "unplaced",
            Freedom::Free => "translate",
            Freedom::Slide => "slide",
            Freedom::Roll => "roll",
            Freedom::Bolted => "none",
        }
    }

    /// The parameter this freedom edits, when it edits one.
    pub(crate) fn param(self) -> Option<&'static str> {
        match self {
            Freedom::Slide => Some("u"),
            Freedom::Roll => Some("yaw"),
            Freedom::Free | Freedom::Bolted | Freedom::Unplaced => None,
        }
    }
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

// ---------------------------------------------------------------------------
// The tab body
// ---------------------------------------------------------------------------

/// The stage tab. Almost stateless: what a builder is doing lives on the
/// [`Build`] beside the picture, and this is the venue the tab dies with.
#[derive(Debug)]
pub(crate) struct StagePage {
    pub(crate) venue_name: String,
}

impl Luma {
    /// Reveal the selected venue's builder as one target-keyed workspace tab.
    pub(crate) fn open_stage(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let venue_name = browser.venue_name().to_string();
        let target = crate::tabs::Target::Stage {
            venue: venue_id.clone(),
        };
        if self.workspace.body_mut(&target).is_some() {
            self.workspace.select(&target);
            cx.notify();
            return;
        }
        let page = StagePage { venue_name };
        self.open_tab(
            target,
            move || crate::shell::Body::Stage(Box::new(page)),
            cx,
        );
    }
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

impl Luma {
    pub(crate) fn build_mut(&mut self) -> Option<&mut Build> {
        self.visualizer.as_mut()?.build.as_mut()
    }

    pub(crate) fn build_state(&self) -> Option<&Build> {
        self.visualizer.as_ref()?.build.as_ref()
    }

    /// Run one verb and adopt the venue it hands back.
    ///
    /// Every mutating command returns the whole solved venue, so there is
    /// exactly one place a report is unpacked and exactly one place the room
    /// is re-read. `committing` is what keeps a second release during the
    /// round trip from placing a second copy.
    fn stage_verb(
        &mut self,
        pending: impl std::future::Future<Output = Result<PlacementReport, LibraryError>>
            + Send
            + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(build) = self.build_mut() {
            build.committing = true;
        }
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    match &result {
                        Ok(report) => {
                            build.report = report.warnings.clone();
                            if !report.ok {
                                build.report.push("the placement was refused".to_string());
                            }
                            // What the verb touched is what the inspector should
                            // be about: a placement the operator just made is the
                            // one they are about to trim, flip or detach.
                            build.selected = Some(report.node_id.clone());
                        }
                        Err(error) => build.report = vec![error.to_string()],
                    }
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the venue after a write.
    ///
    /// The whole rig rather than the report's venue: the picture is built from
    /// fixture definitions and meshes as well as poses, and a builder that
    /// updated the graph without the scene would draw the room it had before
    /// the edit.
    pub(crate) fn reload_stage(&mut self, cx: &mut Context<Self>) {
        let Some(venue) = self.visualizer.as_ref().map(|v| v.venue_id.clone()) else {
            return;
        };
        let pending = self.library.venue_rig(&venue);
        cx.spawn(async move |this, cx| {
            let loaded = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this
                    .visualizer
                    .as_mut()
                    .filter(|state| state.venue_id == venue)
                {
                    state.rig_reloaded(loaded);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Arm a palette row: from here the cursor carries a ghost.
    pub(crate) fn stage_arm(&mut self, row: &PaletteRow, cx: &mut Context<Self>) {
        let footing = hand::footing_for(row.catalog_ref, row.tower);
        let Some(build) = self.build_mut() else {
            return;
        };
        build.palette_open = false;
        build.hand = Hand::Holding(Held::new(Holding::Piece {
            catalog_ref: row.catalog_ref.to_string(),
            kind: NodeKind::Piece,
            display_name: row.label.clone(),
            footing,
            params: BTreeMap::new(),
        }));
        cx.notify();
    }

    /// Put the hand down without placing anything.
    pub(crate) fn stage_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Empty;
            build.palette_open = false;
        }
        cx.notify();
    }

    /// The cursor moved over the room while something is held.
    pub(crate) fn stage_aim(&mut self, world: glam::DVec3, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let held = build.held_sockets();
        let Some(hand::Held { what, latched, .. }) = build.hand.held() else {
            return;
        };
        let kind = what.kind();
        let footing = what.footing().map(str::to_string);
        let exclude = match what {
            Holding::Duplicate { root, .. } => Some(root.clone()),
            _ => None,
        };
        let latch = latched.clone();
        let solved = build.room.land(
            &held,
            kind,
            footing.as_deref(),
            world,
            latch.as_ref(),
            None,
            exclude.as_deref(),
        );
        if let Hand::Holding(held) = &mut build.hand {
            match solved {
                Some((landed, latch)) => {
                    held.landed = Some(landed);
                    held.latched = latch;
                }
                None => {
                    held.landed = None;
                    held.latched = None;
                }
            }
        }
        cx.notify();
    }

    /// Aim at a named socket — what a press on a socket bead means. The bead
    /// *is* the aim: a projected point would put the ghost a pixel of camera
    /// error away from the joint it is naming.
    pub(crate) fn stage_aim_socket(&mut self, node: &str, socket: &str, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Some(at) = build.room.socket_world(node, socket) else {
            return;
        };
        self.stage_aim(at, cx);
    }

    /// Release: commit whatever the ghost is standing on.
    pub(crate) fn stage_drop(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        if build.committing {
            return;
        }
        let venue = build.venue_id.clone();
        let Some(held) = build.hand.held() else {
            return;
        };
        let Some(landed) = held.landed.clone() else {
            return;
        };
        if landed.refused.is_some() {
            return;
        }
        let what = held.what.clone();
        build.hand = Hand::Empty;
        let root = build.room.root().to_string();
        let pending: Verb = match (&what, &landed.how) {
            (
                Holding::Tray { node, .. },
                Landing::Socket {
                    parent,
                    my_socket,
                    their_socket,
                    yaw,
                },
            ) => {
                Box::pin(
                    self.library
                        .reattach(&venue, node, parent, my_socket, their_socket, *yaw),
                )
            }
            // A fixture nobody has placed still has a row, so it is
            // re-attached rather than created — the tray is the one place an
            // unplaced fixture may live, never the origin.
            (
                Holding::Tray { node, .. },
                Landing::Free {
                    surface,
                    my_socket,
                    seat,
                },
            ) => {
                let (parent, socket) = surface
                    .clone()
                    .unwrap_or((root, luma_scene::venue::FLOOR_SOCKET.to_string()));
                Box::pin(
                    self.library
                        .reattach(&venue, node, &parent, my_socket, &socket, seat.yaw),
                )
            }
            (
                Holding::Piece {
                    catalog_ref,
                    kind,
                    display_name,
                    params,
                    ..
                },
                Landing::Socket {
                    parent,
                    my_socket,
                    their_socket,
                    yaw,
                },
            ) => Box::pin(self.library.attach(
                &venue,
                kind.as_str(),
                Some(catalog_ref),
                Some(display_name),
                parent,
                my_socket,
                their_socket,
                *yaw,
                params.clone(),
            )),
            (
                Holding::Piece {
                    catalog_ref,
                    kind,
                    display_name,
                    ..
                },
                Landing::Free {
                    surface,
                    my_socket,
                    seat,
                },
            ) => Box::pin(self.library.place_free(
                &venue,
                kind.as_str(),
                Some(catalog_ref),
                Some(display_name),
                surface.as_ref().map(|(n, s)| (n.as_str(), s.as_str())),
                my_socket,
                *seat,
            )),
            (Holding::Duplicate { root, flip, .. }, how) => {
                self.stage_duplicate_commit(&venue, root, *flip, how, cx);
                return;
            }
        };
        self.stage_verb(pending, cx);
    }

    /// Click an open socket: either aim the held piece at it, or start a run
    /// out of it.
    pub(crate) fn stage_socket_clicked(
        &mut self,
        node: String,
        socket: String,
        cx: &mut Context<Self>,
    ) {
        let holding = self
            .build_state()
            .is_some_and(|build| build.hand.held().is_some());
        if holding {
            self.stage_aim_socket(&node, &socket, cx);
            self.stage_drop(cx);
            return;
        }
        let Some(build) = self.build_mut() else {
            return;
        };
        let reach = build.room.cast(&node, &socket);
        let length_m = reach
            .as_ref()
            .map_or(hand::STUB_LENGTH_M, |reach| reach.gap_m);
        let label = build.label_of(&node);
        build.hand = Hand::Extending(Extending {
            from_node: node,
            from_node_label: label,
            from_socket: socket,
            reach,
            length_m,
        });
        cx.notify();
    }

    /// Type or drag a length for the run in hand.
    pub(crate) fn stage_set_length(&mut self, metres: f64, cx: &mut Context<Self>) {
        if let Some(Hand::Extending(run)) = self.build_mut().map(|build| &mut build.hand) {
            run.length_m = hand::quantize(metres);
        }
        cx.notify();
    }

    /// Commit the run: a bridge, or a stub.
    pub(crate) fn stage_commit_run(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Some(run) = build.hand.extending() else {
            return;
        };
        if run.refused().is_some() {
            return;
        }
        let venue = build.venue_id.clone();
        let parent = run.from_node.clone();
        let their_socket = run.from_socket.clone();
        let bridge = run.bridges().cloned();
        let params = BTreeMap::from([("span".to_string(), run.length_m)]);
        build.hand = Hand::Empty;
        let library = &self.library;
        let attach = library.attach(
            &venue,
            NodeKind::Run.as_str(),
            Some(hand::TRUSS_STRAIGHT),
            None,
            &parent,
            "end_a",
            &their_socket,
            0.0,
            params,
        );
        if let Some(bridge) = bridge {
            // The far end is a **check**, not a second parent: the run hangs
            // off the socket it grew from, and the socket it reaches is
            // written down separately so `dangling()` can report it satisfied.
            let venue = venue.clone();
            let library_constrain = |node: &str| {
                self.library
                    .constrain(&venue, node, "end_b", &bridge.node, &bridge.socket)
            };
            let _ = &library_constrain;
            cx.spawn(async move |this, cx| {
                let placed = attach.await;
                let node = placed.as_ref().ok().map(|r| r.node_id.clone());
                this.update(cx, |this, cx| {
                    if let (Some(node), Some(build)) = (node, this.build_state()) {
                        let venue = build.venue_id.clone();
                        let pending = this.library.constrain(
                            &venue,
                            &node,
                            "end_b",
                            &bridge.node,
                            &bridge.socket,
                        );
                        this.stage_verb(pending, cx);
                    } else {
                        this.reload_stage(cx);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        self.stage_verb(attach, cx);
    }

    /// ⌘D: copy the selected subtree and put it in the hand.
    pub(crate) fn stage_duplicate(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Some(root) = build.selected.clone() else {
            return;
        };
        let display_name = build.label_of(&root);
        build.hand = Hand::Holding(Held::new(Holding::Duplicate {
            root,
            display_name,
            flip: false,
        }));
        cx.notify();
    }

    /// Invert the handedness of the copy in hand.
    pub(crate) fn stage_flip(&mut self, cx: &mut Context<Self>) {
        if let Some(Hand::Holding(held)) = self.build_mut().map(|build| &mut build.hand) {
            if let Holding::Duplicate { flip, .. } = &mut held.what {
                *flip = !*flip;
            }
        }
        cx.notify();
    }

    /// Trim: how high a free placement flies. Children follow, because the
    /// resolver moves them — a subtree is a relation, not a set of poses.
    pub(crate) fn stage_set_trim(&mut self, metres: f64, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let (Some(node), venue) = (build.selected.clone(), build.venue_id.clone()) else {
            return;
        };
        build.trim_draft = None;
        let pending = self.library.set_params(
            &venue,
            &node,
            BTreeMap::from([("trim".to_string(), metres)]),
            None,
        );
        self.stage_verb(pending, cx);
    }

    /// Write one parameter of one node — the whole of what a joint's own
    /// freedom can change. `yaw` lands on the edge, which is where a mate's
    /// turn about the shared normal lives.
    pub(crate) fn stage_set_param(
        &mut self,
        node: &str,
        key: &str,
        value: f64,
        cx: &mut Context<Self>,
    ) {
        let Some(venue) = self.build_state().map(|build| build.venue_id.clone()) else {
            return;
        };
        let pending = self.library.set_params(
            &venue,
            node,
            BTreeMap::from([(key.to_string(), value)]),
            None,
        );
        self.stage_verb(pending, cx);
    }

    /// Detach the selected node. Its rows stay: it lands in the tray, which is
    /// the difference between unplaced and deleted.
    pub(crate) fn stage_detach(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let (Some(node), venue) = (build.selected.clone(), build.venue_id.clone()) else {
            return;
        };
        let pending = self.library.detach(&venue, &node);
        self.stage_verb(pending, cx);
    }

    /// Open the distribution popup on one host feature.
    pub(crate) fn stage_open_distribute(
        &mut self,
        node: String,
        socket: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(build) = self.build_mut() {
            build.distribute = Some(Distribute::new(node, socket));
        }
        self.stage_search_fixtures(String::new(), cx);
        cx.notify();
    }

    /// Search the QLC+ library for the distribution popup.
    pub(crate) fn stage_search_fixtures(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            if let Some(popup) = build.distribute.as_mut() {
                popup.query.clone_from(&query);
            }
        }
        let pending = self.library.search_fixtures(&query, 40);
        cx.spawn(async move |this, cx| {
            let found = pending.await;
            this.update(cx, |this, cx| {
                let found = match found {
                    Ok(found) => found,
                    // A library that will not answer is a state the popup has
                    // to show: an empty list that means "no such fixture" and
                    // one that means "the index never built" are the same
                    // picture, and only one of them is the operator's problem.
                    Err(error) => {
                        if let Some(build) = this.build_mut() {
                            build.report = vec![error.to_string()];
                        }
                        cx.notify();
                        return;
                    }
                };
                if let Some(popup) = this.build_mut().and_then(|b| b.distribute.as_mut()) {
                    popup.results = found
                        .into_iter()
                        .map(|entry| {
                            (
                                format!("{} {}", entry.manufacturer, entry.model),
                                entry.path,
                            )
                        })
                        .collect();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run the distribution the popup describes.
    pub(crate) fn stage_distribute(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let venue = build.venue_id.clone();
        let Some(popup) = build.distribute.as_ref() else {
            return;
        };
        let (Some(path), Some(mode)) = (popup.fixture_path.clone(), popup.mode_name.clone()) else {
            return;
        };
        let host = (popup.host_node.clone(), popup.host_socket.clone());
        let count = popup.count;
        let layout = popup.layout;
        let pending = self.library.distribute(
            &venue,
            Some((&host.0, &host.1)),
            &path,
            &mode,
            count,
            layout,
            None,
        );
        cx.spawn(async move |this, cx| {
            let report = pending.await;
            this.update(cx, |this, cx| {
                match report {
                    Ok(report) => {
                        if let Some(popup) = this.build_mut().and_then(|b| b.distribute.as_mut()) {
                            popup.report = Some(report);
                        }
                    }
                    // A distribution that never reached the command is not a
                    // fit failure and must not read as one: the popup keeps its
                    // last report and the refusal is said out loud.
                    Err(error) => {
                        if let Some(build) = this.build_mut() {
                            build.report = vec![error.to_string()];
                        }
                    }
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The fit failure's own fix: extend the host to the length it named, then
    /// try the same distribution again.
    pub(crate) fn stage_extend_and_retry(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let venue = build.venue_id.clone();
        let Some(fit) = build
            .distribute
            .as_ref()
            .and_then(|popup| popup.report.as_ref())
            .and_then(|report| report.fit.clone())
        else {
            return;
        };
        let pending = self.library.set_params(
            &venue,
            &fit.extend_node_id,
            BTreeMap::from([("span".to_string(), fit.needed_m)]),
            None,
        );
        cx.spawn(async move |this, cx| {
            let _ = pending.await;
            this.update(cx, |this, cx| {
                this.stage_distribute(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Place a duplicated subtree, root first, then every descendant against
    /// the copy of its parent.
    fn stage_duplicate_commit(
        &mut self,
        venue: &str,
        root: &str,
        flip: bool,
        how: &Landing,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_state() else {
            return;
        };
        let Some(plan) = build.copy_plan(root, flip, how) else {
            return;
        };
        let venue = venue.to_string();
        let library_calls: Vec<CopyStep> = plan;
        let first = library_calls.first().cloned();
        let Some(first) = first else { return };
        let rest = library_calls[1..].to_vec();
        let pending = self.library.attach(
            &venue,
            first.kind.as_str(),
            first.catalog_ref.as_deref(),
            first.label.as_deref(),
            &first.parent,
            &first.my_socket,
            &first.their_socket,
            first.yaw,
            first.params.clone(),
        );
        cx.spawn(async move |this, cx| {
            let mut minted: HashMap<String, String> = HashMap::new();
            let mut placed = pending.await;
            if let Ok(report) = &placed {
                minted.insert(first.source.clone(), report.node_id.clone());
            }
            for step in rest {
                let Some(parent) = minted.get(&step.parent).cloned() else {
                    continue;
                };
                let next = this
                    .update(cx, |this, _| {
                        this.library.attach(
                            &venue,
                            step.kind.as_str(),
                            step.catalog_ref.as_deref(),
                            step.label.as_deref(),
                            &parent,
                            &step.my_socket,
                            &step.their_socket,
                            step.yaw,
                            step.params.clone(),
                        )
                    })
                    .ok();
                let Some(next) = next else { break };
                placed = next.await;
                if let Ok(report) = &placed {
                    minted.insert(step.source.clone(), report.node_id.clone());
                }
            }
            this.update(cx, |this, cx| {
                if let (Ok(report), Some(build)) = (&placed, this.build_mut()) {
                    build.report = report.warnings.clone();
                    build.selected = minted.get(&first.source).cloned();
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// One `attach` a duplicate owes.
#[derive(Clone)]
struct CopyStep {
    /// The node this copies, so the next step can name its copy as a parent.
    source: String,
    kind: NodeKind,
    catalog_ref: Option<String>,
    label: Option<String>,
    /// The *source* parent for a descendant; the landing's host for the root.
    parent: String,
    my_socket: String,
    their_socket: String,
    yaw: f64,
    params: BTreeMap<String, f64>,
}

/// One verb in flight. Boxed because the four placements are four different
/// futures of one shape, and the caller only ever awaits them.
type Verb = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<PlacementReport, LibraryError>> + Send>,
>;

impl Build {
    /// The `attach` calls that reproduce a subtree at a new joint.
    ///
    /// **Flip** is the mirror about the root socket's own normal, and it is
    /// expressed as the one number an edge has: `roll`. A wing bolted to a
    /// stage's downstage-left corner and duplicated onto downstage-right is
    /// the same subtree turned half a turn about the joint, which is what
    /// makes the copy's handedness the opposite of the original's — the
    /// design doc's "inverts the subtree's handedness about its root socket".
    /// No node kind, no mirrored geometry, and nothing to keep in sync: the
    /// resolver already clamps a roll to the freedom its joint admits, so a
    /// flip a bolted joint cannot express comes back as a warning rather than
    /// a lie.
    fn copy_plan(&self, root: &str, flip: bool, how: &Landing) -> Option<Vec<CopyStep>> {
        let Landing::Socket {
            parent,
            my_socket,
            their_socket,
            yaw,
        } = how
        else {
            // A duplicate is placed on a socket: every compatible open socket
            // lights up precisely so the copy lands on a joint, and a copy
            // seated free would be a second, different gesture.
            return None;
        };
        let mut steps = Vec::new();
        let source = self.graph.node(root)?;
        steps.push(CopyStep {
            source: root.to_string(),
            kind: source.kind,
            catalog_ref: source.catalog_ref.clone(),
            label: source.label.clone(),
            parent: parent.clone(),
            my_socket: my_socket.clone(),
            their_socket: their_socket.clone(),
            yaw: yaw + if flip { std::f64::consts::PI } else { 0.0 },
            params: source
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        });
        for id in self.graph.subtree(root).into_iter().filter(|id| id != root) {
            let (Some(node), Some(edge)) = (self.graph.node(&id), self.graph.edge(&id)) else {
                continue;
            };
            steps.push(CopyStep {
                source: id.clone(),
                kind: node.kind,
                catalog_ref: node.catalog_ref.clone(),
                label: node.label.clone(),
                parent: edge.parent.clone(),
                my_socket: edge.my_socket.clone(),
                their_socket: edge.their_socket.clone(),
                yaw: edge.roll,
                params: node
                    .params
                    .iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            });
        }
        Some(steps)
    }
}

// ---------------------------------------------------------------------------
// What the page shows
// ---------------------------------------------------------------------------

/// The builder, projected into the shapes the tab body draws.
///
/// Owned rather than borrowed because the tab body is rendered inside a
/// mutable borrow of the workspace, and the builder lives beside the picture.
/// Every field is also a *claim* the harness can read — see the module docs.
pub(crate) struct StageView {
    pub(crate) hand: String,
    pub(crate) landing: Option<String>,
    pub(crate) refusal: Option<String>,
    pub(crate) measurement: Option<(String, String)>,
    pub(crate) length_m: Option<f64>,
    pub(crate) palette_open: bool,
    pub(crate) selected: Option<SelectedView>,
    pub(crate) tray: Vec<(String, String)>,
    pub(crate) dangling: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) distribute: Option<DistributeView>,
    pub(crate) faces: Vec<(String, String, String)>,
}

/// The selected node, and the freedoms it actually has.
pub(crate) struct SelectedView {
    pub(crate) node: String,
    pub(crate) label: String,
    /// The freedom the joint admits. A snapped piece has no transform gizmo —
    /// it moves in this and nowhere else.
    pub(crate) freedom: Freedom,
    pub(crate) relation: Option<String>,
    pub(crate) constraint: Option<String>,
    pub(crate) trim: f64,
    pub(crate) param: f64,
    pub(crate) draft: Option<String>,
}

pub(crate) struct DistributeView {
    pub(crate) host: String,
    pub(crate) layout: &'static str,
    pub(crate) fixture: Option<String>,
    pub(crate) mode: Option<String>,
    pub(crate) count: usize,
    pub(crate) results: Vec<(String, String)>,
    pub(crate) placed: Option<usize>,
    pub(crate) fit: Option<String>,
}

impl Luma {
    /// The builder as the page draws it, or `None` when no room is up.
    pub(crate) fn stage_view(&self) -> Option<StageView> {
        let build = self.build_state()?;
        let held = build.hand.held();
        let run = build.hand.extending();
        let selected = build.selected.as_ref().map(|node| {
            let freedom = build.freedom_of(node);
            let param = |key: &str| {
                build
                    .solved
                    .nodes
                    .iter()
                    .find(|n| &n.id == node)
                    .and_then(|n| n.params.get(key).copied())
                    .unwrap_or(0.0)
            };
            SelectedView {
                node: node.clone(),
                label: build.label_of(node),
                freedom,
                relation: build.relation_of(node),
                constraint: build.constraint_of(node),
                trim: param("trim"),
                param: freedom.param().map_or(0.0, param),
                draft: build.trim_draft.clone(),
            }
        });
        Some(StageView {
            hand: build.hand.readout(),
            landing: held
                .and_then(|h| h.landed.as_ref())
                .map(hand::Landed::readout),
            refusal: run
                .and_then(Extending::refused)
                .or_else(|| held.and_then(|h| h.landed.as_ref()?.refused.clone())),
            measurement: run.map(|run| {
                (
                    run.measurement(),
                    hand::feet_and_inches(run.reach.as_ref().map_or(run.length_m, |r| r.gap_m)),
                )
            }),
            length_m: run.map(|run| run.length_m),
            palette_open: build.palette_open,
            selected,
            tray: build
                .solved
                .unplaced
                .iter()
                .map(|node| {
                    (
                        node.node_id.clone(),
                        node.label.clone().unwrap_or_else(|| node.node_id.clone()),
                    )
                })
                .collect(),
            dangling: build
                .solved
                .dangling
                .iter()
                .map(|d| format!("{} {}", build.label_of(&d.node_id), d.socket))
                .collect(),
            warnings: build.report.clone(),
            distribute: build.distribute.as_ref().map(|popup| DistributeView {
                host: format!("{} {}", build.label_of(&popup.host_node), popup.host_socket),
                layout: match popup.layout {
                    DistributeLayout::Even => "even",
                    DistributeLayout::Spacing { .. } => "spacing",
                    DistributeLayout::Span { .. } => "span",
                },
                fixture: popup.fixture_path.clone(),
                mode: popup.mode_name.clone(),
                count: popup.count,
                results: popup.results.clone(),
                placed: popup
                    .report
                    .as_ref()
                    .filter(|r| r.ok)
                    .map(|r| r.fixtures.len()),
                fit: popup
                    .report
                    .as_ref()
                    .and_then(|r| r.fit.as_ref())
                    .map(|fit| fit.suggestion.clone()),
            }),
            faces: build
                .room
                .open_sockets()
                // A *feature*, not every host: a distribution runs along
                // something, so the vocabulary is surfaces and edges. A bolt
                // circle hosts structure and seats nothing.
                .filter(|(_, socket)| hand::is_feature(socket))
                .map(|(node, socket)| {
                    (
                        node.to_string(),
                        socket.name.clone(),
                        format!("{} {}", build.label_of(node), socket.name),
                    )
                })
                .collect(),
        })
    }
}

/// The tab body: the tools, under the room.
pub(crate) fn stage_page(
    state: &StagePage,
    app: &Entity<Luma>,
    view: Option<&StageView>,
) -> AnyElement {
    let Some(view) = view else {
        return div()
            .size_full()
            .bg(ladder::background())
            .child(luma_ui::plate(
                "The room is not up.".to_string(),
                ladder::muted_foreground(),
            ))
            .agent_node(Role::Card, format!("{} Stage builder", state.venue_name))
            .into_any_element();
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .child(header(state, app, view))
        .child(readout(view))
        .child(run_controls(view, app))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(tray(view, app))
                .child(inspector(view, app)),
        )
        .children(
            view.distribute
                .as_ref()
                .map(|popup| distribute_card(popup, app)),
        )
        .agent_node(Role::Card, format!("{} Stage builder", state.venue_name))
        .into_any_element()
}

fn header(state: &StagePage, app: &Entity<Luma>, view: &StageView) -> AnyElement {
    let action = |label: &'static str, run: fn(&mut Luma, &mut Context<Luma>)| {
        let app = app.clone();
        luma_ui::luma_button(label, Enabled::Yes)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| run(this, cx));
            })
            .agent_node(Role::Button, label)
    };
    div()
        .flex_shrink_0()
        .px(px(18.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .border_b_1()
        .border_color(ladder::trim())
        .child(luma_ui::silkscreen("STAGE BUILDER"))
        .child(div().flex_1())
        .child(palette_trigger(app, view))
        .child(action("Duplicate", |this, cx| this.stage_duplicate(cx)))
        .child(action("Flip", |this, cx| this.stage_flip(cx)))
        .child(action("Detach", |this, cx| this.stage_detach(cx)))
        .child(action("Cancel", |this, cx| this.stage_cancel(cx)))
        .agent_node(Role::Row, format!("{} builder actions", state.venue_name))
        .into_any_element()
}

/// The palette: catalog rows, grouped, each arming the hand.
fn palette_trigger(app: &Entity<Luma>, view: &StageView) -> AnyElement {
    let toggle = app.clone();
    let mut trigger = luma_ui::luma_button("Palette", Enabled::Yes)
        .id("stage-palette")
        .on_click(move |_, _, cx| {
            toggle.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.palette_open = !build.palette_open;
                }
                cx.notify();
            });
        })
        .agent_node(Role::Button, "Palette")
        .into_any_element();
    if !view.palette_open {
        return trigger;
    }
    let dismiss = app.clone();
    let mut card = float::popover_card().w(px(240.0)).max_h(px(420.0));
    for group in PaletteGroup::ALL {
        let rows: Vec<PaletteRow> = palette_rows()
            .into_iter()
            .filter(|row| row.group == group)
            .collect();
        if rows.is_empty() {
            continue;
        }
        card = card.child(float::section_heading(group.as_str()));
        for row in rows {
            let app = app.clone();
            let label = row.label.clone();
            card = card.child(
                float::menu_row(RowState::Rest, label.clone())
                    .id(gpui::SharedString::from(format!(
                        "palette:{}:{}",
                        row.catalog_ref, row.tower
                    )))
                    .child(label.clone())
                    .on_click(move |_, _, cx| {
                        let row = PaletteRow {
                            catalog_ref: row.catalog_ref,
                            label: row.label.clone(),
                            group: row.group,
                            tower: row.tower,
                        };
                        app.update(cx, |this, cx| this.stage_arm(&row, cx));
                    })
                    .agent_node(Role::Row, label),
            );
        }
    }
    trigger = div()
        .relative()
        .child(trigger)
        .child(float::anchored_below(
            "stage-palette-menu",
            CONTROL_HEIGHT,
            Dismiss::on_press_out(move |_, cx| {
                dismiss.update(cx, |this, cx| {
                    if let Some(build) = this.build_mut() {
                        build.palette_open = false;
                    }
                    cx.notify();
                });
            }),
            card.into_any_element(),
        ))
        .into_any_element();
    trigger
}

/// The claim strip: what is held, where it would land, why it would not.
fn readout(view: &StageView) -> AnyElement {
    let line = |text: String, color: gpui::Rgba| {
        div()
            .text_size(px(11.0))
            .text_color(color)
            .child(text.clone())
            .agent_node(Role::Text, text)
    };
    div()
        .flex_shrink_0()
        .px(px(18.0))
        .py(px(8.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .border_b_1()
        .border_color(ladder::trim())
        .child(line(view.hand.clone(), ladder::foreground_90()))
        .children(
            view.landing
                .clone()
                .map(|text| line(text, ladder::muted_foreground())),
        )
        .children(view.measurement.clone().map(|(metres, feet)| {
            div()
                .flex()
                .items_baseline()
                .gap(px(6.0))
                .child(line(metres, ladder::accent()))
                .child(
                    div()
                        .text_size(px(9.0))
                        .text_color(ladder::muted_foreground())
                        .child(feet.clone())
                        .agent_node(Role::Text, feet),
                )
                .into_any_element()
        }))
        .children(
            view.refusal
                .clone()
                .map(|text| line(format!("Refused: {text}"), ladder::danger())),
        )
        .children(
            view.warnings
                .iter()
                .map(|text| line(format!("Warning: {text}"), ladder::status_warn())),
        )
        .into_any_element()
}

/// The tray: fixtures the patch knows about that nothing has placed.
fn tray(view: &StageView, app: &Entity<Luma>) -> AnyElement {
    div()
        .w(px(240.0))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(ladder::trim())
        .child(
            div()
                .px(px(12.0))
                .py(px(8.0))
                .child(luma_ui::silkscreen("TRAY")),
        )
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .children(view.tray.iter().map(|(node, label)| {
                    let app = app.clone();
                    let node = node.clone();
                    let label_for_click = label.clone();
                    div()
                        .id(gpui::SharedString::from(format!("tray:{node}")))
                        .px(px(12.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .text_color(ladder::foreground_90())
                        .hover(|row| row.bg(ladder::hover()))
                        .child(label.clone())
                        .on_click(move |_, _, cx| {
                            let node = node.clone();
                            let label = label_for_click.clone();
                            app.update(cx, |this, cx| {
                                if let Some(build) = this.build_mut() {
                                    build.hand =
                                        Hand::Holding(Held::new(Holding::Tray { node, label }));
                                }
                                cx.notify();
                            });
                        })
                        .agent_node(Role::Row, label.clone())
                }))
                .when(view.tray.is_empty(), |list| {
                    list.child(float::empty_row("Nothing unplaced"))
                }),
        )
        .into_any_element()
}

/// The inspector: what is selected, what freedom it has, and what the last
/// solve is unhappy about.
fn inspector(view: &StageView, app: &Entity<Luma>) -> AnyElement {
    let mut column = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .px(px(18.0))
        .py(px(10.0))
        // The feature list is as long as the room is complicated; without this
        // the last rows are clipped and a hand cannot reach them.
        .overflow_y_scrollbar();
    match &view.selected {
        None => {
            column = column.child(float::empty_row("Nothing selected"));
        }
        Some(selected) => {
            let claim = |text: String, color: gpui::Rgba| {
                div()
                    .text_size(px(10.0))
                    .text_color(color)
                    .child(text.clone())
                    .agent_node(Role::Text, text)
            };
            column = column
                .child(luma_ui::silkscreen(selected.label.clone()))
                .child(claim(
                    format!("Gizmo: {}", selected.freedom.as_str()),
                    ladder::muted_foreground(),
                ))
                .children(
                    selected
                        .relation
                        .clone()
                        .map(|text| claim(text, ladder::muted_foreground())),
                )
                .children(
                    selected
                        .constraint
                        .clone()
                        .map(|text| claim(text, ladder::accent())),
                );
            if let Some(key) = selected.freedom.param() {
                let nudge = |app: Entity<Luma>, label: &'static str, delta: f64| {
                    let node = selected.node.clone();
                    let value = selected.param + delta;
                    luma_ui::luma_button(label, Enabled::Yes)
                        .id(label)
                        .on_click(move |_, _, cx| {
                            let node = node.clone();
                            app.update(cx, |this, cx| {
                                this.stage_set_param(&node, key, value, cx);
                            });
                        })
                        .agent_node(Role::Button, label)
                };
                let step = if key == "u" { 0.25 } else { 0.5 };
                column = column.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .w(px(48.0))
                                .text_size(px(10.0))
                                .text_color(ladder::param_label())
                                .child(key.to_uppercase()),
                        )
                        .child(claim(
                            format!("{} {:.2}", key.to_uppercase(), selected.param),
                            ladder::foreground_90(),
                        ))
                        .child(nudge(app.clone(), "Nudge back", -step))
                        .child(nudge(app.clone(), "Nudge on", step)),
                );
            }
            if selected.freedom == Freedom::Free {
                let value = selected
                    .draft
                    .clone()
                    .unwrap_or_else(|| format!("{:.2}", selected.trim));
                let up = app.clone();
                let down = app.clone();
                let step = |app: Entity<Luma>, delta: f64, label: &'static str, trim: f64| {
                    luma_ui::luma_button(label, Enabled::Yes)
                        .id(label)
                        .on_click(move |_, _, cx| {
                            app.update(cx, |this, cx| {
                                this.stage_set_trim((trim + delta).max(0.0), cx);
                            });
                        })
                        .agent_node(Role::Button, label)
                };
                column = column.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .w(px(48.0))
                                .text_size(px(10.0))
                                .text_color(ladder::param_label())
                                .child("TRIM"),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(ladder::foreground_90())
                                .child(format!("{value} m"))
                                .agent_node(Role::Input, format!("Trim {value}")),
                        )
                        .child(step(down, -0.5, "Trim down", selected.trim))
                        .child(step(up, 0.5, "Trim up", selected.trim)),
                );
            }
        }
    }
    column = column.child(
        div()
            .text_size(px(10.0))
            .text_color(ladder::muted_foreground())
            .child(format!("Dangling: {}", view.dangling.len()))
            .agent_node(Role::Text, format!("Dangling {}", view.dangling.len())),
    );
    column
        .children(view.dangling.iter().map(|text| {
            div()
                .text_size(px(10.0))
                .text_color(ladder::status_warn())
                .child(text.clone())
                .agent_node(Role::Text, format!("Dangling {text}"))
        }))
        .child(
            div()
                .mt(px(10.0))
                .child(luma_ui::silkscreen("DISTRIBUTE ONTO"))
                .children(view.faces.iter().take(24).map(|(node, socket, label)| {
                    let app = app.clone();
                    let node = node.clone();
                    let socket = socket.clone();
                    div()
                        .id(gpui::SharedString::from(format!("face:{node}:{socket}")))
                        .py(px(4.0))
                        .text_size(px(10.0))
                        .text_color(ladder::muted_foreground())
                        .hover(|row| row.bg(ladder::hover()))
                        .child(label.clone())
                        .on_click(move |_, _, cx| {
                            let (node, socket) = (node.clone(), socket.clone());
                            app.update(cx, |this, cx| {
                                this.stage_open_distribute(node, socket, cx);
                            });
                        })
                        .agent_node(Role::Row, label.clone())
                })),
        )
        .into_any_element()
}

/// The distribution popup: a fixture, a count, a layout, and the report.
fn distribute_card(view: &DistributeView, app: &Entity<Luma>) -> AnyElement {
    let mut card = float::popover_card()
        .absolute()
        .right(px(24.0))
        .bottom(px(24.0))
        .w(px(320.0))
        .occlude()
        .child(luma_ui::silkscreen(format!("DISTRIBUTE · {}", view.host)));
    for (label, path) in view.results.iter().take(8) {
        let app = app.clone();
        let path = path.clone();
        let chosen = view.fixture.as_deref() == Some(path.as_str());
        card = card.child(
            float::menu_row(RowState::of(chosen, false), label.clone())
                .id(gpui::SharedString::from(format!("fixture:{path}")))
                .child(label.clone())
                .on_click(move |_, _, cx| {
                    let path = path.clone();
                    app.update(cx, |this, cx| {
                        if let Some(popup) = this.build_mut().and_then(|b| b.distribute.as_mut()) {
                            popup.fixture_path = Some(path.clone());
                            popup.mode_name = None;
                        }
                        let pending = this.library.fixture_definition(&path);
                        cx.spawn(async move |this, cx| {
                            let mode = pending
                                .await
                                .ok()
                                .and_then(|def| def.modes.first().map(|mode| mode.name.clone()));
                            this.update(cx, |this, cx| {
                                if let Some(popup) =
                                    this.build_mut().and_then(|b| b.distribute.as_mut())
                                {
                                    popup.mode_name = mode;
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                        cx.notify();
                    });
                })
                .agent_node(Role::Row, label.clone()),
        );
    }
    let count = |label: &'static str, delta: i64| {
        let app = app.clone();
        luma_ui::luma_button(label, Enabled::Yes)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(popup) = this.build_mut().and_then(|b| b.distribute.as_mut()) {
                        popup.count = popup.count.saturating_add_signed(delta as isize).max(1);
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Button, label)
    };
    let layout = {
        // Two of the three layouts, because the third is a pair of fractions and
        // wants a handle rather than a button. Even always fits; a pitch is the
        // one that can be refused, which is what makes the fit failure a state
        // a hand can reach.
        let app = app.clone();
        let label = if view.layout == "even" {
            "Layout even"
        } else {
            "Layout spacing"
        };
        luma_ui::luma_button(label, Enabled::Yes)
            .id("stage-layout")
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(popup) = this.build_mut().and_then(|b| b.distribute.as_mut()) {
                        popup.layout = match popup.layout {
                            DistributeLayout::Even => DistributeLayout::Spacing { metres: 1.0 },
                            _ => DistributeLayout::Even,
                        };
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Button, label)
    };
    let run = app.clone();
    let retry = app.clone();
    card = card
        .child(layout)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::foreground_90())
                        .child(format!("Count {}", view.count))
                        .agent_node(Role::Text, format!("Count {}", view.count)),
                )
                .child(count("Fewer", -1))
                .child(count("More", 1)),
        )
        .child({
            let ready = view.fixture.is_some() && view.mode.is_some();
            luma_ui::luma_button("Distribute", if ready { Enabled::Yes } else { Enabled::No })
                .id("stage-distribute")
                .on_click(move |_, _, cx| {
                    run.update(cx, |this, cx| this.stage_distribute(cx));
                })
                .agent_node(Role::Button, "Distribute")
                .agent_disabled(!ready)
        });
    if let Some(placed) = view.placed {
        card = card.child(
            div()
                .text_size(px(10.0))
                .text_color(ladder::status_ok())
                .child(format!("Placed {placed} fixtures"))
                .agent_node(Role::Text, format!("Placed {placed} fixtures")),
        );
    }
    if let Some(fit) = &view.fit {
        card = card
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(ladder::status_warn())
                    .child(fit.clone())
                    .agent_node(Role::Text, fit.clone()),
            )
            .child(
                luma_ui::luma_button("Extend and retry", Enabled::Yes)
                    .id("stage-extend-retry")
                    .on_click(move |_, _, cx| {
                        retry.update(cx, |this, cx| this.stage_extend_and_retry(cx));
                    })
                    .agent_node(Role::Button, "Extend and retry"),
            );
    }
    card.into_any_element()
}

/// The length controls for a run in hand: type it, step it, commit it.
///
/// They live in the page rather than over the picture because a length is a
/// number, and a number wants a field beside its readout — the picture carries
/// the line, the page carries the value.
pub(crate) fn run_controls(view: &StageView, app: &Entity<Luma>) -> AnyElement {
    let Some(length) = view.length_m else {
        return div().into_any_element();
    };
    let step = |label: &'static str, delta: f64| {
        let app = app.clone();
        luma_ui::luma_button(label, Enabled::Yes)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    this.stage_set_length((length + delta).max(hand::LENGTH_STEP_M), cx);
                });
            })
            .agent_node(Role::Button, label)
    };
    let commit = app.clone();
    let cancel = app.clone();
    div()
        .flex_shrink_0()
        .px(px(18.0))
        .py(px(6.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            div()
                .text_size(px(11.0))
                .text_color(ladder::foreground_90())
                .child(format!("Length {length:.2} m"))
                .agent_node(Role::Text, format!("Length {length:.2} m")),
        )
        .child(step("Shorter", -hand::LENGTH_STEP_M))
        .child(step("Longer", hand::LENGTH_STEP_M))
        .child({
            let refused = view.refusal.is_some();
            luma_ui::luma_button(
                "Place run",
                if refused { Enabled::No } else { Enabled::Yes },
            )
            .id("stage-place-run")
            .on_click(move |_, _, cx| {
                commit.update(cx, |this, cx| this.stage_commit_run(cx));
            })
            .agent_node(Role::Button, "Place run")
            .agent_disabled(refused)
        })
        .child(
            luma_ui::luma_button("Cancel run", Enabled::Yes)
                .id("stage-cancel-run")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, |this, cx| this.stage_cancel(cx));
                })
                .agent_node(Role::Button, "Cancel run"),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// The layer over the picture
// ---------------------------------------------------------------------------

/// One socket, projected.
pub(crate) struct Bead {
    pub(crate) node: String,
    pub(crate) socket: String,
    pub(crate) label: String,
    pub(crate) at: Point<Pixels>,
    pub(crate) state: luma_render::scene_desc::SocketMarkState,
}

/// Every socket worth pointing at, in window-relative pixels.
///
/// Projection here rather than in the picture is what makes a socket
/// *clickable*: a bead drawn by the renderer is a triangle with no hitbox, and
/// a hitbox is what both a hand and a script need. The renderer draws the same
/// beads for the eye ([`luma_render::scene_desc::SocketMark`]); this is the
/// half a pointer can reach, and both read the same room, so they cannot
/// disagree about where a joint is.
pub(crate) fn beads(build: &Build, camera: &luma_scene::Camera, size: (f32, f32)) -> Vec<Bead> {
    use luma_render::scene_desc::SocketMarkState;
    let (width, height) = size;
    if width <= 1.0 || height <= 1.0 {
        return Vec::new();
    }
    let aspect = width / height;
    let held = build.held_sockets();
    let latched = build
        .hand
        .held()
        .and_then(|held| held.latched.as_ref())
        .cloned();
    let forward = (camera.target - camera.position()).normalize_or_zero();
    let mut out = Vec::new();
    for (node, socket) in build.room.open_sockets() {
        if !hand::can_host(socket) {
            continue;
        }
        let Some(at) = build.room.socket_world(node, &socket.name) else {
            continue;
        };
        let world = coords::world_from_three(at.as_vec3());
        if (world - camera.position()).dot(forward) <= 0.0 {
            continue;
        }
        let ndc = camera.project(world, aspect);
        if ndc.x.abs() > 1.2 || ndc.y.abs() > 1.2 {
            continue;
        }
        let state = if latched
            .as_ref()
            .is_some_and(|m| m.host_id.as_deref() == Some(node) && m.host_socket == socket.name)
        {
            SocketMarkState::Latched
        } else if !held.is_empty() && hand::compatible(socket, &held) {
            SocketMarkState::Compatible
        } else {
            SocketMarkState::Open
        };
        out.push(Bead {
            node: node.to_string(),
            socket: socket.name.clone(),
            label: format!("Socket {} {}", build.label_of(node), socket.name),
            at: Point::new(
                px((ndc.x * 0.5 + 0.5) * width),
                px((1.0 - (ndc.y * 0.5 + 0.5)) * height),
            ),
            state,
        });
    }
    out
}

/// Where a window-space point meets the floor, in the socket layer's frame.
///
/// The builder's fallback target. A mesh raycast would be better over a deck,
/// and is what the picture's own hit test does; this is the answer that exists
/// with no rendered frame, which is the state a headless run is always in.
fn cursor_world(
    camera: &luma_scene::Camera,
    origin: Point<Pixels>,
    size: (f32, f32),
    at: Point<Pixels>,
) -> Option<glam::DVec3> {
    let (width, height) = size;
    if width <= 1.0 || height <= 1.0 {
        return None;
    }
    let local = glam::Vec2::new(f32::from(at.x - origin.x), f32::from(at.y - origin.y));
    let ndc = glam::Vec2::new(local.x / width * 2.0 - 1.0, 1.0 - local.y / height * 2.0);
    hand::floor_point(&camera.ray(ndc, width / height))
}

/// The clickable half of the builder, laid over the viewport.
///
/// It mounts whether or not the renderer is running, because the claims it
/// carries — which sockets are open, which one the ghost is stuck to — are
/// facts about the room and not about the picture, and a builder whose only
/// evidence was pixels would have none at all under the headless harness.
pub(crate) fn build_layer(
    build: &Build,
    camera: &luma_scene::Camera,
    origin: Point<Pixels>,
    size: (f32, f32),
    app: &Entity<Luma>,
) -> AnyElement {
    use luma_render::scene_desc::SocketMarkState;
    let mut layer = div().absolute().inset_0();
    // A held piece owns the pointer: the surface takes the press so an orbit
    // cannot start under a ghost, and it is not mounted at all when the hand
    // is empty (`luma_ui::pane`'s rule, applied to a layer).
    if build.hand.owns_pointer() {
        let drop = app.clone();
        let aim = app.clone();
        let camera = *camera;
        let (width, height) = size;
        layer = layer.child(
            div()
                .id("stage-drop-surface")
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_move(move |event, _, cx| {
                    let Some(world) =
                        cursor_world(&camera, origin, (width, height), event.position)
                    else {
                        return;
                    };
                    aim.update(cx, |this, cx| this.stage_aim(world, cx));
                })
                .on_click(move |event, _, cx| {
                    let at = cursor_world(&camera, origin, (width, height), event.position());
                    drop.update(cx, |this, cx| {
                        if let Some(world) = at {
                            this.stage_aim(world, cx);
                        }
                        this.stage_drop(cx);
                    });
                })
                .agent_node(Role::Card, "Stage drop surface"),
        );
    }
    for bead in beads(build, camera, size) {
        let app = app.clone();
        let (node, socket) = (bead.node.clone(), bead.socket.clone());
        let colour = match bead.state {
            SocketMarkState::Open => ladder::muted_foreground(),
            SocketMarkState::Compatible => ladder::accent(),
            SocketMarkState::Latched => ladder::foreground(),
        };
        let diameter = hand::SOCKET_MARK_PICK_PX * 2.0;
        layer = layer.child(
            div()
                .id(gpui::SharedString::from(format!("bead:{node}:{socket}")))
                .absolute()
                .left(bead.at.x - px(hand::SOCKET_MARK_PICK_PX))
                .top(bead.at.y - px(hand::SOCKET_MARK_PICK_PX))
                .w(px(diameter))
                .h(px(diameter))
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(
                    div()
                        .w(px(7.0))
                        .h(px(7.0))
                        .rounded(px(3.5))
                        .bg(colour)
                        .border_1()
                        .border_color(ladder::control_border()),
                )
                .on_click(move |_, _, cx| {
                    let (node, socket) = (node.clone(), socket.clone());
                    app.update(cx, |this, cx| this.stage_socket_clicked(node, socket, cx));
                })
                .agent_node(Role::Button, bead.label),
        );
    }
    layer.into_any_element()
}

/// Push the builder's shapes into the scene the renderer draws.
///
/// The eye's half of the same two facts the element layer carries: the ghost
/// and the measurement. Written straight onto `scene.editor` so the idle gate
/// sees them — `IdleKey` compares the whole editor, so a ghost that moved is a
/// frame the stage owes.
pub(crate) fn install(build: &Build, editor: &mut luma_render::scene_desc::Editor) {
    use luma_render::scene_desc::{Build as SceneBuild, Ghost, Measure, SocketMark};
    let mut out = SceneBuild::default();
    if let Some(held) = build.hand.held() {
        if let Some(landed) = &held.landed {
            if let Some(catalog_ref) = held.what.catalog_ref() {
                if let Some(geometry) = geometry_of(catalog_ref, &build.sockets) {
                    let (pos, rot) = luma_scene::coords::data_pose_of_d(landed.world);
                    out.ghost = Some(Ghost {
                        geometry,
                        pos: pos.map(|v| v as f32),
                        rot: rot.map(|v| v as f32),
                        scale: 1.0,
                        refused: landed.refused.is_some(),
                    });
                }
            }
        }
    }
    if let Some(run) = build.hand.extending() {
        if let Some(from) = build.room.socket_world(&run.from_node, &run.from_socket) {
            let socket = build.room.socket(&run.from_node, &run.from_socket);
            let pose = build.room.pose(&run.from_node);
            if let (Some(socket), Some(pose)) = (socket, pose) {
                let direction = pose.transform_vector3(socket.normal).normalize_or_zero();
                let to = from + direction * run.length_m;
                out.measure = Some(Measure {
                    from: point_of(from),
                    to: point_of(to),
                    refused: run.refused().is_some(),
                });
            }
        }
    }
    let held = build.held_sockets();
    out.sockets = build
        .room
        .open_sockets()
        .filter(|(_, socket)| hand::can_host(socket))
        .filter_map(|(node, socket)| {
            let at = build.room.socket_world(node, &socket.name)?;
            let pose = build.room.pose(node)?;
            Some(SocketMark {
                pos: point_of(at),
                normal: point_of(pose.transform_vector3(socket.normal).normalize_or_zero()),
                state: if !held.is_empty() && hand::compatible(socket, &held) {
                    luma_render::scene_desc::SocketMarkState::Compatible
                } else {
                    luma_render::scene_desc::SocketMarkState::Open
                },
            })
        })
        .collect();
    editor.build = out;
}

/// A socket-layer point as the data-space triple `scene_desc` carries.
fn point_of(at: glam::DVec3) -> [f32; 3] {
    let data = luma_scene::coords::data_pose_of_d(glam::DMat4::from_translation(at)).0;
    data.map(|v| v as f32)
}

/// The geometry a catalog entry draws as, at the palette's own defaults.
fn geometry_of(
    catalog_ref: &str,
    _sockets: &VenueSockets,
) -> Option<luma_render::scene_desc::Geometry> {
    use luma_render::scene_desc::Geometry;
    match luma_scene::catalog::piece(catalog_ref)?.geometry {
        luma_scene::catalog::Geometry::Mesh { path } => Some(Geometry::mesh(path)),
        luma_scene::catalog::Geometry::Procedural(family) => Some(Geometry::Procedural(
            luma_render::catalog::default_params(family),
        )),
    }
}
