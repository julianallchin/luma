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
use gpui::{div, px, AnyElement, Context, Div, Entity, Pixels, Point, Window};
use gpui_component::{Icon, IconName};
use luma_lib::models::venue_graph::{PlacementReport, ResolvedVenue};
use luma_render::catalog::VenueSockets;
use luma_scene::catalog::{pieces, PaletteGroup};
use luma_scene::coords;
use luma_scene::venue::{NodeKind, NodeSockets as _, VenueGraph};
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::fixture_library::{self, FixtureLibrary};
use crate::library::{LibraryError, Rig};
use crate::Luma;

use hand::{Extending, Hand, Held, Holding, Landed, Landing, Room};

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
    /// Where the selection card is parked, in the socket layer's world space.
    /// Written only by [`Build::select`].
    pub(crate) card_anchor: Option<glam::DVec3>,
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
            card_anchor: None,
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
    pub(crate) fn set_fixture_facts(&mut self, mode: Option<String>, width: f64) {
        let what = match &mut self.hand {
            Hand::Placing(held) => &mut held.what,
            Hand::Configuring(row) => &mut row.what,
            Hand::Idle | Hand::Choosing(_) | Hand::Extending(_) => return,
        };
        if let Holding::Fixture {
            mode: slot,
            width_m,
            ..
        } = what
        {
            *slot = mode;
            *width_m = width;
        }
    }

    /// Re-solve the row being configured, in place.
    ///
    /// The preview and the commit share both halves of the arithmetic —
    /// [`luma_render::face::host_face`] for how long the face is, and
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
            .and_then(|node| luma_render::face::host_face(&self.sockets, node, &row.host_socket))
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

    /// How wide the inspector should be, from the state that decides it.
    ///
    /// Derived rather than stored so the four places that change the selection
    /// do not each have to remember to open the sheet — the classic "caller
    /// must call A before B", answered by not having a B.
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
            Holding::Unplaced { .. } | Holding::Fixture { .. } => {
                vec![luma_render::catalog::fixture_clamp()]
            }
        }
    }

    /// The held thing's local bounds, for the collision arm of a landing.
    /// `None` — a fixture, an unfetched definition — collides with nothing.
    fn held_bounds(&self) -> Option<luma_scene::aabb::DAabb> {
        let held = self.hand.held()?;
        match &held.what {
            Holding::Piece {
                catalog_ref,
                params,
                ..
            } => match luma_scene::catalog::piece(catalog_ref)?.geometry {
                luma_scene::catalog::Geometry::Mesh { .. } => {
                    self.sockets.catalog().bounds(catalog_ref)
                }
                luma_scene::catalog::Geometry::Procedural(family) => {
                    Some(luma_render::catalog::procedural_bounds(
                        luma_render::catalog::node_params(family, &params_of(params)),
                    ))
                }
            },
            Holding::Duplicate { root, .. } => self.room.bounds_of(root),
            Holding::Unplaced { .. } | Holding::Fixture { .. } => None,
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

    /// Whether a transform gizmo applies to what is selected right now.
    ///
    /// The design's rule reaching the one control that draws it: only a piece
    /// sitting free on the venue's own floor has axes to drag, so a snapped
    /// piece — or nothing selected at all — is a state the Translate/Rotate
    /// track does not apply to, and a control that does not apply is not drawn.
    /// See [`Room::pick`]. Ray in the socket layer's world space.
    pub(crate) fn room_pick(&self, origin: glam::DVec3, dir: glam::DVec3) -> Option<String> {
        self.room.pick(origin, dir)
    }

    /// A node's position in the socket layer's world space, for whoever needs
    /// a point to aim a camera at.
    pub(crate) fn room_pose_point(&self, node: &str) -> Option<glam::DVec3> {
        self.room.pose(node).map(|pose| pose.w_axis.truncate())
    }

    /// The one writer of `selected`. Freezes where the selection card sits —
    /// the piece's position at selection time — so a trim or span drag moves
    /// the piece and not the control being dragged. Re-selecting the same
    /// node is a no-op for the same reason: every verb re-adopts the room,
    /// and an adopt mid-scrub that re-anchored the card would put the jitter
    /// back.
    pub(crate) fn select(&mut self, node: Option<String>) {
        if self.selected == node {
            return;
        }
        self.card_anchor = node
            .as_ref()
            .and_then(|node| self.room.pose(node))
            .map(|pose| pose.w_axis.truncate());
        self.selected = node;
        self.trim_draft = None;
    }

    /// The selected node as the page draws it — label, freedoms, relation.
    /// One projection shared by the tab view and the in-scene card.
    pub(crate) fn selected_view(&self) -> Option<SelectedView> {
        let node = self.selected.as_ref()?;

        let freedom = self.freedom_of(node);
        let param = |key: &str| {
            self.solved
                .nodes
                .iter()
                .find(|n| &n.id == node)
                .and_then(|n| n.params.get(key).copied())
                .unwrap_or(0.0)
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
                Some("yaw") => self
                    .graph
                    .edge(node)
                    .map_or(0.0, |edge| edge.roll.to_degrees().rem_euclid(360.0)),
                Some(key) => param(key),
                None => 0.0,
            },
            span: self
                .solved
                .nodes
                .iter()
                .find(|n| &n.id == node)
                .and_then(|n| n.params.get("span").copied()),
            angle: {
                let hinge = self
                    .graph
                    .node(node)
                    .and_then(|n| n.catalog_ref.as_deref())
                    .and_then(luma_scene::catalog::piece)
                    .is_some_and(|piece| {
                        matches!(
                            piece.geometry,
                            luma_scene::catalog::Geometry::Procedural(
                                luma_scene::catalog::Family::Hinge
                            )
                        )
                    });
                hinge.then(|| {
                    self.solved
                        .nodes
                        .iter()
                        .find(|n| &n.id == node)
                        .and_then(|n| n.params.get("angle").copied())
                        .unwrap_or(f64::from(luma_render::catalog::DEFAULT_HINGE_ANGLE_DEG))
                })
            },
        })
    }

    pub(crate) fn gizmo_offered(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|node| self.freedom_of(node) == Freedom::Free)
    }

    /// Whether this node's open sockets wear beads right now.
    ///
    /// The room at rest is a picture, not a control surface: beads appear when
    /// the hand needs a joint to aim at — every candidate while something is
    /// held or a run is being measured — and, at rest, only on the selected
    /// piece, which is how an extend is begun. Choosing and configuring draw
    /// their own affordances and no beads at all.
    pub(crate) fn socket_shown(&self, node: &str) -> bool {
        match &self.hand {
            Hand::Placing(_) | Hand::Extending(_) => true,
            Hand::Choosing(_) | Hand::Configuring(_) => false,
            Hand::Idle => self.selected.as_deref() == Some(node),
        }
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
    /// The parameter this freedom edits, when it edits one.
    pub(crate) fn param(self) -> Option<&'static str> {
        match self {
            Freedom::Slide => Some("u"),
            Freedom::Roll => Some("yaw"),
            Freedom::Free | Freedom::Bolted | Freedom::Unplaced => None,
        }
    }
}

/// Where one body of a row sits, `u` metres along the face from its middle.
///
/// The face's own frame, in the host's: tangent forward, normal up — which is
/// what makes "hanging under" and "standing on" the same call with a different
/// socket, exactly as [`luma_render::face::HostFace`] describes.
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

// ---------------------------------------------------------------------------
// The tab body
// ---------------------------------------------------------------------------

/// The stage tab. Almost stateless: what a builder is doing lives on the
/// [`Build`] beside the picture, and this is the venue the tab dies with.
pub(crate) struct StagePage {
    pub(crate) venue_name: String,
    /// The one search the add-element dialog has, and the fixture rows it
    /// fetched. The shared picker rather than a private query: the bundle is
    /// fifteen thousand definitions, so "what fixtures match this" is paged,
    /// generation-guarded and error-reporting wherever it is asked, and the
    /// stage page asking it a second way was the duplicate.
    ///
    /// Its field is the dialog's *only* field: [`FixtureLibrary::query`] is
    /// what the catalog and unplaced sections are filtered by too, so one
    /// string narrows all three provenances.
    pub(crate) library: FixtureLibrary,
    /// The focus the dialog traps while it is up.
    pub(crate) focus: gpui::FocusHandle,
    /// Where the node menu is open, if it is — and which bead opened it, so
    /// the menu can offer that socket's own verbs. On the page and not the
    /// hand: a menu is chrome about the selection, not a thing the hand is
    /// doing.
    pub(crate) menu: Option<NodeMenuAt>,
    /// The add-element dialog's exit. The hand decides *whether* the dialog is
    /// up ([`Hand::Choosing`]); this only buys the card its leaving frames —
    /// see [`luma_ui::dialog::Popup`].
    pub(crate) closing: luma_ui::dialog::Popup<()>,
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
        let page = StagePage {
            venue_name,
            library: FixtureLibrary::new("Search elements", cx, |luma, query, cx| {
                luma.stage_fixture_query(query, cx);
            }),
            focus: cx.focus_handle(),
            menu: None,
            closing: luma_ui::dialog::Popup::default(),
        };
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
                            // Warnings only. A report is what the *graph* now
                            // says, and `PlacementReport::outcome` is a fact
                            // about the node rather than a verdict on the call
                            // — `detach` answers `Unplaced` because that is
                            // what it was asked to do. A refusal is the `Err`
                            // arm below and cannot arrive here at all.
                            build.report = report.warnings.clone();
                            // What the verb touched is what the inspector
                            // should be about: the piece just placed is the one
                            // about to be trimmed, flipped or detached. A
                            // branch that has just *left* the room is not — a
                            // sheet open on a detached wing is a sheet about
                            // nothing, and `Unplaced` is exactly what `detach`
                            // was asked for.
                            build
                                .select(report.outcome.is_placed().then(|| report.node_id.clone()));
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

    /// Open the add-element dialog.
    /// Open the chooser and put the caret in its search field — the button's
    /// and the `A` key's shared path. Search-first is the whole dialog: it
    /// opens ready to be typed at.
    pub(crate) fn stage_open_chooser_focused(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.stage_open_chooser(cx);
        if let Some(page) = self.stage_page_mut() {
            let field = page.library.field().clone();
            let handle = gpui::Focusable::focus_handle(field.read(cx), cx);
            window.focus(&handle, cx);
        }
    }

