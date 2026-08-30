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
use gpui::{div, px, AnyElement, App, Context, Div, Entity, Pixels, Point, Window};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Icon, IconName};
use luma_lib::models::distribute::DistributeLayout;
use luma_lib::models::venue_graph::{PlacementReport, ResolvedVenue};
use luma_render::catalog::VenueSockets;
use luma_scene::catalog::{pieces, PaletteGroup};
use luma_scene::coords;
use luma_scene::venue::{NodeKind, NodeSockets as _, VenueGraph};
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

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
    /// How much of the inspector sheet is on screen. The one place "is the
    /// inspector up" lives — see [`luma_ui::pane::PaneWidth`]; its target is
    /// *derived* from the selection by [`Build::inspector_target`], so a
    /// selection made anywhere cannot forget to open the sheet.
    pub(crate) inspector: luma_ui::pane::PaneWidth,
    /// The operator asked to see the solve's complaints with nothing selected.
    pub(crate) warnings_pinned: bool,
    /// What the fixture library last answered the dialog's query with.
    pub(crate) fixtures: Vec<(String, String)>,
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
            inspector: luma_ui::pane::PaneWidth::new(0.0),
            warnings_pinned: false,
            fixtures: Vec::new(),
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
    pub(crate) fn inspector_target(&self) -> f32 {
        let wanted = self.selected.is_some()
            || (self.warnings_pinned
                && !(self.solved.dangling.is_empty() && self.report.is_empty()));
        if wanted {
            luma_ui::sheet::WIDTH
        } else {
            0.0
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
            Holding::Tray { .. } | Holding::Fixture { .. } => {
                vec![luma_render::catalog::fixture_clamp()]
            }
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

/// The row layout, in the words the commit speaks.
///
/// Two vocabularies exist because the preview is geometry and the commit is a
/// request over the wire; this is the one place they meet, so neither has to
/// know the other's spelling.
///
/// **Smell:** `luma_lib`'s `distribute` model already carries the *forward*
/// conversion as `impl From<DistributeLayout> for Layout`. This is its inverse
/// and belongs beside it as a second `From`, not here — it lives in the app
/// only because `src-tauri` is another owner's tree this pass.
fn layout_of(layout: luma_scene::distribute::Layout) -> DistributeLayout {
    match layout {
        luma_scene::distribute::Layout::Even => DistributeLayout::Even,
        luma_scene::distribute::Layout::Spacing(metres) => DistributeLayout::Spacing { metres },
        luma_scene::distribute::Layout::Span(t0, t1) => DistributeLayout::Span { from: t0, to: t1 },
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
    /// The add-element dialog's query. An editor is an entity — it owns a
    /// caret, a selection and an undo history — so it lives on the page rather
    /// than in [`hand::Hand`], which holds no state a window could focus.
    pub(crate) search: Entity<luma_ui::text_input::TextInput>,
    /// The focus the dialog traps while it is up.
    pub(crate) focus: gpui::FocusHandle,
    /// Where the node menu is open, if it is. On the page and not the hand:
    /// a menu is chrome about the selection, not a thing the hand is doing.
    pub(crate) menu: Option<Point<Pixels>>,
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
            search: cx.new(|cx| luma_ui::text_input::TextInput::search("Search elements…", cx)),
            focus: cx.focus_handle(),
            menu: None,
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

    /// The inspector's width this frame, retargeted from the state that
    /// decides it — see [`Build::inspector_target`].
    pub(crate) fn stage_inspector_width(&mut self, window: &mut Window, cx: &App) -> Pixels {
        let Some(build) = self.build_mut() else {
            return px(0.0);
        };
        let target = build.inspector_target();
        build.inspector.retarget(target, cx);
        build.inspector.eval(window)
    }

    /// Open the add-element dialog.
    pub(crate) fn stage_open_chooser(&mut self, cx: &mut Context<Self>) {
        if let Some(build) = self.build_mut() {
            build.hand = Hand::Choosing(hand::Choosing::default());
        }
        // The library is asked once per opening rather than per keystroke: the
        // dialog filters what it has, and forty rows is the whole index for a
        // query this shallow.
        self.stage_search_fixtures(String::new(), cx);
        cx.notify();
    }

    /// Take a row out of the dialog and into the hand.
    ///
    /// A library fixture arrives without its mode or its measurements — the
    /// definition is a file read — so the hand takes it immediately and the
    /// two facts land when they land. What the hand *cannot* do until they do
    /// is commit, which is what [`ConfigureView::ready`] gates.
    pub(crate) fn stage_take(&mut self, what: Holding, cx: &mut Context<Self>) {
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
    pub(crate) fn stage_open_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(crate::shell::Body::Stage(page)) = self.workspace.active_body_mut() {
            page.menu = Some(at);
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
    pub(crate) fn stage_escape(&mut self, cx: &mut Context<Self>) {
        self.stage_close_menu(cx);
        if let Some(build) = self.build_mut() {
            let was_idle = matches!(build.hand, Hand::Idle);
            build.hand = std::mem::take(&mut build.hand).escape();
            if was_idle {
                build.selected = None;
                build.warnings_pinned = false;
            }
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
        if let Hand::Placing(held) = &mut build.hand {
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
        // A library fixture reaches the room only through the configure
        // popover — it is created as a *row*, and a row needs a face to lie
        // along. Dropping one on the floor has no verb, so the gesture is not
        // one: the ghost simply stays in the hand.
        if matches!(held.what, Holding::Fixture { .. }) {
            return;
        }
        let what = held.what.clone();
        // Place mode is sticky, and only for a catalog piece: that is a stamp,
        // and an operator rigging a row of them should not have to walk back
        // to the dialog between each. A tray fixture and a duplicate are one
        // specific thing each — once placed there is no second one to hold.
        build.hand = match &what {
            Holding::Piece { .. } => Hand::Placing(Held::again(&what)),
            Holding::Duplicate { .. } | Holding::Tray { .. } | Holding::Fixture { .. } => {
                Hand::Idle
            }
        };
        let root = build.room.root().to_string();
        let pending: Verb = match (&what, &landed.how) {
            // Refused above: a fixture is created as a row, never dropped.
            (Holding::Fixture { .. }, _) => return,
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
        build.hand = Hand::Extending(Box::new(Extending {
            from_node: node,
            from_socket: socket,
            reach,
            length_m,
        }));
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
        build.hand = Hand::Idle;
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
            layout_of(row.layout),
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

    /// Search the fixture library for the add-element dialog.
    pub(crate) fn stage_search_fixtures(&mut self, query: String, cx: &mut Context<Self>) {
        let pending = self.library.search_fixtures(&query, 0, 40);
        cx.spawn(async move |this, cx| {
            let found = pending.await;
            this.update(cx, |this, cx| {
                match found {
                    Ok(found) => {
                        if let Some(build) = this.build_mut() {
                            build.fixtures = found
                                .into_iter()
                                .map(|entry| {
                                    (
                                        format!("{} {}", entry.manufacturer, entry.model),
                                        entry.path,
                                    )
                                })
                                .collect();
                        }
                    }
                    // A library that will not answer is a state the dialog has
                    // to show: an empty list meaning "no such fixture" and one
                    // meaning "the index never built" are the same picture, and
                    // only one of them is the operator's problem.
                    Err(error) => {
                        if let Some(build) = this.build_mut() {
                            build.report = vec![error.to_string()];
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

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
    /// **Flip** inverts the subtree's handedness about its root socket, and it
    /// is written in the rows themselves rather than as an op: every relation
    /// inside the copy meets the mirrored socket
    /// ([`hand::mirror_socket`]), every roll changes sign, and every
    /// along-the-face offset is negated. What comes out is ordinary rows any
    /// other verb can edit afterwards — no node kind, no mirrored geometry, and
    /// nothing to keep in sync.
    ///
    /// A half turn about the joint would have been smaller and would have been
    /// wrong: handedness is a reflection, and the joints a wing actually bolts
    /// to (`TrussEnd`, `FloorCorner`) have `RollFreedom::Fixed`, so the
    /// resolver would clamp that turn to nothing and the button would do
    /// nothing but warn.
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
        // `u` runs along the host feature's tangent, so its sign is which side
        // of that feature a child sits on. Nothing else in the vocabulary is
        // handed: `v` is across, `trim` is up, and a span has no side.
        let mirrored = |params: &luma_scene::venue::Params| -> BTreeMap<String, f64> {
            params
                .iter()
                .map(|(key, value)| {
                    let value = if flip && key == "u" { -value } else { value };
                    (key.to_string(), value)
                })
                .collect()
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
            yaw: *yaw,
            params: mirrored(&source.params),
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
                their_socket: if flip {
                    hand::mirror_socket(&edge.their_socket)
                } else {
                    edge.their_socket.clone()
                },
                yaw: if flip { -edge.roll } else { edge.roll },
                params: mirrored(&node.params),
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
    /// The add-element dialog's keyboard cursor, when it is up.
    pub(crate) choosing: Option<usize>,
    /// Whether the sheet is *heading* open — what decides it takes clicks. A
    /// leaving sheet is painted and untouchable (`luma_ui::sheet`).
    pub(crate) inspector_open: bool,
    pub(crate) selected: Option<SelectedView>,
    /// Unplaced fixtures, as dialog rows.
    pub(crate) tray: Vec<(String, String)>,
    /// What the fixture library last answered with, as dialog rows.
    pub(crate) fixtures: Vec<(String, String)>,
    pub(crate) dangling: Vec<String>,
    pub(crate) warnings: Vec<String>,
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
}

impl Luma {
    /// The builder as the page draws it, or `None` when no room is up.
    pub(crate) fn stage_view(&self) -> Option<StageView> {
        let build = self.build_state()?;
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
            }
        });
        Some(StageView {
            choosing: build.hand.choosing().map(|c| c.cursor),
            inspector_open: build.inspector.target() > 0.0,
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
            fixtures: build.fixtures.clone(),
            dangling: build
                .solved
                .dangling
                .iter()
                .map(|d| format!("{} {}", build.label_of(&d.node_id), d.socket))
                .collect(),
            warnings: build.report.clone(),
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
    revealed: Pixels,
    window: &Window,
    cx: &App,
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
        .child(add_button(view, app))
        .children(
            view.choosing
                .map(|cursor| chooser(state, view, cursor, app, window, cx)),
        )
        .children(view.configuring.as_ref().map(|row| configure(row, app)))
        .children(state.menu.as_ref().map(|at| node_menu(*at, app)))
        .child(inspector(view, app, revealed))
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
fn add_button(view: &StageView, app: &Entity<Luma>) -> AnyElement {
    let open = app.clone();
    let unresolved = view.dangling.len() + view.warnings.len();
    let pin = app.clone();
    let mut bar = div()
        .absolute()
        .top(px(crate::visualizer::HEADER_HEIGHT + INSET))
        .left(px(INSET))
        .flex()
        .items_center()
        .gap(px(6.0))
        .occlude()
        .child(
            float::popover_card().p(px(4.0)).child(
                float::btn("Add element", "stage-add")
                    .id("stage-add")
                    .child(
                        Icon::new(IconName::Plus)
                            .size(px(13.0))
                            .text_color(ladder::foreground_alpha(0.7)),
                    )
                    .on_click(move |_, _, cx| {
                        open.update(cx, |this, cx| this.stage_open_chooser(cx));
                    })
                    .agent_node(Role::Button, "Add element"),
            ),
        );
    // The solve's complaints, as a count that opens the sheet. A number is all
    // the room has space for; the sentences are what the inspector is for.
    if unresolved > 0 {
        bar = bar.child(
            float::popover_card().p(px(4.0)).child(
                float::btn("", "stage-warnings")
                    .id("stage-warnings")
                    .px(px(8.0))
                    .child(float::badge(unresolved, ladder::status_warn().into()))
                    .on_click(move |_, _, cx| {
                        pin.update(cx, |this, cx| {
                            if let Some(build) = this.build_mut() {
                                build.warnings_pinned = !build.warnings_pinned;
                            }
                            cx.notify();
                        });
                    })
                    .agent_node(Role::Button, format!("Warnings {unresolved}")),
            ),
        );
    }
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

/// Every element this venue can be given, in one list.
///
/// Catalog pieces, the fixtures the patch has never placed, and the library —
/// three provenances, one question ("what goes in next"), so one list. They
/// were two menus and a search field before, which is three places to look for
/// one answer.
pub(crate) fn chooser_rows(view: &StageView) -> Vec<ChooserRow> {
    let mut rows: Vec<ChooserRow> = palette_rows()
        .into_iter()
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
    rows.extend(view.tray.iter().map(|(node, label)| ChooserRow {
        label: label.clone(),
        section: "Unplaced",
        take: Holding::Tray {
            node: node.clone(),
            label: label.clone(),
        },
    }));
    rows.extend(view.fixtures.iter().map(|(label, path)| ChooserRow {
        label: label.clone(),
        section: "Fixtures",
        take: Holding::Fixture {
            path: path.clone(),
            label: label.clone(),
            mode: None,
            width_m: DEFAULT_FIXTURE_WIDTH_M,
        },
    }));
    rows
}

/// The width a fixture is assumed to be until its definition lands. The same
/// fallback `luma_lib`'s `body_width_m` uses, so an un-fetched preview and a
/// fetched one differ only where the definition actually says something.
const DEFAULT_FIXTURE_WIDTH_M: f64 = 0.3;

/// Which rows a query leaves, in list order.
pub(crate) fn chooser_matches(rows: &[ChooserRow], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            needle.is_empty()
                || row.label.to_lowercase().contains(&needle)
                || row.section.to_lowercase().contains(&needle)
        })
        .map(|(at, _)| at)
        .collect()
}

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
        Holding::Tray { .. } => "Patched, never placed".to_string(),
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
    cx: &App,
) -> AnyElement {
    let rows = chooser_rows(view);
    let query = state.search.read(cx).text().to_string();
    let shown = chooser_matches(&rows, &query);
    let dismiss = app.clone();

    let mut list = float::list().id("stage-chooser-list").overflow_y_scroll();
    let mut section = "";
    for (at, &index) in shown.iter().enumerate() {
        let row = &rows[index];
        if row.section != section {
            section = row.section;
            list = list.child(float::section_heading(section.to_string()));
        }
        let app = app.clone();
        let take = row.take.clone();
        let label = row.label.clone();
        list = list.child(
            float::menu_row(RowState::of(false, at == cursor), label.clone())
                .id(gpui::SharedString::from(format!("choose:{index}")))
                .child(label.clone())
                .on_click(move |_, _, cx| {
                    let take = take.clone();
                    app.update(cx, |this, cx| this.stage_take(take, cx));
                })
                .agent_node(Role::Row, label),
        );
    }
    if shown.is_empty() {
        list = list.child(float::empty_row("Nothing matches"));
    }

    let keys = app.clone();
    let taken: Vec<Holding> = shown.iter().map(|&at| rows[at].take.clone()).collect();
    let body = div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground())
        // The list is walked from the frame, not from the field: the query is
        // short and edited with backspace, so the arrows belong to the rows —
        // the same call `luma_ui::float`'s pickers make.
        .on_key_down(move |event, _, cx| {
            let step: isize = match event.keystroke.key.as_str() {
                "down" => 1,
                "up" => -1,
                "enter" => {
                    let take = taken.get(cursor).cloned();
                    keys.update(cx, |this, cx| {
                        if let Some(take) = take {
                            this.stage_take(take, cx);
                        }
                    });
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
        .child(
            float::header_band().child(
                float::field()
                    .w_full()
                    .child(state.search.clone())
                    .agent_node(Role::Input, "Search elements"),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(chooser_preview(shown.get(cursor).map(|&at| &rows[at])))
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

    luma_ui::dialog::Host {
        id: "stage-chooser".into(),
        viewport: window.viewport_size(),
        focus: &state.focus,
        focused: true,
        label: "Add element dialog".into(),
        scrim_dismiss: luma_ui::dialog::ScrimDismiss::Enabled(Box::new(move |_, cx| {
            dismiss.update(cx, |this, cx| this.stage_escape(cx));
        })),
        closing: None,
    }
    .render(luma_ui::dialog::morph::fixed_card(
        "Add element card",
        CHOOSER_SIZE,
        body,
    ))
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
    card = card.child(float::arg_row(
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
    card = card.child(float::arg_row("Layout", track));

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
        card.into_any_element(),
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
fn node_menu(at: Point<Pixels>, app: &Entity<Luma>) -> AnyElement {
    let (dup, flip, detach, close) = (app.clone(), app.clone(), app.clone(), app.clone());
    luma_ui::menu::ContextMenu::new("stage-node-menu", at)
        .item("Duplicate", move |_, cx| {
            dup.update(cx, |this, cx| this.stage_duplicate(cx));
        })
        .item("Flip", move |_, cx| {
            flip.update(cx, |this, cx| this.stage_flip(cx));
        })
        .separator()
        .destructive("Detach", move |_, cx| {
            detach.update(cx, |this, cx| this.stage_detach(cx));
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
fn inspector(view: &StageView, app: &Entity<Luma>, revealed: Pixels) -> AnyElement {
    if revealed <= px(0.0) {
        return div().into_any_element();
    }
    let pad = px(luma_ui::sheet::PAD);
    let mut column = div()
        .id("stage-inspector-body")
        .size_full()
        .min_h_0()
        .overflow_y_scrollbar()
        .flex()
        .flex_col()
        .gap(px(14.0))
        .p(pad);
    if let Some(selected) = &view.selected {
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(ladder::foreground())
                        .child(selected.label.clone())
                        .agent_node(Role::Text, selected.label.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::foreground_alpha(0.5))
                        .child(format!("Gizmo: {}", selected.freedom.as_str()))
                        .agent_node(Role::Text, format!("Gizmo: {}", selected.freedom.as_str())),
                ),
        );
        // The one editable freedom the joint admits, as one control. A bolted
        // plate gets none, and gets no row rather than a disabled one.
        let editable = selected.freedom.param().map_or_else(
            || (selected.freedom == Freedom::Free).then_some(("trim", selected.trim, MAX_TRIM_M)),
            |key| Some((key, selected.param, if key == "u" { 1.0 } else { 360.0 })),
        );
        if let Some((key, value, max)) = editable {
            let app = app.clone();
            let node = selected.node.clone();
            let step = if key == "yaw" { 5.0 } else { 0.05 };
            column = column.child(float::arg_row(
                axis_title(key),
                float::scrub(
                    format!("stage-{key}"),
                    value,
                    0.0,
                    max,
                    step,
                    luma_ui::sheet::CONTENT_WIDTH,
                    move |value, _, cx| {
                        let node = node.clone();
                        app.update(cx, |this, cx| {
                            if key == "trim" {
                                this.stage_set_trim(value, cx);
                            } else {
                                this.stage_set_param(&node, key, value, cx);
                            }
                        });
                    },
                ),
            ));
        }
        column = column
            .children(selected.relation.clone().map(note))
            .children(selected.constraint.clone().map(note));
    }
    // Warnings last: a selection is what the hand is about to do, a warning is
    // a report on what it already did.
    if !view.dangling.is_empty() || !view.warnings.is_empty() {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(float::label("Unresolved"));
        for text in view.warnings.iter().chain(view.dangling.iter()) {
            block = block.child(
                div()
                    .text_size(px(11.5))
                    .text_color(ladder::status_warn())
                    .child(text.clone())
                    .agent_node(Role::Row, text.clone()),
            );
        }
        column = column.child(block);
    }
    luma_ui::sheet::Sheet {
        label: "Stage inspector".into(),
        width: luma_ui::sheet::WIDTH,
        revealed,
        interactive: view.inspector_open,
    }
    .render(column.into_any_element())
}

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
        if let Some(landed) = &held.landed {
            if let Some(at) = project(camera, size, landed.world.w_axis.truncate()) {
                out.push(Mark {
                    at,
                    label: format!("Ghost {}", held.what.label()),
                    refused: landed.refused.is_some(),
                });
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

/// Where the gap label sits, and what it says.
fn measurement_label(
    build: &Build,
    camera: &luma_scene::Camera,
    size: (f32, f32),
) -> Option<(Point<Pixels>, String, String)> {
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
    Some((
        at,
        run.measurement()?,
        hand::feet_and_inches(run.reach.as_ref()?.gap_m),
    ))
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
        let camera = *camera;
        let (width, height) = size;
        let mut surface = div()
            .id("stage-drop-surface")
            .absolute()
            .inset_0()
            .occlude();
        // Handlers only while the pointer is what aims — see
        // [`hand::Hand::aims_with_pointer`]. A run keeps the occluder and
        // loses the listeners.
        if build.hand.aims_with_pointer() {
            let drop = app.clone();
            let aim = app.clone();
            surface = surface
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
                });
        }
        layer = layer.child(surface.agent_node(Role::Card, "Stage drop surface"));
    }
    // Beads first, then everything that sits over them. gpui hit-tests in paint
    // order, so a control added before a bead is a control the bead swallows —
    // which is exactly what the run's length box did from under one.
    for bead in beads(build, camera, size) {
        let app = app.clone();
        let hover = app.clone();
        let (node, socket) = (bead.node.clone(), bead.socket.clone());
        let (hover_node, hover_socket) = (node.clone(), socket.clone());
        let (menu, menu_node) = (app.clone(), node.clone());
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
                .occlude()
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
                    let (node, at) = (menu_node.clone(), event.position);
                    menu.update(cx, |this, cx| {
                        if let Some(build) = this.build_mut() {
                            build.selected = Some(node);
                        }
                        this.stage_open_menu(at, cx);
                    });
                })
                .agent_node(Role::Button, bead.label),
        );
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
    if let Some((at, metres, feet)) = measurement_label(build, camera, size) {
        let refused = build
            .hand
            .extending()
            .and_then(Extending::refused)
            .is_some();
        let length = build.hand.extending().map_or(0.0, |run| run.length_m);
        let (set, commit) = (app.clone(), app.clone());
        layer = layer.child(
            div().absolute().left(at.x).top(at.y).occlude().child(
                float::popover_card()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .p(px(4.0))
                    .child(
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
                            .agent_node(Role::Text, format!("{metres} · {feet}")),
                    )
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
                    out.ghosts.push(Ghost {
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