    pub(crate) fn stage_open_chooser(&mut self, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Choosing(hand::Choosing::default());
        }
        if let Some(page) = self.stage_page_mut() {
            page.closing.finish_close();
            page.closing.open(());
        }
        // Opened on a fresh question. A dialog that came back holding the last
        // search would be answering one nobody asked — and the field is the
        // page's, not the hand's, so nothing else clears it.
        if let Some(page) = self.stage_page_mut() {
            page.library.set_query(String::new());
            let field = page.library.field().clone();
            field.update(cx, |field, cx| field.set_text("", cx));
        }
        self.stage_fetch_fixture_page(cx);
        cx.notify();
    }

    /// The stage tab the dialog belongs to — the active one, because the hand
    /// it arms is the one beside the picture and there is only ever one of
    /// those.
    /// The stage page wherever it lives, for the shell's per-frame popup
    /// reap — the exit must finish even if another tab has focus.
    pub(crate) fn stage_page_for_tick(&mut self) -> Option<&mut StagePage> {
        self.stage_page_mut()
    }

    fn stage_page_mut(&mut self) -> Option<&mut StagePage> {
        match self.workspace.active_body_mut()? {
            crate::shell::Body::Stage(page) => Some(page),
            _ => None,
        }
    }

    /// A keystroke in the dialog's field.
    fn stage_fixture_query(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(page) = self.stage_page_mut() {
            page.library.set_query(query);
        }
        self.stage_fetch_fixture_page(cx);
        cx.notify();
    }

    /// Ask the bundle for the next page of fixture rows, if there is one.
    fn stage_fetch_fixture_page(&mut self, cx: &mut Context<Self>) {
        // Two disjoint fields of `self`: the tab's browsing state, and the
        // command seam it asks through.
        let Some(crate::shell::Body::Stage(page)) = self.workspace.active_body_mut() else {
            return;
        };
        let generation = page.library.generation();
        let Some(pending) = page.library.page(&self.library) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let page = pending.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.stage_page_mut() {
                    state.library.landed(generation, page);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Take a row out of the dialog and into the hand.
    ///
    /// A library fixture arrives without its mode or its measurements — the
    /// definition is a file read — so the hand takes it immediately and the
    /// two facts land when they land. What the hand *cannot* do until they do
    /// is commit, which is what [`ConfigureView::ready`] gates.
    pub(crate) fn stage_take(&mut self, what: Holding, cx: &mut Context<Self>) {
        if let Some(page) = self.stage_page_mut() {
            page.closing.begin_close(cx);
        }
        let path = match &what {
            Holding::Fixture { path, .. } => Some(path.clone()),
            _ => None,
        };
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Placing(Held::new(what));
        }
        if let Some(path) = path {
            let pending = self.library.fixture_definition(&path);
            cx.spawn(async move |this, cx| {
                let Ok(definition) = pending.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    let mode = definition.modes.first().map(|mode| mode.name.clone());
                    let width = luma_lib::services::distribute::body_width_m(&definition);
                    if let Some(build) = this.build_mut() {
                        build.set_fixture_facts(mode, width);
                        build.repreview();
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Open the node menu where the pointer is.
    pub(crate) fn stage_open_menu(
        &mut self,
        at: Point<Pixels>,
        node: String,
        socket: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(crate::shell::Body::Stage(page)) = self.workspace.active_body_mut() {
            page.menu = Some(NodeMenuAt { at, node, socket });
        }
        cx.notify();
    }

    /// Close the node menu.
    pub(crate) fn stage_close_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(crate::shell::Body::Stage(page)) = self.workspace.active_body_mut() {
            page.menu = None;
        }
        cx.notify();
    }

    /// `Escape`: one rung down the flow, then the selection.
    /// Select whatever placed piece is under `at` — the headless twin of the
    /// viewport click, writing the same two stores the same way.
    pub(crate) fn stage_pick_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        let Some(ray) = state.stage_pick_ray(at) else {
            return;
        };
        let origin = luma_scene::coords::three_from_world(ray.origin).as_dvec3();
        let dir = luma_scene::coords::three_from_world(ray.dir)
            .as_dvec3()
            .normalize_or_zero();
        let picked = state
            .build
            .as_ref()
            .and_then(|build| build.room_pick(origin, dir));
        match picked.clone() {
            Some(id) => state.select_object(luma_render::frame::EditorObject::StagePiece(id)),
            None => state.clear_selection(),
        }
        if let Some(build) = state.build.as_mut() {
            build.select(picked);
        }
        cx.notify();
    }

    pub(crate) fn stage_escape(&mut self, cx: &mut Context<Self>) {
        self.stage_close_menu(cx);
        let mut was_choosing = false;
        if let Some(build) = self.build_mut() {
            was_choosing = matches!(build.hand, Hand::Choosing(_));
            build.hand = std::mem::take(&mut build.hand).escape();
            // Escape empties the hand *and* the head: whatever mode it left,
            // nothing stays selected — coming out of place mode with the last
            // selection still lit read as the editor holding a grudge.
            build.select(None);
        }
        if let Some(state) = self.visualizer_mut() {
            state.clear_selection();
        }
        if was_choosing {
            if let Some(page) = self.stage_page_mut() {
                page.closing.begin_close(cx);
            }
        }
        cx.notify();
    }

    /// The cursor moved over the room while something is held. `hit` is the
    /// mesh face under it, when a rendered frame had one to give.
    pub(crate) fn stage_aim(
        &mut self,
        world: glam::DVec3,
        hit: Option<hand::SurfaceHit>,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let held = build.held_sockets();
        let body = build.held_bounds();
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
            hit.as_ref(),
            exclude.as_deref(),
            body,
        );
        if let Hand::Placing(held) = &mut build.hand {
            held.cursor = Some((world, hit));
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

    /// Edit one parameter of the thing in the hand — the span scrub a held
    /// truss offers. The landing re-solves from the remembered cursor, so the
    /// ghost grows in place instead of waiting for the pointer to move.
    pub(crate) fn stage_set_held_param(&mut self, key: &str, value: f64, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let Hand::Placing(held) = &mut build.hand else {
            return;
        };
        let Holding::Piece { params, .. } = &mut held.what else {
            return;
        };
        params.insert(key.to_string(), value);
        // A changed parameter moves the sockets, so last frame's latch may
        // describe a joint the piece no longer reaches.
        held.latched = None;
        let cursor = held.cursor.clone();
        match cursor {
            Some((world, hit)) => self.stage_aim(world, hit, cx),
            None => cx.notify(),
        }
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
        self.stage_aim(at, None, cx);
    }

    /// Aim the hand at whatever the pointer is over: the picture's own mesh
    /// hit when a frame is up, the floor plane when none is.
    pub(crate) fn stage_aim_from_pointer(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((world, hit)) = self
            .visualizer
            .as_ref()
            .and_then(|state| state.stage_cursor(at))
        else {
            return;
        };
        self.stage_aim(world, hit, cx);
    }

    /// A click on the room while the hand is placing: aim there, then commit.
    ///
    /// The viewport's release handler calls this for a *click* — a left drag
    /// is the camera's, which is what makes an orbit available with a ghost
    /// in the air.
    pub(crate) fn stage_click_room(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self
            .build_state()
            .is_some_and(|build| build.hand.aims_with_pointer())
        {
            return;
        }
        self.stage_aim_from_pointer(at, cx);
        self.stage_drop(at, cx);
    }

    /// Release: commit whatever the ghost is standing on.
    ///
    /// `at` is where the release landed in window pixels, which only a fixture
    /// needs: it is the point the configure popover anchors to.
    pub(crate) fn stage_drop(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(build) = self.build_mut() else {
            return;
        };
        if build.committing {
            return;
        }
        let venue = build.venue_id.clone();
        let root = build.room.root().to_string();
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
        // A library fixture reaches the room only through the configure
        // popover — it is created as a *row*, and a row needs a face to lie
        // along. Which face is whatever the ghost is standing on: the bead is
        // how a *named* socket is aimed at, but the floor and a truss's
        // underside are areas, and a face a person can point at is a face they
        // can distribute along. Landing on something that is not a feature —
        // a bolt circle, a clamp — leaves the ghost in the hand, because "one
        // of them there" is not what a row means.
        if matches!(what, Holding::Fixture { .. }) {
            let (node, socket) = match &landed.how {
                Landing::Free { surface, .. } => surface
                    .clone()
                    .unwrap_or((root, luma_scene::venue::FLOOR_SOCKET.to_string())),
                Landing::Socket {
                    parent,
                    their_socket,
                    ..
                } => (parent.clone(), their_socket.clone()),
            };
            if build
                .room
                .socket(&node, &socket)
                .is_some_and(hand::is_feature)
            {
                self.stage_configure(what, node, socket, at, cx);
            }
            return;
        }
        // Place mode is sticky, and only for a catalog piece: that is a stamp,
        // and an operator rigging a row of them should not have to walk back
        // to the dialog between each. A tray fixture and a duplicate are one
        // specific thing each — once placed there is no second one to hold.
        build.hand = match &what {
            Holding::Piece { .. } => Hand::Placing(Held::again(&what)),
            Holding::Duplicate { .. } | Holding::Unplaced { .. } | Holding::Fixture { .. } => {
                Hand::Idle
            }
        };
        let pending: Verb = match (&what, &landed.how) {
            // Refused above: a fixture is created as a row, never dropped.
            (Holding::Fixture { .. }, _) => return,
            (
                Holding::Unplaced { node, .. },
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
                Holding::Unplaced { node, .. },
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
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let feature = self
            .build_state()
            .and_then(|build| build.room.socket(&node, &socket))
            .is_some_and(hand::is_feature);
        if let Some(held) = self.build_state().and_then(|build| build.hand.held()) {
            // A fixture meeting a face is a *row*, not one light: that is the
            // gesture the whole configure surface exists for. Structure meeting
            // the same face is still one piece — a truss lands where it is
            // pointed.
            if feature && held.what.kind() == NodeKind::Fixture {
                let what = held.what.clone();
                self.stage_configure(what, node, socket, at, cx);
                return;
            }
            self.stage_aim_socket(&node, &socket, cx);
            self.stage_drop(at, cx);
            return;
        }

        // At rest a bead is an aiming affordance and a menu anchor. Placing
        // starts only in the add-element dialog, and a run starts only from
        // the bead's own menu — a bare click that armed a whole mode was the
        // "what is this Place run thing" surprise.
    }

    /// Start measuring a run out of `socket` — the node menu's "Extend run".
    pub(crate) fn stage_extend_from(
        &mut self,
        node: String,
        socket: String,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        // The measurement is the verb's, not a ray of the page's own: `extend`
        // refuses against exactly this number, and a second cast here could
        // offer a length the command would then reject. Asked once, on the
        // click — the answer is a property of the room, and re-casting it while
        // a length is being typed would move the number under the operator.
        let venue = build.venue_id.clone();
        build.hand = Hand::Extending(Box::new(Extending {
            from_node: node.clone(),
            from_socket: socket.clone(),
            reach: None,
            length_m: hand::STUB_LENGTH_M,
        }));
        let measuring = self.library.extend_reach(&venue, &node, &socket);
        cx.spawn(async move |this, cx| {
            let reach = measuring.await.ok().flatten();
            this.update(cx, |this, cx| {
                if let Some(Hand::Extending(run)) = this.build_mut().map(|b| &mut b.hand) {
                    // Only if the same run is still in hand: the operator may
                    // have escaped or clicked elsewhere while this was in
                    // flight, and a measurement for a socket nobody is pointing
                    // at is worse than none.
                    if run.from_node == node && run.from_socket == socket {
                        run.length_m = reach.as_ref().map_or(hand::STUB_LENGTH_M, |r| r.gap_m);
                        run.reach = reach;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Type or drag a length for the run in hand.
    pub(crate) fn stage_set_length(&mut self, metres: f64, cx: &mut Context<Self>) {
        if let Some(Hand::Extending(run)) = self.build_mut().map(|build| &mut build.hand) {
            run.length_m = hand::quantize(metres);
        }
        cx.notify();
    }

    /// Commit the run.
    ///
    /// One verb: `extend` re-measures the gap, refuses a length past it, and
    /// writes the far-end check itself when the run bridges — so the page does
    /// not compose an attach and a constrain of its own, and the Python facade
    /// and this button cannot disagree about what an extend is.
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
        let socket = run.from_socket.clone();
        let length = run.length_m;
        build.hand = Hand::Idle;
        let pending = self.library.extend(&venue, &parent, &socket, Some(length));
        self.stage_verb(Box::pin(pending), cx);
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
        build.hand = Hand::Placing(Held::new(Holding::Duplicate {
            root,
            display_name,
            flip: false,
        }));
        cx.notify();
    }

    /// Invert the handedness of the copy in hand.
    pub(crate) fn stage_flip(&mut self, cx: &mut Context<Self>) {
        if let Some(Hand::Placing(held)) = self.build_mut().map(|build| &mut build.hand) {
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

    /// Write dragged pieces' previewed poses into the graph.
    ///
    /// The gizmo previewed by writing the scene; a piece's position *lives* in
    /// its joint, so the release inverts the mate — the same
    /// [`luma_scene::venue::invert_placement`] the drop path uses — and writes
    /// `(u, v, yaw, trim)` through the ordinary verb. The re-solve then hands
    /// back the pose that was previewed, read out of the graph.
    pub(crate) fn stage_commit_pose(&mut self, pieces: Vec<String>, cx: &mut Context<Self>) {
        let Some(venue) = self.build_state().map(|build| build.venue_id.clone()) else {
            return;
        };
        let jobs: Vec<_> = pieces
            .iter()
            .filter_map(|id| self.gizmo_seat(id).map(|params| (id.clone(), params)))
            .collect();
        for (node, params) in jobs {
            let pending = self.library.set_params(&venue, &node, params, None);
            self.stage_verb(pending, cx);
        }
    }

    /// A dragged piece's previewed pose as the seat its joint would write.
    fn gizmo_seat(&self, node: &str) -> Option<BTreeMap<String, f64>> {
        let build = self.build_state()?;
        let world = self.visualizer.as_ref()?.piece_pose_three(node)?;
        let edge = build.graph.edge(node)?;
        let host = build.room.socket(&edge.parent, &edge.their_socket)?;
        let parent_world = build.room.pose(&edge.parent)?;
        let data = build.graph.node(node)?;
        let held_sockets = build.sockets.sockets(data);
        let held = held_sockets.iter().find(|s| s.name == edge.my_socket)?;
        let seat = luma_scene::venue::invert_placement(world, parent_world, host, held, data.kind);
        Some(BTreeMap::from([
            ("u".to_string(), seat.u),
            ("v".to_string(), seat.v),
            ("trim".to_string(), seat.trim.max(0.0)),
            ("yaw".to_string(), seat.yaw),
        ]))
    }

    /// Delete the selected node and everything hanging off it.
    ///
    /// Fixtures riding a deleted piece are trayed rather than destroyed —
    /// deletion is creation's dual (`delete_subtree`), so the patch survives
    /// the structure it was hung on.
    pub(crate) fn stage_delete(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        if build.committing {
            return;
        }
        let (Some(node), venue) = (build.selected.clone(), build.venue_id.clone()) else {
            return;
        };
        let pending = self.library.delete_subtree(&venue, &node);
        if let Some(build) = self.build_mut() {
            build.committing = true;
            build.select(None);
        }
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    if let Err(error) = &result {
                        build.report = vec![error.to_string()];
                    }
                }
                this.reload_stage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Start fitting a row of `what` to one face.
    ///
    /// The preview is solved immediately rather than on the first edit, so the
    /// popover opens over a picture that already answers "what would one look
    /// like here" — which is the question the count is an adjustment to.
    pub(crate) fn stage_configure(
        &mut self,
        what: Holding,
        node: String,
        socket: String,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(build) = self.build_mut() else {
            return;
        };
        let host_label = build.label_of(&node);
        build.hand = Hand::Configuring(Box::new(hand::Configuring {
            what,
            host_node: node,
            host_label,
            host_socket: socket,
            at,
            count: 1,
            layout: luma_scene::distribute::Layout::Even,
            preview: Ok(Vec::new()),
        }));
        build.repreview();
        cx.notify();
    }

    /// Change how many bodies the row has, and re-solve it.
    pub(crate) fn stage_set_count(&mut self, count: usize, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            if let Hand::Configuring(row) = &mut build.hand {
                row.count = count.max(1);
            }
            build.repreview();
        }
        cx.notify();
    }

    /// Change how the row is spread, and re-solve it.
    pub(crate) fn stage_set_layout(
        &mut self,
        layout: luma_scene::distribute::Layout,
        cx: &mut Context<Self>,
    ) {
        if let Some(build) = self.build_mut() {
            if let Hand::Configuring(row) = &mut build.hand {
                row.layout = layout;
            }
            build.repreview();
        }
        cx.notify();
    }

    /// Grow the host to the length the fit failure asked for, then re-solve.
    ///
    /// The one press that answers a refusal, and it is a *preview* press: the
    /// host really does get longer, because that is an edit to the room and
    /// the room is where a truss's span lives — but the row itself is still
    /// only previewed, so the operator sees the fit before committing to it.
    pub(crate) fn stage_extend_host(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let venue = build.venue_id.clone();
        let Some(row) = build.hand.configuring() else {
            return;
        };
        let Err(luma_scene::distribute::Fit::TooLong { needed_m, .. }) = row.preview else {
            return;
        };
        let host = row.host_node.clone();
        let pending = self.library.set_params(
            &venue,
            &host,
            BTreeMap::from([("span".to_string(), needed_m)]),
            None,
        );
        cx.spawn(async move |this, cx| {
            let _ = pending.await;
            this.update(cx, |this, cx| {
                this.reload_stage(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Commit the row the popover has been previewing.
    pub(crate) fn stage_apply_row(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.build_state() else {
            return;
        };
        let venue = build.venue_id.clone();
        let Some(row) = build.hand.configuring() else {
            return;
        };
        // A refused row has no poses, and a partial distribution is not one.
        if row.preview.is_err() {
            return;
        }
        let Holding::Fixture { path, mode, .. } = &row.what else {
            return;
        };
        let Some(mode) = mode.clone() else {
            return;
        };
        let pending = self.library.distribute(
            &venue,
            Some((row.host_node.as_str(), row.host_socket.as_str())),
            path,
            &mode,
            row.count,
            row.layout.into(),
            None,
        );
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Idle;
            build.committing = true;
        }
        cx.spawn(async move |this, cx| {
            let report = pending.await;
            this.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    build.committing = false;
                    build.report = match &report {
                        Ok(report) => report.warnings.clone(),
                        Err(error) => vec![error.to_string()],
                    };
                }
                // The row's own report carries no venue, so the room is re-read
                // rather than adopted — one more round trip, and the only verb
                // that needs it.
                this.reload_stage(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Commit a held duplicate.
    ///
    /// One verb: `duplicate` walks the same plan the ghost previewed and writes
    /// every copy in one call, so the page owns no loop of its own to drift
    /// from the one the Python facade calls.
    fn stage_duplicate_commit(
        &mut self,
        venue: &str,
        root: &str,
        flip: bool,
        how: &Landing,
        cx: &mut Context<Self>,
    ) {
        // A duplicate is placed on a socket: every compatible open socket
        // lights up precisely so the copy lands on a joint, and a copy seated
        // free would be a second, different gesture.
        let Landing::Socket {
            parent,
            their_socket,
            ..
        } = how
        else {
            return;
        };
        let pending = self
            .library
            .duplicate(venue, root, parent, their_socket, flip);
        self.stage_verb(Box::pin(pending), cx);
    }
}

/// One body a held thing would put in the room, ready to be drawn.
struct GhostBody {
    geometry: luma_render::scene_desc::Geometry,
    world: glam::DMat4,
    scale: f32,
}

use luma_lib::services::stage_ops::CopyStep;

/// One verb in flight. Boxed because the four placements are four different
/// futures of one shape, and the caller only ever awaits them.
type Verb = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<PlacementReport, LibraryError>> + Send>,
>;

impl Build {
    /// Every body the held thing would put in the room, at the pose it would
    /// put it.
    ///
    /// A ghost is a *preview*, so what it draws is what lands: a truss draws a
    /// truss, a fixture draws a housing, and a duplicated wing draws the whole
    /// wing. A single mark under the cursor is enough to know something is held
    /// and not enough to know what — which is the one question a ghost exists
    /// to answer, and the reason "no catalog entry" cannot mean "draw nothing".
    ///
    /// The copy's poses come from [`Self::copy_plan`] fed through the ordinary
    /// resolver, not from a transform composed here: flip is written in the
    /// rows, so a preview that reproduced it geometrically would be a second
    /// implementation of handedness that could disagree with the commit.
    fn ghost_bodies(&self, what: &Holding, landed: &Landed) -> Vec<GhostBody> {
        use luma_render::scene_desc::Geometry;
        match what {
            Holding::Piece {
                catalog_ref,
                params,
                ..
            } => geometry_with(catalog_ref, params)
                .map(|geometry| GhostBody {
                    geometry,
                    world: landed.world,
                    scale: 1.0,
                })
                .into_iter()
                .collect(),
            // A fixture's housing, in the shape the row preview already draws
            // one in ([`install`]'s stations): the ghost and the stations are
            // the same body seen before and after the count is set, and two
            // spellings of "a light goes here" would be one too many.
            Holding::Fixture { .. } | Holding::Unplaced { .. } => vec![GhostBody {
                geometry: Geometry::Procedural(luma_render::catalog::default_params(
                    luma_scene::catalog::Family::Corner,
                )),
                world: landed.world,
                scale: STATION_GHOST_SCALE,
            }],
            Holding::Duplicate { root, flip, .. } => self.copy_ghosts(root, *flip, landed),
        }
    }

    /// The duplicated subtree, solved where it would land.
    ///
    /// The plan is inserted into a *clone* of the graph under ids no venue can
    /// mint, and the whole thing is resolved. That is a solve per pointer
    /// sample while a copy is held — a depth-first walk over a rig the design
    /// bounds at ~500 nodes, against the alternative of a second, divergent
    /// notion of where the copy goes.
    fn copy_ghosts(&self, root: &str, flip: bool, landed: &Landed) -> Vec<GhostBody> {
        let Some(plan) = self.copy_plan(root, flip, &landed.how) else {
            return Vec::new();
        };
        let mut graph = self.graph.clone();
        let mut minted: HashMap<String, String> = HashMap::new();
        for (index, step) in plan.iter().enumerate() {
            let id = format!("{}{index}", hand::GHOST_NODE);
            let parent = if index == 0 {
                step.parent.clone()
            } else {
                match minted.get(&step.parent) {
                    Some(parent) => parent.clone(),
                    None => continue,
                }
            };
            let mut params = luma_scene::venue::Params::default();
            for (key, value) in &step.params {
                params.set(key.clone(), *value);
            }
            graph.insert_placed(
                luma_scene::venue::Node {
                    id: id.clone(),
                    kind: step.kind,
                    catalog_ref: step.catalog_ref.clone(),
                    label: step.label.clone(),
                    params,
                },
                luma_scene::venue::Edge {
                    parent,
                    my_socket: step.my_socket.clone(),
                    their_socket: step.their_socket.clone(),
                    roll: step.yaw,
                },
            );
            minted.insert(step.source.clone(), id);
        }
        let solved = luma_scene::venue::resolve(&graph, &self.sockets);
        minted
            .values()
            .filter_map(|id| {
                let pose = solved.pose(id)?;
                let geometry = pose
                    .catalog_ref
                    .as_deref()
                    .and_then(geometry_of)
                    .unwrap_or_else(|| {
                        luma_render::scene_desc::Geometry::Procedural(
                            luma_render::catalog::default_params(
                                luma_scene::catalog::Family::Corner,
                            ),
                        )
                    });
                Some(GhostBody {
                    geometry,
                    world: pose.world,
                    scale: if pose.catalog_ref.is_some() {
                        1.0
                    } else {
                        STATION_GHOST_SCALE
                    },
                })
            })
            .collect()
    }

    /// The `attach` list a duplicate of `root` would owe, landing on `how`.
    ///
    /// [`luma_lib::services::stage_ops::duplicate_plan`]'s answer, not a second
    /// one: the ghost previews exactly the rows the verb will write, so a
    /// flipped wing cannot draw one way and land another.
    fn copy_plan(&self, root: &str, flip: bool, how: &Landing) -> Option<Vec<CopyStep>> {
        // A duplicate is placed on a socket: every compatible open socket
        // lights up precisely so the copy lands on a joint, and a copy seated
        // free would be a second, different gesture.
        let Landing::Socket {
            parent,
            their_socket,
            ..
        } = how
        else {
            return None;
        };
        luma_lib::services::stage_ops::duplicate_plan(&self.graph, root, parent, their_socket, flip)
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
    /// The add-element dialog's keyboard cursor, when it is up.
    pub(crate) choosing: Option<usize>,
    pub(crate) selected: Option<SelectedView>,
    /// Unplaced fixtures, as dialog rows.
    pub(crate) unplaced: Vec<(String, String)>,
    pub(crate) configuring: Option<ConfigureView>,
}

/// The row being fitted, as the popover draws it.
pub(crate) struct ConfigureView {
    pub(crate) host: String,
    pub(crate) what: String,
    pub(crate) at: Point<Pixels>,
    pub(crate) count: usize,
    pub(crate) even: bool,
    /// How many bodies the preview placed, or why it placed none.
    pub(crate) fits: Result<usize, String>,
    /// The label of the one press that would make a refused row fit.
    pub(crate) offer: Option<String>,
    /// Whether the row can be committed — it fits and its mode has landed.
    pub(crate) ready: bool,
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
}

impl Luma {
    /// The builder as the page draws it, or `None` when no room is up.
    pub(crate) fn stage_view(&self) -> Option<StageView> {
        let build = self.build_state()?;
        let selected = build.selected_view();
        Some(StageView {
            choosing: build.hand.choosing().map(|c| c.cursor),
            selected,
            unplaced: build
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
            configuring: build.hand.configuring().map(|row| ConfigureView {
                host: format!("{} {}", row.host_label, row.host_socket),
                what: row.what.label().to_string(),
                at: row.at,
                count: row.count,
                even: matches!(row.layout, luma_scene::distribute::Layout::Even),
                fits: match &row.preview {
                    Ok(stations) => Ok(stations.len()),
                    Err(luma_scene::distribute::Fit::TooLong {
                        needed_m,
                        available_m,
                    }) => Err(format!(
                        "Needs {needed_m:.2} m; the face is {available_m:.2} m"
                    )),
                },
                // Named off the length it would set, not off a refusal's prose:
                // the button says what pressing it does, and the line above it
                // already says why it is there.
                offer: match &row.preview {
                    Err(luma_scene::distribute::Fit::TooLong { needed_m, .. }) => {
                        Some(format!("Extend to {needed_m:.2} m"))
                    }
                    Ok(_) => None,
                },
                ready: row.preview.is_ok()
                    && matches!(&row.what, Holding::Fixture { mode, .. } if mode.is_some()),
            }),
        })
    }
}

/// The tab body: the room, and whatever the hand is doing over it.
///
/// # Why nothing here is a panel
///
/// The room *is* the page. Every gesture the builder has is aimed at the
/// picture, so a strip of chrome that takes height off the viewport is spending
/// the one surface the work happens on. The page is therefore an overlay: a
/// transparent box the size of the tab that paints no background and does not
/// occlude, so a press that misses a control lands on the room — the only
/// sensible thing for it to land on.
///
/// # One surface per state
///
/// Everything below is a `match` on [`hand::Hand`]. That is the whole reason
/// the state machine swallowed the three booleans that used to sit beside it:
/// a mode with no controls now draws none by construction, where before every
/// flag brought a panel that had to remember to hide itself.
///
/// # The picture is the evidence
///
/// The readouts this page used to print — the hand, the landing, the gap, the
/// refusal — were never *for* a person: the picture says all four, and says
/// them where the work is. They are not published as text either. A hidden
/// "Hand: holding X" node would be the same state label with its paint turned
/// off, and a driver asserting on it would be asserting on a caption rather
/// than on the thing. What carries them instead is the *picture's own* tree —
/// the ghost, the station marks, the measurement and the beads are agent
/// nodes where they are drawn, so a script and an eye read the same evidence
/// at the same place. See [`build_layer`].
pub(crate) fn stage_page(
    state: &StagePage,
    app: &Entity<Luma>,
    view: Option<&StageView>,
    window: &Window,
) -> AnyElement {
    let Some(view) = view else {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(float::empty_row("The room is not up."))
            .agent_node(Role::Card, format!("{} Stage builder", state.venue_name))
            .into_any_element();
    };
    div()
        .absolute()
        .inset_0()
        .child(add_button(app))
        .children(
            (view.choosing.is_some() || state.closing.is_closing())
                .then(|| chooser(state, view, view.choosing.unwrap_or(0), app, window)),
        )
        .children(view.configuring.as_ref().map(|row| configure(row, app)))
        .children(state.menu.as_ref().map(|menu| node_menu(menu, app)))
        .agent_node(Role::Card, format!("{} Stage builder", state.venue_name))
        .into_any_element()
}

/// Air between a floating control and the corner of the room it sits in.
///
/// Measured from the *picture*, not from the tab: the venue's own header row
/// sits above the room, and chrome that floated over it would cover the one
/// line naming what is being built.
const INSET: f32 = 12.0;

/// The builder's one resting affordance: add something.
///
/// A single button, because with nothing in the hand and nothing selected
/// there is exactly one thing to do. Everything else in the builder is reached
/// by pointing at the room — which is the point of a page whose subject is a
/// picture.
fn add_button(app: &Entity<Luma>) -> AnyElement {
    let open = app.clone();
    let bar = div()
        .absolute()
        .top(px(crate::visualizer::HEADER_HEIGHT + INSET))
        .left(px(INSET))
        .flex()
        .items_center()
        .gap(px(6.0))
        .occlude()
        .child(
            float::popover_card()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .p(px(4.0))
                .child(
                    float::btn("Add element", "stage-add")
                        .id("stage-add")
                        .child(
                            Icon::new(IconName::Plus)
                                .size(px(13.0))
                                .text_color(ladder::foreground_alpha(0.7)),
                        )
                        .on_click(move |_, window, cx| {
                            open.update(cx, |this, cx| this.stage_open_chooser_focused(window, cx));
                        })
                        .agent_node(Role::Button, "Add element"),
                )
                .child(float::key_hint_text("A", "")),
        );
    bar.into_any_element()
}

// ---------------------------------------------------------------------------
// The add-element dialog
// ---------------------------------------------------------------------------

/// How big the add-element card is. Fixed, for the reason every palette in the
/// app is: a card that resized as the query narrowed would move the row under
/// the pointer between one keystroke and the next.
const CHOOSER_SIZE: luma_ui::dialog::morph::MorphSize = luma_ui::dialog::morph::MorphSize {
    width: 640.0,
    height: 420.0,
};

/// One row the dialog can offer, and what taking it puts in the hand.
pub(crate) struct ChooserRow {
    pub(crate) label: String,
    pub(crate) section: &'static str,
    pub(crate) take: Holding,
}

/// Every element this venue can be given that matches the dialog's query, in
/// one list.
///
/// Catalog pieces, the fixtures the patch has never placed, and the library —
/// three provenances, one question ("what goes in next"), so one list. They
/// were two menus and a search field before, which is three places to look for
/// one answer.
///
/// Already narrowed, because the three provenances narrow differently and only
/// this function knows which is which: the catalog and the unplaced rows are
/// in memory and filtered here, while the library's rows *are* the answer to
/// the query — [`Library::search_fixtures`] matched them, and re-filtering the
/// page it returned would drop rows it matched on a field the label does not
/// carry.
pub(crate) fn chooser_rows(view: &StageView, library: &FixtureLibrary) -> Vec<ChooserRow> {
    let needle = library.query().trim().to_lowercase();
    let matches = |label: &str, section: &str| {
        needle.is_empty()
            || label.to_lowercase().contains(&needle)
            || section.to_lowercase().contains(&needle)
    };
    let mut rows: Vec<ChooserRow> = palette_rows()
        .into_iter()
        .filter(|row| matches(&row.label, row.group.as_str()))
        .map(|row| ChooserRow {
            label: row.label.clone(),
            section: row.group.as_str(),
            take: Holding::Piece {
                catalog_ref: row.catalog_ref.to_string(),
                kind: NodeKind::Piece,
                display_name: row.label,
                footing: hand::footing_for(row.catalog_ref, row.tower),
                params: BTreeMap::new(),
            },
        })
        .collect();
    rows.extend(
        view.unplaced
            .iter()
            .filter(|(_, label)| matches(label, "Unplaced"))
            .map(|(node, label)| ChooserRow {
                label: label.clone(),
                section: "Unplaced",
                take: Holding::Unplaced {
                    node: node.clone(),
                    label: label.clone(),
                },
            }),
    );
    rows.extend(library.entries().iter().map(|entry| {
        let label = format!("{} {}", entry.manufacturer, entry.model);
        ChooserRow {
            label: label.clone(),
            section: "Fixtures",
            take: Holding::Fixture {
                path: entry.path.clone(),
                label,
                mode: None,
                width_m: DEFAULT_FIXTURE_WIDTH_M,
            },
        }
    }));
    rows
}

/// The width a fixture is assumed to be until its definition lands. The same
/// fallback `luma_lib`'s `body_width_m` uses, so an un-fetched preview and a
/// fetched one differ only where the definition actually says something.
const DEFAULT_FIXTURE_WIDTH_M: f64 = 0.3;

/// What the row under the keyboard is, at a size worth looking at.
///
/// A pane and not a tooltip, because choosing between two trusses is a
/// comparison and a comparison wants one place the answer always appears. It
/// is deliberately words rather than a rendered thumbnail: the catalog's
/// meshes are loaded by the renderer for the *room*, and spinning one up per
/// highlighted row would put a mesh load inside an arrow key.
fn chooser_preview(row: Option<&ChooserRow>) -> Div {
    let pane = div()
        .w(px(CHOOSER_PREVIEW_W))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(16.0));
    let Some(row) = row else {
        return pane.child(float::empty_row("Nothing highlighted"));
    };
    let detail = match &row.take {
        Holding::Piece { footing, .. } => footing.map_or_else(
            || "Seats on its own underside".to_string(),
            |footing| format!("Stands on {footing}"),
        ),
        Holding::Unplaced { .. } => "Patched, never placed".to_string(),
        // The dialog never offers one — a duplicate comes off a selection.
        Holding::Duplicate { .. } => "A copy of what is selected".to_string(),
        Holding::Fixture { .. } => "Placed as a row along a face".to_string(),
    };
    pane.child(float::label(row.section.to_string()))
        .child(
            div()
                .text_size(px(15.0))
                .text_color(ladder::foreground())
                .child(row.label.clone())
                .agent_node(Role::Text, format!("Preview {}", row.label)),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.5))
                .child(detail),
        )
}

/// How wide the dialog's preview pane is — a third of the card, so the list
/// beside it still shows a full row without truncating.
const CHOOSER_PREVIEW_W: f32 = 208.0;

/// The add-element dialog: a search field, a preview, a sectioned list.
fn chooser(
    state: &StagePage,
    view: &StageView,
    cursor: usize,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let rows = chooser_rows(view, &state.library);
    let dismiss = app.clone();

    let mut list = float::list().id("stage-chooser-list").overflow_y_scroll();
    let mut section = "";
    for (index, row) in rows.iter().enumerate() {
        if row.section != section {
            section = row.section;
            list = list.child(float::section_heading(section.to_string()));
        }
        let app = app.clone();
        let take = row.take.clone();
        let label = row.label.clone();
        list = list.child(
            float::menu_row(RowState::of(false, index == cursor), label.clone())
                .id(gpui::SharedString::from(format!("choose:{index}")))
                .child(label.clone())
                .on_click(move |_, window, cx| {
                    let take = take.clone();
                    // The field held focus; the dialog it lived in is gone.
                    // Focus goes back to the tab, where the stage's own keys
                    // (W/E/A/F, escape) route from — a dangling handle would
                    // eat them all, and bare blur leaves them just as dead.
                    let focus = app.update(cx, |this, cx| {
                        this.stage_take(take, cx);
                        this.focus.clone()
                    });
                    window.focus(&focus, cx);
                })
                .agent_node(Role::Row, label),
        );
    }
    if rows.is_empty() {
        list = list.child(float::empty_row("Nothing matches"));
    }

    let keys = app.clone();
    let taken: Vec<Holding> = rows.iter().map(|row| row.take.clone()).collect();
    let body = div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground())
        // The list is walked from the frame, not from the field: the query is
        // short and edited with backspace, so the arrows belong to the rows —
        // the same call `luma_ui::float`'s pickers make.
        .on_key_down(move |event, window, cx| {
            let step: isize = match event.keystroke.key.as_str() {
                "down" => 1,
                "up" => -1,
                // Handled here as well as at the shell's rung, because the
                // search field holds focus and a field taking typed text
                // out-ranks the window's own Escape.
                "escape" => {
                    let focus = keys.update(cx, |this, cx| {
                        this.stage_escape(cx);
                        this.focus.clone()
                    });
                    window.focus(&focus, cx);
                    return;
                }
                "enter" => {
                    let take = taken.get(cursor).cloned();
                    let focus = keys.update(cx, |this, cx| {
                        if let Some(take) = take {
                            this.stage_take(take, cx);
                        }
                        this.focus.clone()
                    });
                    window.focus(&focus, cx);
                    return;
                }
                _ => return,
            };
            let len = taken.len();
            keys.update(cx, |this, cx| {
                if let Some(build) = this.build_mut() {
                    if let Hand::Choosing(choosing) = &mut build.hand {
                        // Wraps at both ends, so a list is a ring and neither
                        // end is a dead key.
                        choosing.cursor = if len == 0 {
                            0
                        } else {
                            (choosing.cursor as isize + step).rem_euclid(len as isize) as usize
                        };
                    }
                }
                cx.notify();
            });
        })
        .child(float::header_band().child(
            float::field().w_full().child(fixture_library::search_field(
                &state.library,
                true,
                true,
            )),
        ))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(chooser_preview(rows.get(cursor)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .border_l_1()
                        .border_color(luma_ui::glass::hairline(0.06))
                        .child(float::viewport().child(list)),
                ),
        )
        .child(
            float::footer_band()
                .child(float::key_hint_pair(
                    IconName::ArrowUp,
                    IconName::ArrowDown,
                    "Navigate",
                ))
                .child(float::key_hint_text("enter", "Take"))
                .child(float::key_hint_text("esc", "Close")),
        )
        .into_any_element();

    let host = luma_ui::dialog::Host {
        id: "stage-chooser".into(),
        viewport: window.viewport_size(),
        focus: &state.focus,
        focused: true,
        label: "Add element dialog".into(),
        scrim_dismiss: luma_ui::dialog::ScrimDismiss::Enabled(Box::new(move |window, cx| {
            let focus = dismiss.update(cx, |this, cx| {
                this.stage_escape(cx);
                this.focus.clone()
            });
            window.focus(&focus, cx);
        })),
        closing: state.closing.closing_since(),
    }
    .render(luma_ui::dialog::morph::fixed_card(
        "Add element card",
        CHOOSER_SIZE,
        body,
    ));
    // Anchored to the *window*, not to the page: the stage tab lives inside
    // the room's own box, and a card centred in that box sits off the
    // screen's centre. The host is already sized to the whole viewport, so
    // pinning its top-left to the window's is what centres it for real.
    gpui::deferred(
        gpui::anchored()
            .position(gpui::point(px(0.0), px(0.0)))
            .anchor(gpui::Anchor::TopLeft)
            .child(host),
    )
    .priority(1)
    .into_any_element()
}

// ---------------------------------------------------------------------------
// Configure: a row being fitted to a face
// ---------------------------------------------------------------------------

/// The configure popover, hung at the point on the face that was clicked.
///
/// In the viewport and not a modal, because every control on it changes the
/// picture behind it: the count and the layout are questions about what the
/// room would look like, and a card that covered the room while asking them
/// would be asking the operator to imagine the answer it is already drawing.
fn configure(view: &ConfigureView, app: &Entity<Luma>) -> AnyElement {
    let mut card = float::popover_card().w(px(260.0)).gap(px(8.0)).p(px(8.0));
    card = card.child(float::label(view.what.clone())).child(
        div()
            .text_size(px(11.0))
            .text_color(ladder::foreground_alpha(0.45))
            .child(view.host.clone())
            .agent_node(Role::Text, format!("On {}", view.host)),
    );

    let set_count = app.clone();
    card = card.child(float::field_row(
        "Count",
        float::scrub(
            "stage-count",
            view.count as f64,
            1.0,
            MAX_COUNT,
            1.0,
            CONTROL_WIDTH,
            move |value, _, cx| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let count = value.round().max(1.0) as usize;
                set_count.update(cx, |this, cx| this.stage_set_count(count, cx));
            },
        ),
    ));

    // Two of the three layouts: even always fits, a pitch is the one that can
    // be refused, and the third is a pair of fractions that wants a handle on
    // the picture rather than a cell in a card.
    let mut track = float::segmented().w(px(CONTROL_WIDTH));
    for (name, even) in [("Even", true), ("Spacing", false)] {
        let app = app.clone();
        track = track.child(
            float::segment(name, view.even == even, name)
                .id(name)
                .on_click(move |_, _, cx| {
                    let layout = if even {
                        luma_scene::distribute::Layout::Even
                    } else {
                        luma_scene::distribute::Layout::Spacing(DEFAULT_PITCH_M)
                    };
                    app.update(cx, |this, cx| this.stage_set_layout(layout, cx));
                })
                .agent_node(Role::Toggle, name),
        );
    }
    card = card.child(float::field_row("Layout", track));

    // The fit, always — a row that fits says so as quietly as one that does
    // not says why. Both sit directly above the button they qualify.
    match &view.fits {
        Ok(placed) => {
            card = card.child(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::foreground_alpha(0.45))
                    .child(format!("{placed} will fit"))
                    .agent_node(Role::Text, format!("{placed} will fit")),
            );
        }
        Err(why) => {
            card = card.child(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::status_warn())
                    .child(why.clone())
                    .agent_node(Role::Text, why.clone()),
            );
            if let Some(offer) = &view.offer {
                let extend = app.clone();
                card = card.child(
                    float::btn(offer.clone(), "stage-extend")
                        .id("stage-extend")
                        .w_full()
                        .on_click(move |_, _, cx| {
                            extend.update(cx, |this, cx| this.stage_extend_host(cx));
                        })
                        .agent_node(Role::Button, offer.clone()),
                );
            }
        }
    }

    let apply = app.clone();
    let ready = view.ready;
    card = card.child(
        float::btn_primary("Place")
            .id("stage-place-row")
            .w_full()
            .when(!ready, |b| b.opacity(float::INERT_OPACITY))
            .on_click(move |_, _, cx| {
                apply.update(cx, |this, cx| this.stage_apply_row(cx));
            })
            .agent_node(Role::Button, "Place")
            .agent_disabled(!ready),
    );

    let close = app.clone();
    float::anchored_at(
        "stage-configure",
        view.at,
        Dismiss::on_press_out(move |_, cx| {
            close.update(cx, |this, cx| this.stage_escape(cx));
        }),
        // The card reports its own bounds, because *where it is* is a claim:
        // the spec asks it to sit on the face that was clicked, and only the
        // rectangle can say whether it did.
        card.agent_node(Role::Card, "Row popover")
            .into_any_element(),
    )
}

/// The most bodies one gesture will seat on one face.
const MAX_COUNT: f64 = 24.0;

/// The pitch a spacing layout starts at, in metres.
const DEFAULT_PITCH_M: f64 = 1.0;

/// The width of a control inside the configure popover — the card, less both
/// gutters.
const CONTROL_WIDTH: f32 = 244.0;

// ---------------------------------------------------------------------------
// The selection
// ---------------------------------------------------------------------------

/// What a right-click on a placed node offers.
///
/// A menu and not a toolbar: these three act on *this* node, so they belong
/// where the node is. A bar at the top of the screen carrying "Flip" while the
/// thing it would flip is four hundred pixels away is a control that has lost
/// its subject.
/// The bead the node menu was opened from, and where.
pub(crate) struct NodeMenuAt {
    pub(crate) at: Point<Pixels>,
    pub(crate) node: String,
    pub(crate) socket: Option<String>,
}

fn node_menu(menu_at: &NodeMenuAt, app: &Entity<Luma>) -> AnyElement {
    let (dup, flip, detach, delete, close) = (
        app.clone(),
        app.clone(),
        app.clone(),
        app.clone(),
        app.clone(),
    );
    let mut menu = luma_ui::menu::ContextMenu::new("stage-node-menu", menu_at.at);
    if let Some(socket) = menu_at.socket.clone() {
        let extend = app.clone();
        let node = menu_at.node.clone();
        menu = menu
            .item("Extend run", move |_, cx| {
                let (node, socket) = (node.clone(), socket.clone());
                extend.update(cx, |this, cx| this.stage_extend_from(node, socket, cx));
            })
            .separator();
    }
    menu.item("Duplicate", move |_, cx| {
        dup.update(cx, |this, cx| this.stage_duplicate(cx));
    })
    .item("Flip", move |_, cx| {
        flip.update(cx, |this, cx| this.stage_flip(cx));
    })
    .separator()
    .destructive("Detach", move |_, cx| {
        detach.update(cx, |this, cx| this.stage_detach(cx));
    })
    .destructive("Delete", move |_, cx| {
        delete.update(cx, |this, cx| this.stage_delete(cx));
    })
    .render(move |_, cx| {
        close.update(cx, |this, cx| this.stage_close_menu(cx));
    })
}

/// The inspector: what is selected, what it may be moved by, and what the last
/// solve is still unhappy about.
///
/// A [`luma_ui::sheet`] and not a panel, for that module's reason: it is read
/// *while* working the room, so the room underneath keeps taking clicks the
/// whole time it is up. It retargets rather than reopening when the selection
/// changes.
/// How wide the selection's in-scene card is, and how it keeps clear of the
/// picture's edges. Beside the piece, never centred on it: the card is a
/// caption, and a caption over its subject is a cover.
const SELECTED_CARD_W: f32 = 190.0;
const SELECTED_CARD_GAP: f32 = 26.0;
const SELECTED_CARD_CLEAR: f32 = 170.0;

/// The highest a free placement may be scrubbed to, in metres — a rig's trim,
/// not a building's.
const MAX_TRIM_M: f64 = 8.0;

/// A quiet line of prose in the inspector: what the graph says about the
/// selection, in the graph's own words.
fn note(text: String) -> AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(ladder::foreground_alpha(0.5))
        .child(text.clone())
        .agent_node(Role::Text, text)
        .into_any_element()
}

/// The word an editable freedom goes by in the inspector.
///
/// The graph's parameter keys are `u`, `yaw`, `trim` — the names the solver
/// argues in. A person adjusting a light is sliding it, turning it or lifting
/// it, so the row is titled for the gesture and the key stays in the model.
fn axis_title(key: &str) -> &'static str {
    match key {
        "u" => "Slide",
        "yaw" => "Turn",
        _ => "Trim",
    }
}

// ---------------------------------------------------------------------------
// The layer over the picture
// ---------------------------------------------------------------------------

/// One thing the hand is doing, projected onto the picture.
pub(crate) struct Mark {
    pub(crate) at: Point<Pixels>,
    pub(crate) label: String,
    pub(crate) refused: bool,
}

/// Every placement the hand is currently proposing, in window pixels: the
/// ghost under the cursor, or one per body of a row being fitted.
///
/// The element-layer twin of the ghosts [`install`] pushes at the renderer —
/// same poses, same count, from the same state — because a preview drawn only
/// in pixels has no evidence under a headless run, and the spec asks that a
/// value change move these *before* Apply.
pub(crate) fn marks(build: &Build, camera: &luma_scene::Camera, size: (f32, f32)) -> Vec<Mark> {
    let mut out = Vec::new();
    if let Some(held) = build.hand.held() {
        if let Some(landed) = held.landed.as_ref().filter(|l| l.refused.is_none()) {
            for body in build.ghost_bodies(&held.what, landed) {
                if let Some(at) = project(camera, size, body.world.w_axis.truncate()) {
                    out.push(Mark {
                        at,
                        label: format!("Ghost {}", held.what.label()),
                        refused: false,
                    });
                }
            }
        }
    }
    if let Some(row) = build.hand.configuring() {
        if let Ok(stations) = &row.preview {
            for (index, station) in stations.iter().enumerate() {
                if let Some(at) = project(camera, size, station.w_axis.truncate()) {
                    out.push(Mark {
                        at,
                        label: format!("Station {} {}", index + 1, row.what.label()),
                        refused: false,
                    });
                }
            }
        }
    }
    out
}

/// Where the run's own controls sit, and the gap the ray found there.
///
/// `Some` for the whole of [`Hand::Extending`], because a run with nothing in
/// front of it is still a run being built: it gets its length box and its
/// commit, and what a ray hitting nothing takes away is only the *measurement*.
/// The design's fourth extend case — "ray hits nothing → ghost at
/// [`hand::STUB_LENGTH_M`], type a length" — is unreachable if the card is
/// gated on there being a gap to report.
fn run_card(build: &Build, camera: &luma_scene::Camera, size: (f32, f32)) -> Option<RunCard> {
    let run = build.hand.extending()?;
    let from = build.room.socket_world(&run.from_node, &run.from_socket)?;
    build.room.socket(&run.from_node, &run.from_socket)?;
    // The socket the run comes *out* of, not the middle of the line.
    //
    // A dimension belongs at the middle, and that is where this sat until the
    // controls joined it: the midpoint is a function of the length, so dragging
    // the length box slid the box out from under the pointer that was dragging
    // it. The socket does not move while a run grows, which is the whole
    // requirement for anchoring a control to it.
    let at = project(camera, size, from)?;
    Some(RunCard {
        at,
        measured: run.measurement().zip(
            run.reach
                .as_ref()
                .map(|reach| hand::feet_and_inches(reach.gap_m)),
        ),
    })
}

/// Where the run's controls hang, and the gap they hang beside.
struct RunCard {
    at: Point<Pixels>,
    /// The gap, in metres and in feet, when the ray found one to report.
    measured: Option<(String, String)>,
}

/// A socket-layer point in window pixels, or `None` behind the camera or well
/// off screen. The projection [`beads`] does, shared so a station and a socket
/// cannot disagree about where the same metre is.
fn project(
    camera: &luma_scene::Camera,
    size: (f32, f32),
    at: glam::DVec3,
) -> Option<Point<Pixels>> {
    let (width, height) = size;
    if width <= 1.0 || height <= 1.0 {
        return None;
    }
    let world = coords::world_from_three(at.as_vec3());
    let forward = (camera.target - camera.position()).normalize_or_zero();
    if (world - camera.position()).dot(forward) <= 0.0 {
        return None;
    }
    let ndc = camera.project(world, width / height);
    if ndc.x.abs() > 1.2 || ndc.y.abs() > 1.2 {
        return None;
    }
    Some(Point::new(
        px((ndc.x * 0.5 + 0.5) * width),
        px((1.0 - (ndc.y * 0.5 + 0.5)) * height),
    ))
}

/// The widest a run may be scrubbed to, in metres. A bound rather than a
/// limit: a scrub maps a box to a range, so the range is the control's scale,
/// and a truss longer than this is asked for by number rather than swept to.
const MAX_RUN_M: f64 = 12.0;

/// How wide the run's length box is.
const RUN_SCRUB_W: f32 = 84.0;

/// How wide the run's whole control strip is: the measurement with its
/// imperial small print, the length box and the commit, inside the card's own
/// padding.
///
/// Fixed rather than fitted, because the strip has to be *placed* before it
/// could be measured — see the clamp in [`build_layer`] — and a card that had
/// to be laid out before it could be kept reachable is a card that cannot be.
/// Its three children are a fixed set, so this is a fact about the strip.
const RUN_CARD_W: f32 = 360.0;

/// The hit box a station mark carries, and the dot inside it.
const MARK_HALF: f32 = 9.0;
const STATION_DOT: f32 = 6.0;

/// How big a preview body is drawn, as a fraction of the catalog block it
/// borrows its shape from.
///
/// A stand-in: a library fixture has no mesh in this catalog, and loading one
/// per preview frame to answer "roughly here, roughly this big" would be a
/// download in a drag loop. The station marks on the element layer are the
/// precise claim; this is the mass that makes a row read as a row.
const STATION_GHOST_SCALE: f32 = 0.5;

/// A socket nobody is aiming at: small enough to read as a mark on the room
/// rather than as a control laid over it.
const BEAD_QUIET: f32 = 5.0;

/// A socket that would take what is in the hand, or has taken it.
const BEAD_LIVE: f32 = 7.0;

/// How far apart two beads must sit before both can be pressed.
///
/// Two beads on one pixel is not a near miss, it is one unreachable socket:
/// hit-testing is paint order, so the later bead swallows every press aimed at
/// the earlier one. The venue's own two hosts are the standing case — the floor
/// and the grid are the *same point* with opposite normals
/// ([`luma_scene::venue::root_socket`]) — but a deck seen edge-on stacks its
/// corners the same way, so the fix is the general one.
const BEAD_CLEAR: f32 = 2.0 * BEAD_LIVE;

/// Push beads apart until each has its own pixel.
///
/// A bead is a *handle for* a socket, not a claim about where the socket is:
/// pressing one aims by name ([`Luma::stage_aim_socket`]), and the renderer
/// draws the socket itself at the exact point. So a nudged handle costs
/// nothing that is true, and buys a socket that can be reached.
///
/// Downward, in list order, because the order sockets come out of the room is
/// stable — so the same room lays its beads out the same way every frame, and
/// a script that found one last frame finds it in the same place this frame.
fn declutter(beads: &mut [Bead]) {
    for at in 1..beads.len() {
        loop {
            let here = beads[at].at;
            let crowded = beads[..at].iter().any(|other| {
                let dx = f32::from(other.at.x - here.x);
                let dy = f32::from(other.at.y - here.y);
                dx.hypot(dy) < BEAD_CLEAR
            });
            if !crowded {
                break;
            }
            beads[at].at.y += px(BEAD_CLEAR);
        }
    }
}

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
    let held_kind = build
        .hand
        .held()
        .map_or(luma_scene::venue::NodeKind::Fixture, |held| {
            held.what.kind()
        });
    let latched = build
        .hand
        .held()
        .and_then(|held| held.latched.as_ref())
        .cloned();
    let forward = (camera.target - camera.position()).normalize_or_zero();
    let mut out = Vec::new();
    for (node, socket) in build.room.open_sockets() {
        if !build.socket_shown(node) || !hand::can_host(socket) {
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
        } else if !held.is_empty() && hand::compatible(socket, &held, held_kind) {
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
    declutter(&mut out);
    out
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
    wired: bool,
    app: &Entity<Luma>,
) -> AnyElement {
    let _ = origin;
    use luma_render::scene_desc::SocketMarkState;
    let mut layer = div().absolute().inset_0();
    // A held piece still *claims* the room — this card is the harness's
    // evidence for what the hand is doing — but with a live viewport it does
    // not take the pointer: the viewport's own listener routes a click to a
    // placement and a drag to the camera, which is what makes an orbit
    // available with a ghost in the air. With no viewport (`!wired`, the
    // headless state) there is no listener, so the card carries the handlers
    // itself and does not occlude — there is no camera gesture to protect.
    if build.hand.owns_pointer() {
        let mut surface = div().id("stage-drop-surface").absolute().inset_0();
        if !wired && build.hand.aims_with_pointer() {
            let aim = app.clone();
            let drop = app.clone();
            surface = surface
                .on_mouse_move(move |event, _, cx| {
                    let at = event.position;
                    aim.update(cx, |this, cx| this.stage_aim_from_pointer(at, cx));
                })
                .on_click(move |event, _, cx| {
                    let at = event.position();
                    drop.update(cx, |this, cx| this.stage_click_room(at, cx));
                });
        }
        layer = layer.child(surface.agent_node(Role::Card, "Stage drop surface"));
    } else if !wired && matches!(build.hand, Hand::Idle) {
        // The headless twin of the viewport's click-to-select: with no canvas
        // there is no listener, so the room at rest is a surface a press can
        // pick a piece off (`Room::pick`). Behind the beads, like the canvas.
        let pick = app.clone();
        layer = layer.child(
            div()
                .id("stage-room-surface")
                .absolute()
                .inset_0()
                .on_click(move |event, _, cx| {
                    let at = event.position();
                    pick.update(cx, |this, cx| this.stage_pick_at(at, cx));
                })
                .agent_node(Role::Card, "Stage room"),
        );
    }
    // Beads first, then everything that sits over them. gpui hit-tests in paint
    // order, so a control added before a bead is a control the bead swallows —
    // which is exactly what the run's length box did from under one.
    for bead in beads(build, camera, size) {
        let app = app.clone();
        let hover = app.clone();
        let (node, socket) = (bead.node.clone(), bead.socket.clone());
        let (hover_node, hover_socket) = (node.clone(), socket.clone());
        let (menu, menu_node, menu_socket) = (app.clone(), node.clone(), socket.clone());
        // Quiet unless it is a candidate. An open socket is a fact about the
        // room and there are dozens of them; a compatible one is an answer to
        // what is in the hand, and the latched one is *the* answer. Size and
        // value carry that ranking together, so the picture reads before any
        // of it is named.
        let (dot, colour, ring) = match bead.state {
            SocketMarkState::Open => (BEAD_QUIET, ladder::foreground_alpha(0.34), 0.0),
            SocketMarkState::Compatible => (BEAD_LIVE, ladder::accent().into(), 0.0),
            SocketMarkState::Latched => (BEAD_LIVE, ladder::foreground().into(), 3.0),
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
                // Not `occlude`: a bead owns the press, but the wheel is the
                // camera's everywhere. While placing, beads pepper the whole
                // structure — full occlusion made zoom dead over exactly the
                // thing being looked at.
                .block_mouse_except_scroll()
                .child(
                    // No border: a hairline around a 5px dot over a dark room
                    // is most of the dot. The latched bead gets a halo instead
                    // — light *around* the mark rather than a line closing it
                    // in, which is what a thing being held looks like.
                    div().w(px(dot)).h(px(dot)).rounded_full().bg(colour).when(
                        ring > 0.0,
                        |mark| {
                            mark.shadow(vec![gpui::BoxShadow {
                                color: ladder::foreground_alpha(0.35),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(ring),
                                spread_radius: px(ring * 0.5),
                                inset: false,
                            }])
                        },
                    ),
                )
                // Aiming is screen-space; acceptance is not. A raised socket is
                // metres away from wherever the cursor's ray meets the floor,
                // so pointing at a bead is the only way a hand can reach one —
                // and the bead's own hitbox is the radius that decides, in
                // pixels. What it then hands the ladder is the socket's *world*
                // position, which is what the snap-out radius is measured in.
                .on_hover(move |hovered, _, cx| {
                    if !*hovered {
                        return;
                    }
                    let (node, socket) = (hover_node.clone(), hover_socket.clone());
                    hover.update(cx, |this, cx| this.stage_aim_socket(&node, &socket, cx));
                })
                .on_click(move |event, _, cx| {
                    let (node, socket) = (node.clone(), socket.clone());
                    let at = event.position();
                    app.update(cx, |this, cx| {
                        this.stage_socket_clicked(node, socket, at, cx);
                    });
                })
                .on_mouse_down(gpui::MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    let (node, socket, at) =
                        (menu_node.clone(), menu_socket.clone(), event.position);
                    menu.update(cx, |this, cx| {
                        if let Some(build) = this.build_mut() {
                            build.select(Some(node.clone()));
                        }
                        this.stage_open_menu(at, node, Some(socket), cx);
                    });
                })
                .agent_node(Role::Button, bead.label),
        );
    }
    // The selection's own card, drawn beside the thing it is about. The
    // sheet this replaces took a column off the room to say the same four
    // facts; the room is the page, so the facts sit next to their subject.
    if matches!(build.hand, Hand::Idle) {
        if let Some(selected) = build.selected_view() {
            {
                // Beside the piece when the piece is on screen; pinned inside
                // the frame when it is not. A trim drag can lift the subject
                // out of view, and a control that unmounted mid-drag would
                // take the drag with it.
                let at = build
                    .card_anchor
                    .or_else(|| {
                        build
                            .room
                            .pose(&selected.node)
                            .map(|pose| pose.w_axis.truncate())
                    })
                    .and_then(|anchor| project(camera, size, anchor))
                    .unwrap_or_else(|| Point::new(px(size.0 * 0.5), px(size.1 * 0.5)));
                let mut card = float::popover_card()
                    .w(px(SELECTED_CARD_W))
                    .gap(px(6.0))
                    .p(px(8.0));
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(ladder::foreground())
                        .child(selected.label.clone())
                        .agent_node(Role::Text, selected.label.clone()),
                );
                let editable = selected.freedom.param().map_or_else(
                    || {
                        (selected.freedom == Freedom::Free).then_some((
                            "trim",
                            selected.trim,
                            MAX_TRIM_M,
                        ))
                    },
                    |key| Some((key, selected.param, if key == "u" { 1.0 } else { 360.0 })),
                );
                if let Some((key, value, max)) = editable {
                    let app = app.clone();
                    let node = selected.node.clone();
                    let step = if key == "yaw" { 5.0 } else { 0.05 };
                    card = card.child(float::field_row(
                        axis_title(key),
                        float::scrub(
                            format!("stage-{key}"),
                            value,
                            0.0,
                            max,
                            step,
                            CONTROL_WIDTH,
                            move |value, _, cx| {
                                let node = node.clone();
                                app.update(cx, |this, cx| {
                                    if key == "trim" {
                                        this.stage_set_trim(value, cx);
                                    } else if key == "yaw" {
                                        this.stage_set_param(&node, key, value.to_radians(), cx);
                                    } else {
                                        this.stage_set_param(&node, key, value, cx);
                                    }
                                });
                            },
                        ),
                    ));
                }
                if let Some(angle) = selected.angle {
                    let app = app.clone();
                    let node = selected.node.clone();
                    card = card.child(float::field_row(
                        "Angle",
                        float::scrub(
                            "stage-angle",
                            angle,
                            0.0,
                            180.0,
                            5.0,
                            CONTROL_WIDTH,
                            move |value, _, cx| {
                                let node = node.clone();
                                app.update(cx, |this, cx| {
                                    this.stage_set_param(&node, "angle", value, cx);
                                });
                            },
                        ),
                    ));
                }
                if let Some(span) = selected.span {
                    let app = app.clone();
                    let node = selected.node.clone();
                    card = card.child(float::field_row(
                        "Span",
                        float::scrub(
                            "stage-span",
                            span,
                            hand::LENGTH_STEP_M,
                            MAX_RUN_M,
                            hand::LENGTH_STEP_M,
                            CONTROL_WIDTH,
                            move |value, _, cx| {
                                let node = node.clone();
                                app.update(cx, |this, cx| {
                                    this.stage_set_param(&node, "span", value, cx);
                                });
                            },
                        ),
                    ));
                }
                card = card
                    .children(selected.relation.clone().map(note))
                    .children(selected.constraint.clone().map(note));
                let left = px((f32::from(at.x) + SELECTED_CARD_GAP)
                    .min(size.0 - SELECTED_CARD_W - INSET)
                    .max(INSET));
                let top = px(f32::from(at.y)
                    .min(size.1 - SELECTED_CARD_CLEAR)
                    .max(crate::visualizer::HEADER_HEIGHT + INSET));
                layer = layer.child(
                    div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .occlude()
                        .child(card)
                        .agent_node(Role::Card, "Selection card"),
                );
            }
        }
    }
    // What the hand is doing, where the hand is doing it. These are the
    // builder's evidence — for an eye because they are drawn over the room,
    // and for a driver because they are nodes. Neither reads a caption.
    layer = layer.children(marks(build, camera, size).into_iter().map(|mark| {
        div()
            .absolute()
            .left(mark.at.x - px(MARK_HALF))
            .top(mark.at.y - px(MARK_HALF))
            .w(px(MARK_HALF * 2.0))
            .h(px(MARK_HALF * 2.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(STATION_DOT))
                    .h(px(STATION_DOT))
                    .rounded_full()
                    .bg(if mark.refused {
                        ladder::danger()
                    } else {
                        ladder::accent()
                    }),
            )
            .agent_node(Role::Text, mark.label)
    }));
    // The run's own controls, on the line the renderer draws. A length is a
    // property of *this* measurement, so the number and the press that commits
    // it ride the thing they are about rather than a bar somewhere else — which
    // is the same argument the configure popover makes about a face.
    if let Some(RunCard { at, measured }) = run_card(build, camera, size) {
        // Clear of the inspector, which is painted over this layer: a run out
        // of a socket on the house-right side of the room put its commit
        // *under* the sheet, where the pointer could not reach it and nothing
        // in the tree said so — the button reports its bounds and its
        // enablement either way. Anchored to the socket until that would hide
        // it, and pushed left exactly as far as it must be.
        let reachable = size.0 - INSET;
        let left = px(f32::from(at.x).min(reachable - RUN_CARD_W).max(INSET));
        let refused = build
            .hand
            .extending()
            .and_then(Extending::refused)
            .is_some();
        let length = build.hand.extending().map_or(0.0, |run| run.length_m);
        let (set, commit) = (app.clone(), app.clone());
        layer = layer.child(
            div().absolute().left(left).top(at.y).occlude().child(
                float::popover_card()
                    .w(px(RUN_CARD_W))
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .p(px(4.0))
                    // The measurement, when the ray found one to make. Nothing
                    // stands in for it when it did not: a readout echoing the
                    // length back as a "gap" would invent a measurement out of
                    // the number being typed.
                    .children(measured.map(|(metres, feet)| {
                        div()
                            .pl(px(6.0))
                            .flex()
                            .items_baseline()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .font_family(luma_ui::fonts::MONO)
                                    .text_size(px(11.0))
                                    .text_color(ladder::foreground())
                                    .child(metres.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(ladder::foreground_alpha(0.45))
                                    .child(feet.clone()),
                            )
                            .agent_node(Role::Text, format!("{metres} · {feet}"))
                    }))
                    .child(float::scrub(
                        "stage-length",
                        length,
                        hand::LENGTH_STEP_M,
                        MAX_RUN_M,
                        hand::LENGTH_STEP_M,
                        RUN_SCRUB_W,
                        move |metres, _, cx| {
                            set.update(cx, |this, cx| this.stage_set_length(metres, cx));
                        },
                    ))
                    .child(
                        float::btn_primary("Place run")
                            .id("stage-place-run")
                            .when(refused, |b| b.opacity(float::INERT_OPACITY))
                            .on_click(move |_, _, cx| {
                                commit.update(cx, |this, cx| this.stage_commit_run(cx));
                            })
                            .agent_node(Role::Button, "Place run")
                            .agent_disabled(refused),
                    ),
            ),
        );
    }
    // A held truss's one parameter, editable in the hand: the ghost is the
    // preview and this is its dial. Top-centre, clear of the add button and
    // the room the ghost is over.
    if let Some(hand::Held {
        what: Holding::Piece {
            catalog_ref,
            params,
            ..
        },
        ..
    }) = build.hand.held()
    {
        let truss = matches!(
            luma_scene::catalog::piece(catalog_ref).map(|piece| piece.geometry),
            Some(luma_scene::catalog::Geometry::Procedural(
                luma_scene::catalog::Family::Truss
            ))
        );
        if truss {
            let span = params
                .get("span")
                .copied()
                .unwrap_or(f64::from(luma_render::catalog::DEFAULT_TRUSS_SPAN_M));
            let set = app.clone();
            layer = layer.child(
                div()
                    .absolute()
                    .top(px(crate::visualizer::HEADER_HEIGHT + INSET))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        float::popover_card()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .p(px(4.0))
                            .occlude()
                            .child(float::label("Span"))
                            .child(float::scrub(
                                "stage-held-span",
                                span,
                                hand::LENGTH_STEP_M,
                                MAX_RUN_M,
                                hand::LENGTH_STEP_M,
                                RUN_SCRUB_W,
                                move |value, _, cx| {
                                    set.update(cx, |this, cx| {
                                        this.stage_set_held_param("span", value, cx);
                                    });
                                },
                            ))
                            .agent_node(Role::Card, "Held span"),
                    ),
            );
        }
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
        // A refused landing draws nothing: the hand still knows where the
        // cursor is, but a piece that cannot go there is not previewed there —
        // the ghost disappearing *is* the refusal.
        if let Some(landed) = held.landed.as_ref().filter(|l| l.refused.is_none()) {
            for body in build.ghost_bodies(&held.what, landed) {
                let (pos, rot) = luma_scene::coords::data_pose_of_d(body.world);
                out.ghosts.push(Ghost {
                    geometry: body.geometry,
                    pos: pos.map(|v| v as f32),
                    rot: rot.map(|v| v as f32),
                    scale: body.scale,
                    refused: false,
                });
            }
        }
    }
    // Every body of a row being fitted, at the poses the band math just
    // answered with. A refused row previews nothing — there are no stations to
    // draw, which is itself the picture of "this will not fit".
    if let Some(row) = build.hand.configuring() {
        if let Ok(stations) = &row.preview {
            for station in stations {
                let (pos, rot) = luma_scene::coords::data_pose_of_d(*station);
                out.ghosts.push(Ghost {
                    geometry: luma_render::scene_desc::Geometry::Procedural(
                        luma_render::catalog::default_params(luma_scene::catalog::Family::Corner),
                    ),
                    pos: pos.map(|v| v as f32),
                    rot: rot.map(|v| v as f32),
                    scale: STATION_GHOST_SCALE,
                    refused: false,
                });
            }
        }
    }
    if let Some(run) = build.hand.extending() {
        if let Some(from) = build.room.socket_world(&run.from_node, &run.from_socket) {
            let socket = build.room.socket(&run.from_node, &run.from_socket);
            let pose = build.room.pose(&run.from_node);
            if let (Some(socket), Some(pose)) = (socket, pose) {
                let refused = run.refused().is_some();
                let direction = pose.transform_vector3(socket.normal).normalize_or_zero();
                let to = from + direction * run.length_m;
                out.measure = Some(Measure {
                    from: point_of(from),
                    to: point_of(to),
                    refused,
                });
                // The run's own ghost, at the length being asked for. This is
                // the design's red case: a length past the measured gap is one
                // of the two hard errors, and the ghost is where it is said.
                #[allow(clippy::cast_possible_truncation)]
                let params = luma_render::scene_desc::Procedural::Truss {
                    span: run.length_m as f32,
                };
                let held = luma_render::catalog::procedural_sockets(params);
                if let Some(end) = held.iter().find(|s| s.name == "end_a") {
                    let world = luma_scene::venue::place_on(
                        pose,
                        socket,
                        end,
                        NodeKind::Run,
                        luma_scene::venue::SurfacePlacement::FLUSH,
                    );
                    let (pos, rot) = luma_scene::coords::data_pose_of_d(world);
                    out.ghosts.push(Ghost {
                        geometry: luma_render::scene_desc::Geometry::Procedural(params),
                        pos: pos.map(|v| v as f32),
                        rot: rot.map(|v| v as f32),
                        scale: 1.0,
                        refused,
                    });
                }
            }
        }
    }
    // The selected piece's own dimension: a run at rest wears the measure the
    // extend flow draws, along its span — the "[ 3.0 m ]" the picture owes a
    // selected stick without a sheet to print it in.
    if out.measure.is_none() {
        if let (Hand::Idle, Some(node)) = (&build.hand, build.selected.as_ref()) {
            if let (Some(from), Some(to)) = (
                build.room.socket_world(node, "end_a"),
                build.room.socket_world(node, "end_b"),
            ) {
                out.measure = Some(Measure {
                    from: point_of(from),
                    to: point_of(to),
                    refused: false,
                });
            }
        }
    }
    let held = build.held_sockets();
    let held_kind = build
        .hand
        .held()
        .map_or(luma_scene::venue::NodeKind::Fixture, |held| {
            held.what.kind()
        });
    out.sockets = build
        .room
        .open_sockets()
        .filter(|(node, socket)| build.socket_shown(node) && hand::can_host(socket))
        .filter_map(|(node, socket)| {
            let at = build.room.socket_world(node, &socket.name)?;
            let pose = build.room.pose(node)?;
            Some(SocketMark {
                pos: point_of(at),
                normal: point_of(pose.transform_vector3(socket.normal).normalize_or_zero()),
                state: if !held.is_empty() && hand::compatible(socket, &held, held_kind) {
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
fn geometry_of(catalog_ref: &str) -> Option<luma_render::scene_desc::Geometry> {
    use luma_render::scene_desc::Geometry;
    match luma_scene::catalog::piece(catalog_ref)?.geometry {
        luma_scene::catalog::Geometry::Mesh { path } => Some(Geometry::mesh(path)),
        luma_scene::catalog::Geometry::Procedural(family) => Some(Geometry::Procedural(
            luma_render::catalog::default_params(family),
        )),
    }
}

/// The geometry a *held* piece draws as, at its own parameters — a truss whose
/// span was scrubbed ghosts at that span, not at the palette default.
fn geometry_with(
    catalog_ref: &str,
    params: &BTreeMap<String, f64>,
) -> Option<luma_render::scene_desc::Geometry> {
    use luma_render::scene_desc::Geometry;
    match luma_scene::catalog::piece(catalog_ref)?.geometry {
        luma_scene::catalog::Geometry::Mesh { path } => Some(Geometry::mesh(path)),
        luma_scene::catalog::Geometry::Procedural(family) => Some(Geometry::Procedural(
            luma_render::catalog::node_params(family, &params_of(params)),
        )),
    }
}

/// A held piece's parameter map in the shape the generator reads.
fn params_of(params: &BTreeMap<String, f64>) -> luma_scene::venue::Params {
    let mut out = luma_scene::venue::Params::default();
    for (key, value) in params {
        out.set(key.clone(), *value);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use luma_scene::venue::{Edge, Node, NodeKind, Params, VenueGraph};

    use super::{Build, CopyStep, Landing, Room};

    /// A graph with no room around it. `copy_plan` reads the graph and nothing
    /// else — the poses are the *renderer's* half of a duplicate, and the rows
    /// it plans are decided before anything is solved.
    fn build_of(graph: VenueGraph) -> Build {
        let sockets = luma_render::catalog::VenueSockets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
        )
        .expect("the shipped meshes");
        Build {
            venue_id: "venue".into(),
            room: Room::new(&graph, &sockets, HashMap::new()),
            graph,
            solved: luma_lib::models::venue_graph::ResolvedVenue::default(),
            hand: super::Hand::Idle,
            sockets,
            selected: None,
            card_anchor: None,
            trim_draft: None,
            report: Vec::new(),
            committing: false,
        }
    }

    fn params(u: f64) -> Params {
        let mut params = Params::default();
        params.set("u", u);
        params
    }

    fn piece(id: &str, u: f64) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Piece,
            catalog_ref: Some("truss.straight".into()),
            label: None,
            params: params(u),
        }
    }

    fn edge(parent: &str, mine: &str, theirs: &str, roll: f64) -> Edge {
        Edge {
            parent: parent.into(),
            my_socket: mine.into(),
            their_socket: theirs.into(),
            roll,
        }
    }

    /// What one row of the plan says, with the minted id dropped: a duplicate
    /// mints new ids by construction, so ids are the one thing two builds of
    /// the same wing cannot agree on.
    fn row(step: &CopyStep, parent: &str) -> (String, String, String, String, i64, i64) {
        #[allow(clippy::cast_possible_truncation)]
        let round = |value: f64| (value * 1e6).round() as i64;
        (
            parent.to_string(),
            step.my_socket.clone(),
            step.their_socket.clone(),
            step.catalog_ref.clone().unwrap_or_default(),
            round(step.yaw),
            round(step.params.get("u").copied().unwrap_or_default()),
        )
    }

    /// Duplicate + flip of an asymmetric wing writes the same `venue_edges`
    /// rows as building the opposite wing by hand.
    ///
    /// The wing is asymmetric on purpose — it bolts to its parent by a *sided*
    /// face, which is the case that a flip mirroring only the host's half of
    /// each joint got wrong: the copy met its parent on the same socket the
    /// original did, so the mirrored wing was bolted on inside-out.
    #[test]
    fn a_flipped_duplicate_writes_the_hand_built_opposite_wings_rows() {
        let root = Node {
            id: "venue".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        };
        let mut graph = VenueGraph::new(root);
        // The left wing: a stick on the downstage-left corner, an arm bolted to
        // its left face, a light clamped under the arm.
        graph.insert_placed(
            piece("wing_l", 0.4),
            edge("venue", "end_a", "corner_fl", 0.3),
        );
        graph.insert_placed(
            piece("arm_l", 0.25),
            edge("wing_l", "face_left", "face_right", 0.2),
        );
        graph.insert_placed(
            piece("light_l", -0.1),
            edge("arm_l", "clamp", "face_-y", 0.0),
        );
        // The same wing built by hand on the other side.
        graph.insert_placed(
            piece("wing_r", -0.4),
            edge("venue", "end_a", "corner_fr", -0.3),
        );
        graph.insert_placed(
            piece("arm_r", -0.25),
            edge("wing_r", "face_right", "face_left", -0.2),
        );
        graph.insert_placed(
            piece("light_r", 0.1),
            edge("arm_r", "clamp", "face_-y", 0.0),
        );

        let build = build_of(graph);
        let landing = Landing::Socket {
            parent: "venue".into(),
            my_socket: "end_a".into(),
            their_socket: "corner_fr".into(),
            yaw: -0.3,
        };
        let plan = build
            .copy_plan("wing_l", true, &landing)
            .expect("a socket landing plans a copy");

        // The plan, with each step's parent named by the *source* it copies —
        // a source id is a tree position, and tree position is the only thing
        // two builds of the same wing can be compared by.
        let copied: Vec<_> = plan.iter().map(|step| row(step, &step.parent)).collect();

        // The hand-built wing's rows, read back off the graph the same way.
        let hand: Vec<_> = ["wing_r", "arm_r", "light_r"]
            .into_iter()
            .map(|id| {
                let node = build.graph.node(id).expect("the hand-built node");
                let edge = build.graph.edge(id).expect("its edge");
                #[allow(clippy::cast_possible_truncation)]
                let round = |value: f64| (value * 1e6).round() as i64;
                (
                    // `wing_r`'s parent is the venue; the others' is the source
                    // id of the copy's corresponding parent, which is what the
                    // loop above spells for the copy.
                    match id {
                        "wing_r" => "venue",
                        "arm_r" => "wing_l",
                        _ => "arm_l",
                    }
                    .to_string(),
                    edge.my_socket.clone(),
                    edge.their_socket.clone(),
                    node.catalog_ref.clone().unwrap_or_default(),
                    round(edge.roll),
                    round(node.params.get("u", 0.0)),
                )
            })
            .collect();
        assert_eq!(copied, hand, "the flipped copy is not the opposite wing");
    }
}
