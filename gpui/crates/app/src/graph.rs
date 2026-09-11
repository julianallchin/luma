//! Native graph canvas. Geometry is measured once per document change; pointer
//! gestures use the same sockets and wire paths that are painted.

mod controls;
mod interaction;
pub(crate) mod preview;
mod score;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::node::{agent_paint_node, agent_paint_node_focused, Instrument, Role};
use luma_ui::{ladder, paint};

use luma_lib::models::node_graph::{Graph, NodeTypeDef, PortDef};

use crate::shell::Body as TabBody;
use crate::tabs::Target;
use crate::Luma;

// -- state --------------------------------------------------------------------

/// The track a graph is evaluated against.
///
/// Resolved from the workspace when the tab opens and re-pointed when the same
/// pattern is opened from another track — never absent, because the doors that
/// cannot resolve one are inert (§6/§9 ruling 1 of
/// `docs/design/graph-editor-interaction.md`). That is what makes every open
/// graph tab preview-capable by construction: `run_graph` needs a track and a
/// venue, and an [`Editor`] cannot exist without both.
#[derive(Clone)]
pub(crate) struct TrackContext {
    pub(crate) track: String,
    /// Venue supplying the heads for the output preview.
    pub(crate) venue: String,
    /// For the toolbar readout — ids are for the seam, not the eye.
    pub(crate) track_name: SharedString,
}

/// The screen's whole state: the pattern it is editing, the document it read,
/// the resolved geometry of that document, and where the eye and the hand are.
pub struct Editor {
    source: Box<score::ScoreGraph>,
    /// See [`TrackContext`]. A field rather than part of the tab's identity:
    /// the tab stays keyed on the pattern alone, so one document never has two
    /// writers racing through the CAS.
    context: TrackContext,
    /// The node catalogue, keyed by `typeId`. A graph is only titles and ports
    /// once this is in hand, so the screen draws nothing until it is.
    types: Rc<HashMap<String, NodeTypeDef>>,
    inspection: Vec<(String, Rc<Graph>)>,
    /// Geometry derived from the projected graph and [`Self::types`], rebuilt
    /// on every change to either and measured once per rebuild. Behind a
    /// `RefCell` because the measure pass needs a text system and therefore
    /// has to run inside the frame — see the module docs.
    scene: Rc<RefCell<Scene>>,
    /// The nodes the keyboard verbs act on. A set, not an option: delete,
    /// undo's snapshot and the marquee all want "these nodes", and widening
    /// later would have meant widening every reader twice.
    selected: Vec<SharedString>,
    selected_edge: Option<SharedString>,
    gesture: Option<Gesture>,
    /// Where the eye is. A `Cell` for the same reason [`Self::origin`] is one:
    /// the first framing is a `fitView`, and a fit cannot be computed until
    /// the canvas knows both its own size and the measured graph's — which is
    /// inside a draw.
    view: Rc<Cell<Viewport>>,
    /// Frame the whole graph, as the web editor does with `fitView` on mount
    /// — and keep it framed when the *canvas* changes size (the workspace
    /// pane resizing under it), until the user takes the view with a pan or a
    /// zoom. Cleared by those gestures, not by the fit itself, so a rebuild
    /// (a save coming back) or a pane resize does not yank an eye the user
    /// has placed.
    fit: bool,
    /// The canvas size the last fit was computed for — how a resize is told
    /// apart from a repaint. Written inside the draw, like [`Self::view`].
    fitted_size: Rc<Cell<gpui::Size<Pixels>>>,
    /// Where the canvas last painted, in window space. A mouse event arrives
    /// in window coordinates and has to be put back into graph coordinates,
    /// which needs this; the canvas knows it and the event handlers do not, so
    /// the canvas writes it down each frame.
    origin: Rc<Cell<Point<Pixels>>>,
    canvas_size: Rc<Cell<Size<Pixels>>>,
    error: Option<String>,
    preview_error: Option<String>,
    preview_generation: u64,
    preview_running: bool,
}

/// What the pointer is doing between a press and a release.
///
/// A gesture is anchored to the tab it started in: every handler
/// [`listen`] registers carries that tab's [`Target`] and routes through
/// [`Luma::edit_graph_tab`], so switching tabs mid-drag cannot strand a
/// gesture in a tab the handlers can no longer see (§11.5 of the design doc).
enum Gesture {
    Wire(interaction::WireDrag),
    /// Moving the eye. `last` is the previous pointer position, so the pan
    /// follows the pointer exactly regardless of zoom.
    Pan {
        last: Point<Pixels>,
    },
    /// Moving the selection. `grab` is where inside the pressed card the
    /// pointer took hold, in graph space, so the card does not jump to centre
    /// itself on the pointer. `moved` distinguishes a drag from a click that
    /// selected. `initial` is where every carried node stood at the press —
    /// the whole selection moves by one shared delta, as the track editor's
    /// clips do.
    Move {
        node: SharedString,
        grab: Point<f32>,
        moved: bool,
        initial: Vec<(SharedString, Point<f32>)>,
    },
    /// Sweeping a selection rect, both corners in graph space. The selection
    /// is recomputed live on every drag step — rect-intersect over
    /// [`Scene::cards`] — and the rect is painted as a 1px
    /// [`ladder::primary`] outline, nothing filled, nothing animated.
    Marquee {
        from: Point<f32>,
        to: Point<f32>,
    },
}

/// Where the eye is: a pan in window pixels and a zoom about it.
#[derive(Clone, Copy)]
struct Viewport {
    pan: Point<Pixels>,
    zoom: f32,
}

impl Viewport {
    /// Deeply composed graphs still need to fit as an overview. Zooming in
    /// reveals their labels and controls; the fit never clips distant nodes.
    const MIN_ZOOM: f32 = 0.02;
    const MAX_ZOOM: f32 = 4.;
    /// Slack left around a fitted graph, as a fraction of the canvas — React
    /// Flow's `fitView` padding default.
    const FIT_PADDING: f32 = 0.1;

    fn to_window(self, origin: Point<Pixels>, at: Point<f32>) -> Point<Pixels> {
        point(
            origin.x + self.pan.x + px(at.x * self.zoom),
            origin.y + self.pan.y + px(at.y * self.zoom),
        )
    }

    fn to_graph(self, origin: Point<Pixels>, at: Point<Pixels>) -> Point<f32> {
        point(
            f32::from(at.x - origin.x - self.pan.x) / self.zoom,
            f32::from(at.y - origin.y - self.pan.y) / self.zoom,
        )
    }

    /// Scale about `anchor`, keeping whatever is under it exactly where it is.
    fn zoom_about(&mut self, origin: Point<Pixels>, anchor: Point<Pixels>, factor: f32) {
        let held = self.to_graph(origin, anchor);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.pan = point(
            anchor.x - origin.x - px(held.x * self.zoom),
            anchor.y - origin.y - px(held.y * self.zoom),
        );
    }

    /// Frame a graph `size` units across inside `canvas`, centred, never
    /// magnified past 1:1 — the same three rules `fitView` follows. `at` is
    /// the graph-space top-left of everything being framed.
    fn fit(canvas: Size<Pixels>, at: Point<f32>, extent: Size<f32>) -> Self {
        if extent.width <= 0. || extent.height <= 0. {
            return Self {
                pan: point(px(0.), px(0.)),
                zoom: 1.,
            };
        }
        let (width, height) = (f32::from(canvas.width), f32::from(canvas.height));
        let room = 1. - Self::FIT_PADDING * 2.;
        let zoom = (width * room / extent.width)
            .min(height * room / extent.height)
            .clamp(Self::MIN_ZOOM, 1.);
        Self {
            pan: point(
                px((width - extent.width * zoom) / 2. - at.x * zoom),
                px((height - extent.height * zoom) / 2. - at.y * zoom),
            ),
            zoom,
        }
    }

    fn card_box(self, origin: Point<Pixels>, card: &Card) -> Bounds<Pixels> {
        Bounds {
            origin: self.to_window(origin, card.origin),
            size: size(px(card.width * self.zoom), px(card.height * self.zoom)),
        }
    }
}

impl Editor {
    /// The pattern being edited. The window title is the only reader outside
    /// this module.
    pub(crate) fn pattern_name(&self) -> &str {
        &self.source.label
    }

    /// Rebuild the resolved geometry from the document. Called on load, and
    /// when a save hands back a canonicalized graph — but *not* on a node
    /// move, which writes one card's origin in place, because a rebuild would
    /// throw away the measure pass along with it.
    fn shown_graph(&self) -> Option<&Graph> {
        self.inspection
            .last()
            .map(|(_, graph)| graph.as_ref())
            .or(Some(self.source.view.as_ref()))
    }

    fn rebuild(&mut self) {
        let scene = self
            .shown_graph()
            .map(|graph| Scene::build(graph, &self.types))
            .unwrap_or_default();
        self.selected
            .retain(|id| scene.cards.iter().any(|card| card.node_id == *id));
        self.selected_edge
            .take_if(|id| !scene.links.iter().any(|link| link.id == *id));
        *self.scene.borrow_mut() = scene;
    }

    fn inspect_node(&mut self, node: &str) -> bool {
        let Some(instance) = self
            .shown_graph()
            .and_then(|graph| graph.nodes.iter().find(|n| n.id == node))
        else {
            return false;
        };
        let Some(definition) = instance
            .type_id
            .strip_prefix(luma_lib::node_graph::lighting::PREFIX)
        else {
            return false;
        };
        let graph =
            luma_lib::node_graph::lighting::project_definition(&self.source.library, definition);
        let Some(graph) = graph else {
            return false;
        };
        self.inspection
            .push((definition.to_owned(), Rc::new(graph)));
        self.reframe();
        true
    }

    fn reframe(&mut self) {
        self.gesture = None;
        self.selected.clear();
        self.selected_edge = None;
        self.fit = true;
        self.fitted_size.set(gpui::Size::default());
        self.rebuild();
    }

    /// Move one node to `origin` in graph space, in both the document and the
    /// geometry drawn from it. Wires follow for free: a wire is stored as the
    /// two ports it joins, not as two points.
    ///
    /// During a drag, only the projected positions move. The release commits
    /// them to the owning score as one undoable edit.
    fn move_node(&mut self, node: &SharedString, origin: Point<f32>) {
        if self.inspecting_builtin() {
            return;
        }
        let source = &mut self.source;

        let view = self
            .inspection
            .last_mut()
            .map(|(_, graph)| graph)
            .unwrap_or(&mut source.view);
        let Some(instance) = Rc::make_mut(view)
            .nodes
            .iter_mut()
            .find(|instance| instance.id == node.as_ref())
        else {
            return;
        };
        instance.position_x = Some(f64::from(origin.x));
        instance.position_y = Some(f64::from(origin.y));

        if let Some(card) = self
            .scene
            .borrow_mut()
            .cards
            .iter_mut()
            .find(|card| &card.node_id == node)
        {
            card.origin = origin;
        }
    }

    /// Add `node` to the selection, or take it back out — a shift-click.
    fn toggle_selected(&mut self, node: SharedString) {
        if let Some(at) = self.selected.iter().position(|id| id == &node) {
            self.selected.remove(at);
        } else {
            self.selected.push(node);
        }
    }
}

// -- navigation and gestures --------------------------------------------------
//
// These hang off `Luma` because opening a pattern is a pair of `Library` calls
// plus a screen transition, and `Luma` owns both.

/// Why the graph doors are inert without a track (§6/§9 ruling 1). One
/// spelling, stated wherever a door is drawn — the pattern rows and the
/// new-tab Pattern choice — so the two surfaces cannot drift.
pub(crate) const NO_TRACK_REASON: &str = "Open a track to edit patterns";

impl Luma {
    /// The track context the graph doors resolve against: the active tab when
    /// it is a track editor, else the strip's remaining track-editor tab.
    /// `None` is what makes the doors inert (§6) — every caller states the
    /// reason where it draws.
    ///
    /// "Most recently active" from the design doc collapses here: the strip
    /// is scoped per track (see `workspace.rs`), so it holds at most one
    /// track-editor tab in practice, and the last one in strip order is that
    /// tab.
    pub(crate) fn graph_track_context(&self) -> Option<TrackContext> {
        let active = self.workspace.active().cloned();
        let mut fallback = None;
        for tab in self.workspace.iter() {
            if let (Target::TrackEditor { track, venue }, TabBody::TrackEditor(state)) =
                (&tab.target, &tab.body)
            {
                let context = TrackContext {
                    track: track.clone(),
                    venue: venue.clone(),
                    track_name: state.track_name().to_string().into(),
                };
                if Some(&tab.target) == active.as_ref() {
                    return Some(context);
                }
                fallback = Some(context);
            }
        }
        fallback
    }

    /// Run `edit` against one graph tab's editor, wherever it sits in the
    /// strip. The async loads come through here so a document landing late
    /// cannot write into whichever tab happens to be visible.
    fn edit_graph_tab(
        &mut self,
        target: &Target,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Editor),
    ) {
        if let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) {
            edit(editor);
            cx.notify();
        }
    }

    /// A press on the canvas: take hold of a port, a card, or the background.
    ///
    /// `target` is the tab the canvas belongs to — every gesture handler is
    /// addressed, not "whatever tab is visible", so a tab switch mid-gesture
    /// cannot strand a gesture (see [`Gesture`]).
    fn graph_press(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        shift: bool,
        clicks: usize,
        cx: &mut Context<Self>,
    ) {
        if self.graph_wire_press(target, at, cx) {
            return;
        }
        self.edit_graph_tab(target, cx, |editor| {
            let origin = editor.origin.get();
            let view = editor.view.get();
            let cursor = view.to_graph(origin, at);
            let scene = Rc::clone(&editor.scene);
            let scene = scene.borrow();
            editor.selected_edge = None;
            {
                let source = &mut editor.source;
                source.selected_output = None;
            }
            match scene.hit(cursor, view.zoom) {
                // Phase 1 of the design doc: a port press names its node. The
                // the graph currently selects the card behind a port.
                Hit::Port { card, .. } => {
                    editor.selected = vec![scene.cards[card].node_id.clone()];
                }
                Hit::Header { card } | Hit::Body { card } => {
                    let node = scene.cards[card].node_id.clone();
                    if clicks >= 2 {
                        drop(scene);
                        if editor.inspect_node(&node) {
                            return;
                        }
                        editor.selected = vec![node];
                        return;
                    }
                    if editor.inspecting_builtin() {
                        editor.selected = vec![node];
                        return;
                    }
                    if shift {
                        editor.toggle_selected(node);
                        return;
                    }
                    // A press on an unselected card selects that card alone; a
                    // press on a selected one carries the whole selection.
                    if !editor.selected.contains(&node) {
                        editor.selected = vec![node.clone()];
                    }
                    let grab = point(
                        cursor.x - scene.cards[card].origin.x,
                        cursor.y - scene.cards[card].origin.y,
                    );
                    let initial = scene
                        .cards
                        .iter()
                        .filter(|card| editor.selected.contains(&card.node_id))
                        .map(|card| (card.node_id.clone(), card.origin))
                        .collect();
                    editor.gesture = Some(Gesture::Move {
                        node,
                        grab,
                        moved: false,
                        initial,
                    });
                }
                Hit::Wire { link } => {
                    editor.selected.clear();
                    editor.selected_edge = Some(scene.links[link].id.clone());
                }
                Hit::Empty => {
                    if shift {
                        editor.gesture = Some(Gesture::Marquee {
                            from: cursor,
                            to: cursor,
                        });
                    } else {
                        // A press on the background clears the selection as
                        // well as starting a pan: the web editor does the
                        // same, and a selection that survived a click
                        // elsewhere would be a second way to have one.
                        editor.selected.clear();
                        editor.fit = false;
                        editor.gesture = Some(Gesture::Pan { last: at });
                    }
                }
            }
        });
    }

    /// A pointer move. Registered on the window rather than on the canvas, so
    /// it arrives for every move over the app whether or not a gesture is
    /// running — hence the early return, which is what keeps an idle mouse
    /// from notifying (and so redrawing) once per event.
    fn graph_drag(&mut self, target: &Target, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.graph_wire_drag(target, at, cx) {
            return;
        }
        match self.workspace.body_mut(target) {
            Some(TabBody::Graph(editor)) if editor.gesture.is_some() => {}
            _ => return,
        }
        let mut moves: Vec<(SharedString, Point<f32>)> = Vec::new();
        self.edit_graph_tab(target, cx, |editor| {
            let origin = editor.origin.get();
            let mut view = editor.view.get();
            let mut sweep = None;
            match &mut editor.gesture {
                Some(Gesture::Pan { last }) => {
                    let delta = point(at.x - last.x, at.y - last.y);
                    *last = at;
                    view.pan = point(view.pan.x + delta.x, view.pan.y + delta.y);
                    editor.view.set(view);
                }
                Some(Gesture::Move {
                    node,
                    grab,
                    moved: dragged,
                    initial,
                }) => {
                    *dragged = true;
                    let cursor = view.to_graph(origin, at);
                    let held = point(cursor.x - grab.x, cursor.y - grab.y);
                    // One delta for the whole selection, measured against the
                    // pressed card, so the group cannot shear.
                    if let Some((_, from)) = initial.iter().find(|(id, _)| id == node) {
                        let delta = point(held.x - from.x, held.y - from.y);
                        moves = initial
                            .iter()
                            .map(|(id, from)| {
                                (id.clone(), point(from.x + delta.x, from.y + delta.y))
                            })
                            .collect();
                    }
                }
                Some(Gesture::Marquee { from, to }) => {
                    *to = view.to_graph(origin, at);
                    sweep = Some((*from, *to));
                }
                Some(Gesture::Wire(_)) | None => {}
            }
            if let Some((a, b)) = sweep {
                let scene = Rc::clone(&editor.scene);
                let scene = scene.borrow();
                editor.selected = scene
                    .cards
                    .iter()
                    .filter(|card| card.intersects(a, b))
                    .map(|card| card.node_id.clone())
                    .collect();
            }
        });
        if !moves.is_empty() {
            self.edit_graph_tab(target, cx, |editor| {
                for (node, origin) in &moves {
                    editor.move_node(node, *origin);
                }
            });
        }
    }

    /// A release. A node that actually moved is written back; a press that
    /// only selected is not, because the document did not change.
    ///
    /// Registered on the window like [`Self::graph_drag`], and guarded the same
    /// way: a click anywhere else in the app must not redraw this screen.
    fn graph_release(&mut self, target: &Target, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.graph_wire_release(target, at, cx) {
            return;
        }
        match self.workspace.body_mut(target) {
            Some(TabBody::Graph(editor)) if editor.gesture.is_some() => {}
            _ => return,
        }
        let mut positions: Option<std::collections::BTreeMap<String, [f64; 2]>> = None;
        self.edit_graph_tab(target, cx, |editor| {
            if let Some(Gesture::Move { moved, initial, .. }) = editor.gesture.take() {
                if moved {
                    {
                        positions = Some(
                            editor
                                .scene
                                .borrow()
                                .cards
                                .iter()
                                .filter(|card| initial.iter().any(|(id, _)| *id == card.node_id))
                                .map(|card| {
                                    (
                                        card.node_id.to_string(),
                                        [f64::from(card.origin.x), f64::from(card.origin.y)],
                                    )
                                })
                                .collect(),
                        );
                    }
                }
            }
        });
        if let Some(positions) = positions {
            let (mut nodes, mut inputs) = (
                std::collections::BTreeMap::new(),
                std::collections::BTreeMap::new(),
            );
            for (id, at) in positions {
                if let Some(key) = luma_lib::node_graph::lighting::input_node_key(&id) {
                    inputs.insert(key.to_string(), at);
                } else {
                    nodes.insert(id, at);
                }
            }
            let mut edits = Vec::new();
            if !nodes.is_empty() {
                edits.push(luma_patterns::GraphEdit::Move { positions: nodes });
            }
            if !inputs.is_empty() {
                edits.push(luma_patterns::GraphEdit::MoveInputs { positions: inputs });
            }
            self.apply_score_graph_edits(target, edits, cx);
        }
    }

    fn graph_zoom(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        wheel: f32,
        cx: &mut Context<Self>,
    ) {
        self.edit_graph_tab(target, cx, |editor| {
            editor.fit = false;
            let origin = editor.origin.get();
            let mut view = editor.view.get();
            // Exponential in the scroll distance, so a fast flick and a slow
            // one over the same distance land in the same place.
            view.zoom_about(origin, at, (wheel * ZOOM_PER_PIXEL).exp());
            editor.view.set(view);
        });
    }

    /// The visible tab, when it is a graph editor — what a keyboard verb acts
    /// on. The bindings are scoped to the `Graph` key context so this is a
    /// guard, not a branch.
    fn active_graph_target(&self) -> Option<Target> {
        self.workspace
            .active()
            .filter(|target| matches!(target, Target::ScoreGraph { .. }))
            .cloned()
    }

    /// Delete selected nodes or edges through the owning score's history.
    pub(crate) fn graph_delete(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        {
            let edits = match self.workspace.body(&target) {
                Some(TabBody::Graph(editor)) if !editor.inspecting_builtin() => {
                    let mut edits: Vec<_> = editor
                        .selected
                        .iter()
                        .filter(|id| id.as_ref() != luma_lib::node_graph::lighting::OUTPUTS_NODE)
                        .map(
                            |id| match luma_lib::node_graph::lighting::input_node_key(id) {
                                Some(key) => {
                                    luma_patterns::GraphEdit::RemoveInput { key: key.into() }
                                }
                                None => luma_patterns::GraphEdit::Remove { id: id.to_string() },
                            },
                        )
                        .collect();
                    // An output stays bound; the wire into the outputs card is
                    // replaced, never removed.
                    if let Some(edge) = editor.selected_edge.as_ref().and_then(|id| {
                        editor
                            .shown_graph()?
                            .edges
                            .iter()
                            .find(|edge| edge.id == id.as_ref())
                            .filter(|edge| {
                                edge.to_node != luma_lib::node_graph::lighting::OUTPUTS_NODE
                            })
                    }) {
                        edits.push(luma_patterns::GraphEdit::Bind {
                            node: edge.to_node.clone(),
                            input: edge.to_port.clone(),
                            binding: None,
                        });
                    }
                    edits
                }
                _ => return,
            };
            self.apply_score_graph_edits(&target, edits, cx);
        }
    }

    /// `Cmd+Z` / `Cmd+Shift+Z`: step the document back, or forward again.
    /// An undo is a write like any other — it goes through the same save.
    pub(crate) fn graph_undo(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.active_graph_target() {
            self.step_score_graph_history(&target, false, cx);
        }
    }

    pub(crate) fn graph_redo(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.active_graph_target() {
            self.step_score_graph_history(&target, true, cx);
        }
    }
}

// -- geometry -----------------------------------------------------------------
//
// One card shape, in graph space. Every constant here is the value
// `getComputedStyle` reports on the web card at zoom 1; the Tailwind class it
// is spelled with is named beside it, because the two disagree often enough
// (`rounded-lg` under `--radius: 0rem`, `h-7` on a control the app otherwise
// keeps at `h-6`) that only one of them can be the source.

/// `min-w-[170px]`, as a border-box minimum.
const CARD_MIN_WIDTH: f32 = 170.;
/// `border-2 border-gutter`, all four sides. The card's outer box includes it,
/// so the content box starts `CARD_BORDER` in.
const CARD_BORDER: f32 = 2.;
/// The `bg-trim` title strip: `px-2 pt-1 pb-1` around a 12px/16px line.
const HEADER_HEIGHT: f32 = 24.;
/// `text-xs` — the title, and the port labels under it, are the same
/// 12px/16px run.
const TEXT_SIZE: f32 = 12.;
const TEXT_LINE: f32 = 16.;
/// `tracking-tight` at 12px.
const TITLE_TRACKING: f32 = -0.3;
/// `pt-1` / `pb-1` on the header, `py-1` on the port and param blocks, `mb-1`
/// under a param label. One 4px step, four uses; `PAD_H` is its `px-2` twin.
const PAD: f32 = 4.;
const PAD_H: f32 = 8.;
/// `gap-1.5` between port rows within a column.
const PORT_ROW_GAP: f32 = 6.;
/// `pl-4` / `pr-4` — the outer padding of a port row, past its ring.
const PORT_INSET: f32 = 16.;
/// `pr-2` / `pl-2` — the inner padding, between a label and the other column.
const PORT_LABEL_PAD: f32 = 8.;
/// `gap-2` — the minimum gutter between the two port columns, and between a
/// selector's label and its chevron.
const COLUMN_GAP: f32 = 8.;
/// Distance from the card's *content* edge to a port's centre. Everything a
/// port draws is centred on this one anchor, exactly as `PORT_ANCHOR` is on
/// the web — which is what makes a wire land in the dot rather than beside it.
const PORT_ANCHOR: f32 = 6.;
const PORT_RING: f32 = 9.;
/// The square a port answers a press from, centred on its anchor. Much larger
/// than the painted ring on purpose: a 16px grab box is the difference between
/// "wiring works" and "wiring is a dexterity test".
const PORT_GRAB: f32 = 16.;
/// How far from a wire's centreline a press still takes the wire, in *window*
/// pixels — a hit is a pointing gesture, so its slack follows the screen, not
/// the zoom. [`Scene::hit`] divides by the zoom to get graph units.
const WIRE_GRAB: f32 = 6.;
const PORT_RING_BORDER: f32 = 1.5;
const PORT_DOT: f32 = 4.;
/// The faint lead-in bar from the card edge to the anchor, drawn only on a
/// wired port so the wire does not appear to stop at the border.
const PORT_GHOST_H: f32 = 2.;
const PORT_GHOST_ALPHA: f32 = 0.4;
/// The horizontal run a wire leaves a port on before it turns.
const WIRE_STUB: f32 = 16.;
/// The corner radius where that stub meets the diagonal.
const WIRE_FILLET: f32 = 10.;
const WIRE_WIDTH: f32 = 2.;
/// Below this zoom, hide labels while keeping topology visible.
const LABEL_FLOOR: f32 = 0.3;
/// Scroll-to-zoom rate, per logical pixel of wheel travel.
const ZOOM_PER_PIXEL: f32 = 0.004;

/// The graph with every position, colour and connection resolved: what the
/// canvas draws and what the pointer hits, in graph space.
#[derive(Default)]
struct Scene {
    cards: Vec<Card>,
    /// A wire, as the two ports it joins. Indices rather than points so that
    /// moving a card moves its wires without anything having to say so.
    links: Vec<Link>,
    /// Widths and heights have been resolved against a text system. False
    /// until the first frame after a rebuild.
    measured: bool,
}

struct Card {
    interface_input: bool,
    /// The document says where this card goes. Otherwise [`Scene::layout`]
    /// decides, once the card has a size.
    placed: bool,
    node_id: SharedString,
    title: SharedString,
    origin: Point<f32>,
    /// Outer box, borders included. Resolved by [`Scene::measure`].
    width: f32,
    height: f32,
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    /// The card's interactive regions, card-local, in resolution order —
    /// ports first (their grab boxes out-rank everything they overlap), then
    /// the header. Resolved by [`Scene::measure`], which is
    /// the pass that already knows every box; the interaction layer costs
    /// zero per-frame work. Whatever no region claims is [`Hit::Body`].
    regions: Vec<Region>,
}

impl Card {
    fn contains(&self, at: Point<f32>) -> bool {
        at.x >= self.origin.x
            && at.x <= self.origin.x + self.width
            && at.y >= self.origin.y
            && at.y <= self.origin.y + self.height
    }

    /// Does the card's box cross the rect spanned by `a` and `b` (any two
    /// opposite corners)? The marquee's question, asked in graph space.
    fn intersects(&self, a: Point<f32>, b: Point<f32>) -> bool {
        let (left, right) = (a.x.min(b.x), a.x.max(b.x));
        let (top, bottom) = (a.y.min(b.y), a.y.max(b.y));
        self.origin.x <= right
            && self.origin.x + self.width >= left
            && self.origin.y <= bottom
            && self.origin.y + self.height >= top
    }

    /// Rebuild [`Card::regions`] from resolved geometry. Called at the end of
    /// the measure pass — everything here (port anchors, slot ys, the card's
    /// final width) exists only once measuring is done.
    fn resolve_regions(&mut self) {
        let mut regions = Vec::new();
        for (ports, output, word) in [
            (&self.inputs, false, "input"),
            (&self.outputs, true, "output"),
        ] {
            for (index, port) in ports.iter().enumerate() {
                regions.push(Region {
                    origin: point(port.at.x - PORT_GRAB / 2., port.at.y - PORT_GRAB / 2.),
                    size: size(PORT_GRAB, PORT_GRAB),
                    kind: RegionKind::Port {
                        port: index,
                        output,
                    },
                    label: format!("{} {word} {}", self.node_id, port.id).into(),
                });
            }
        }
        regions.push(Region {
            origin: point(0., 0.),
            size: size(self.width, CARD_BORDER + HEADER_HEIGHT),
            kind: RegionKind::Header,
            label: self.title.clone(),
        });
        self.regions = regions;
    }
}

/// One interactive region of a card, in card-local graph units.
///
/// Carries its harness label so the geometry and the name a script finds it by
/// are minted in one place ([`Scene::measure`]) from the same port and param
/// structs the paint reads — the label cannot drift from the thing.
struct Region {
    origin: Point<f32>,
    size: Size<f32>,
    kind: RegionKind,
    /// `"{node} input {port}"` / `"{node} output {port}"` /
    /// `"{node} param {id}"` — how a script says "the phase input of osc_1".
    label: SharedString,
}

impl Region {
    fn contains(&self, at: Point<f32>) -> bool {
        at.x >= self.origin.x
            && at.x <= self.origin.x + self.size.width
            && at.y >= self.origin.y
            && at.y <= self.origin.y + self.size.height
    }
}

enum RegionKind {
    Port { port: usize, output: bool },
    Header,
}

/// What the pointer is over, resolved by [`Scene::hit`]: the one question
/// every press, hover and future gesture asks of the canvas.
enum Hit {
    Port {
        card: usize,
        port: usize,
        output: bool,
    },
    Header {
        card: usize,
    },
    Body {
        card: usize,
    },
    Wire {
        link: usize,
    },
    Empty,
}

struct Port {
    id: SharedString,
    label: SharedString,
    color: Rgba,
    /// Centre of the port, as an offset from the card's outer origin. The `x`
    /// half is only known once the card has a width.
    at: Point<f32>,
    /// Drawn as a dot inside its ring when something is wired to it, and as a
    /// bare ring when nothing is.
    connected: bool,
}

struct Link {
    id: SharedString,
    from: (usize, usize),
    to: (usize, usize),
    color: Rgba,
}

impl Scene {
    fn build(graph: &Graph, types: &HashMap<String, NodeTypeDef>) -> Self {
        let wired: HashSet<(&str, &str, bool)> = graph
            .edges
            .iter()
            .flat_map(|edge| {
                [
                    (edge.from_node.as_str(), edge.from_port.as_str(), true),
                    (edge.to_node.as_str(), edge.to_port.as_str(), false),
                ]
            })
            .collect();

        let input_nodes: HashSet<_> = graph
            .nodes
            .iter()
            .filter(|n| n.type_id == "pattern_args")
            .map(|n| n.id.as_str())
            .collect();
        let cards: Vec<Card> = graph
            .nodes
            .iter()
            .filter(|node| node.type_id != "pattern_args")
            .map(|instance| {
                let definition = types.get(&instance.type_id);
                let ports = |defs: &[PortDef], output: bool| {
                    defs.iter()
                        .map(|port| Port {
                            id: port.id.clone().into(),
                            label: if !output {
                                graph
                                    .edges
                                    .iter()
                                    .find(|edge| {
                                        edge.to_node == instance.id
                                            && edge.to_port == port.id
                                            && input_nodes.contains(edge.from_node.as_str())
                                    })
                                    .map(|edge| {
                                        let name = graph
                                            .args
                                            .iter()
                                            .find(|arg| arg.id == edge.from_port)
                                            .map(|arg| arg.name.as_str())
                                            .unwrap_or(&edge.from_port);
                                        if name == port.name || name == port.id {
                                            format!("{} · exposed", port.name)
                                        } else {
                                            format!("{} · {name}", port.name)
                                        }
                                    })
                                    .unwrap_or_else(|| port.name.clone())
                                    .into()
                            } else {
                                port.name.clone().into()
                            },
                            color: ladder::port(port.port_type.key()),
                            at: point(0., 0.),
                            connected: wired.contains(&(
                                instance.id.as_str(),
                                port.id.as_str(),
                                output,
                            )),
                        })
                        .collect::<Vec<_>>()
                };
                Card {
                    interface_input: luma_lib::node_graph::lighting::input_node_key(&instance.id)
                        .is_some(),
                    placed: instance.position_x.is_some() && instance.position_y.is_some(),
                    node_id: instance.id.clone().into(),
                    // A type the catalogue does not know still gets a card: it
                    // is in the document, so hiding it would be a graph the
                    // editor cannot show and cannot fix.
                    title: definition
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| instance.type_id.clone())
                        .into(),
                    origin: point(
                        instance.position_x.unwrap_or(0.) as f32,
                        instance.position_y.unwrap_or(0.) as f32,
                    ),
                    width: CARD_MIN_WIDTH,
                    height: 0.,
                    inputs: definition
                        .map(|d| ports(&d.inputs, false))
                        .unwrap_or_default(),
                    outputs: definition
                        .map(|d| ports(&d.outputs, true))
                        .unwrap_or_default(),
                    regions: Vec::new(),
                }
            })
            .collect();

        let index: HashMap<&str, usize> = cards
            .iter()
            .enumerate()
            .map(|(at, card)| (card.node_id.as_ref(), at))
            .collect();
        let find = |ports: &[Port], id: &str| ports.iter().position(|port| port.id == id);
        // An edge naming a port the node does not have draws nothing — the
        // document is ahead of the catalogue, and half a wire would be a worse
        // answer than none.
        let links = graph
            .edges
            .iter()
            .filter_map(|edge| {
                let from_card = *index.get(edge.from_node.as_str())?;
                let to_card = *index.get(edge.to_node.as_str())?;
                let from_port = find(&cards[from_card].outputs, &edge.from_port)?;
                let to_port = find(&cards[to_card].inputs, &edge.to_port)?;
                Some(Link {
                    id: edge.id.clone().into(),
                    from: (from_card, from_port),
                    to: (to_card, to_port),
                    // The wire carries the *source*'s hue, as on the web side:
                    // an edge is one signal, and the port it came out of is
                    // what says which kind.
                    color: cards[from_card].outputs[from_port].color,
                })
            })
            .collect();

        Self {
            cards,
            links,
            measured: false,
        }
    }

    /// Resolve every width and height that depends on shaped text, once.
    ///
    /// The web card is shrink-to-fit, so its width is `max(170, max-content)`
    /// over the port columns and the body — see the module docs for why that
    /// cannot be guessed at.
    fn measure(&mut self, window: &Window) {
        for card in &mut self.cards {
            if card.interface_input {
                card.width = CARD_MIN_WIDTH
                    .max(run_width(&card.title, TEXT_SIZE, FontWeight::MEDIUM, window) + 40.);
                card.height = HEADER_HEIGHT + CARD_BORDER * 2.;
                for port in &mut card.outputs {
                    port.at = point(card.width - CARD_BORDER - PORT_ANCHOR, card.height / 2.);
                }
                card.resolve_regions();
                continue;
            }
            let widest = |ports: &[Port]| {
                ports
                    .iter()
                    .map(|port| run_width(&port.label, TEXT_SIZE, FontWeight::NORMAL, window))
                    .fold(0., f32::max)
            };
            // Two flex columns with `justify-between` and a `gap-2`; an empty
            // column is zero-width, but the gap still counts.
            let left = if card.inputs.is_empty() {
                0.
            } else {
                PORT_INSET + widest(&card.inputs) + PORT_LABEL_PAD
            };
            let right = if card.outputs.is_empty() {
                0.
            } else {
                PORT_LABEL_PAD + widest(&card.outputs) + PORT_INSET
            };
            let ports_width = left + COLUMN_GAP + right;

            let rows = card.inputs.len().max(card.outputs.len());
            let ports_height = PAD * 2.
                + if rows == 0 {
                    0.
                } else {
                    rows as f32 * TEXT_LINE + (rows - 1) as f32 * PORT_ROW_GAP
                };

            card.width = ports_width.max(CARD_MIN_WIDTH - CARD_BORDER * 2.) + CARD_BORDER * 2.;
            card.height = CARD_BORDER * 2. + HEADER_HEIGHT + ports_height;

            let row_centre = |row: usize| {
                CARD_BORDER
                    + HEADER_HEIGHT
                    + PAD
                    + row as f32 * (TEXT_LINE + PORT_ROW_GAP)
                    + TEXT_LINE / 2.
            };
            for (row, port) in card.inputs.iter_mut().enumerate() {
                port.at = point(CARD_BORDER + PORT_ANCHOR, row_centre(row));
            }
            for (row, port) in card.outputs.iter_mut().enumerate() {
                port.at = point(card.width - CARD_BORDER - PORT_ANCHOR, row_centre(row));
            }
            card.resolve_regions();
        }
        if self.cards.iter().any(|card| !card.placed) {
            self.layout();
        }
        self.measured = true;
    }

    /// Arrange the cards the document does not place. Columns follow the
    /// wires left to right, each card as near its consumers as its producers
    /// allow, so an Input lands beside the card that reads it. Rows follow
    /// the ports a card is wired to, so wires run as straight as the column
    /// lets them. A card the document does place stays put, and a new card
    /// among placed ones takes the nearest free spot beside what it reads.
    fn layout(&mut self) {
        const GUTTER: f32 = 80.;
        const GAP: f32 = 18.;
        let n = self.cards.len();
        let mut succ: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); n];
        let mut pred: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); n];
        for link in &self.links {
            succ[link.from.0].push((link.to.0, link.from.1, link.to.1));
            pred[link.to.0].push((link.from.0, link.from.1, link.to.1));
        }
        // The row a card's wires ask for: each settled neighbour's port,
        // less where that wire meets this card.
        let wanted = |cards: &[Card], i: usize, settled: &dyn Fn(usize) -> bool| {
            let mut sum = 0.;
            let mut count = 0.;
            for &(j, from, to) in &pred[i] {
                if settled(j) {
                    sum +=
                        cards[j].origin.y + cards[j].outputs[from].at.y - cards[i].inputs[to].at.y;
                    count += 1.;
                }
            }
            for &(j, from, to) in &succ[i] {
                if settled(j) {
                    sum +=
                        cards[j].origin.y + cards[j].inputs[to].at.y - cards[i].outputs[from].at.y;
                    count += 1.;
                }
            }
            (count > 0.).then(|| sum / count)
        };
        if self.cards.iter().any(|card| card.placed) {
            let mut settled: Vec<bool> = self.cards.iter().map(|card| card.placed).collect();
            for i in 0..n {
                if settled[i] {
                    continue;
                }
                let left = succ[i]
                    .iter()
                    .filter(|&&(j, ..)| settled[j])
                    .map(|&(j, ..)| self.cards[j].origin.x)
                    .reduce(f32::min);
                let right = pred[i]
                    .iter()
                    .filter(|&&(j, ..)| settled[j])
                    .map(|&(j, ..)| self.cards[j].origin.x + self.cards[j].width)
                    .reduce(f32::max);
                let x = match (left, right) {
                    (Some(left), _) => left - self.cards[i].width - GUTTER,
                    (None, Some(right)) => right + GUTTER,
                    (None, None) => 0.,
                };
                let y = wanted(&self.cards, i, &|j| settled[j]).unwrap_or(0.);
                self.cards[i].origin = point(x, y);
                // Below whatever it would cover.
                while let Some(j) = (0..n).find(|&j| {
                    settled[j]
                        && self.cards[i].intersects(
                            self.cards[j].origin - point(GAP, GAP),
                            self.cards[j].origin
                                + point(self.cards[j].width + GAP, self.cards[j].height + GAP),
                        )
                }) {
                    self.cards[i].origin.y = self.cards[j].origin.y + self.cards[j].height + GAP;
                }
                settled[i] = true;
            }
            return;
        }
        // Columns: the longest wire path from a source, then each card slides
        // toward whichever side it has more wires on, as far as the other
        // side allows. Every slide shortens the wires, so this settles.
        let mut column = vec![0usize; n];
        let mut indegree: Vec<usize> = pred.iter().map(Vec::len).collect();
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut topological = Vec::with_capacity(n);
        while let Some(i) = queue.pop_front() {
            topological.push(i);
            for &(j, ..) in &succ[i] {
                column[j] = column[j].max(column[i] + 1);
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    queue.push_back(j);
                }
            }
        }
        // A cycle, which the document never validates, leaves its cards at the left.
        let missing: Vec<usize> = (0..n).filter(|i| !topological.contains(i)).collect();
        topological.extend(missing);
        loop {
            let mut changed = false;
            for &i in topological.iter().rev() {
                let lo = pred[i]
                    .iter()
                    .map(|&(j, ..)| column[j] + 1)
                    .max()
                    .unwrap_or(0);
                let hi = succ[i]
                    .iter()
                    .map(|&(j, ..)| column[j].saturating_sub(1))
                    .min()
                    .unwrap_or(column[i])
                    .max(lo);
                let target = match succ[i].len().cmp(&pred[i].len()) {
                    std::cmp::Ordering::Greater => hi,
                    std::cmp::Ordering::Less => lo,
                    std::cmp::Ordering::Equal => column[i].clamp(lo, hi),
                };
                if target != column[i] {
                    column[i] = target;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut used = column.clone();
        used.sort_unstable();
        used.dedup();
        let mut columns: Vec<Vec<usize>> = vec![Vec::new(); used.len()];
        for &i in &topological {
            column[i] = used.binary_search(&column[i]).unwrap_or(0);
            columns[column[i]].push(i);
        }
        // Rows within a column: by the rows of a card's neighbours in the
        // column before, then after, and by the port each wire meets there,
        // so a fan of Inputs follows the port order of the card reading it.
        let mut rank = vec![0f32; n];
        for cards in &columns {
            for (row, &i) in cards.iter().enumerate() {
                rank[i] = row as f32;
            }
        }
        for sweep in 0..4 {
            let forward = sweep % 2 == 0;
            let order: Vec<usize> = if forward {
                (0..columns.len()).collect()
            } else {
                (0..columns.len()).rev().collect()
            };
            for c in order {
                let mut keyed: Vec<(f32, usize)> = columns[c]
                    .iter()
                    .map(|&i| {
                        let neighbours = if forward { &pred[i] } else { &succ[i] };
                        let key = if neighbours.is_empty() {
                            rank[i]
                        } else {
                            neighbours
                                .iter()
                                .map(|&(j, from, to)| {
                                    rank[j] + 0.001 * (if forward { from } else { to }) as f32
                                })
                                .sum::<f32>()
                                / neighbours.len() as f32
                        };
                        (key, i)
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
                columns[c] = keyed.into_iter().map(|(_, i)| i).collect();
                for (row, &i) in columns[c].iter().enumerate() {
                    rank[i] = row as f32;
                }
            }
        }
        let mut x = 0.;
        for cards in &columns {
            let mut y = 0.;
            for &i in cards {
                self.cards[i].origin = point(x, y);
                y += self.cards[i].height + GAP;
            }
            x += cards
                .iter()
                .map(|&i| self.cards[i].width)
                .fold(0., f32::max)
                + GUTTER;
        }
        // Pull each card toward the rows its wires ask for, keeping the
        // column's order and gaps: a card that would cover the one above it
        // goes below instead, and the column as a whole shares the pushes.
        for round in 0..8 {
            let order: Vec<usize> = if round % 2 == 0 {
                (0..columns.len()).collect()
            } else {
                (0..columns.len()).rev().collect()
            };
            for c in order {
                let want: Vec<Option<f32>> = columns[c]
                    .iter()
                    .map(|&i| wanted(&self.cards, i, &|_| true))
                    .collect();
                let mut floor = f32::MIN;
                let mut placed = Vec::with_capacity(columns[c].len());
                for (k, &i) in columns[c].iter().enumerate() {
                    let y = want[k].unwrap_or(self.cards[i].origin.y).max(floor);
                    placed.push(y);
                    floor = y + self.cards[i].height + GAP;
                }
                let pushes: Vec<f32> = want
                    .iter()
                    .zip(&placed)
                    .filter_map(|(want, placed)| want.map(|want| want - placed))
                    .collect();
                let shift = pushes.iter().sum::<f32>() / pushes.len().max(1) as f32;
                for (k, &i) in columns[c].iter().enumerate() {
                    self.cards[i].origin.y = placed[k] + shift;
                }
            }
        }
        let top = self
            .cards
            .iter()
            .map(|card| card.origin.y)
            .fold(f32::MAX, f32::min);
        for card in &mut self.cards {
            card.origin.y -= top;
        }
    }

    /// A newly inserted or selected card must be reachable above older cards
    /// at the same position. Links retain their endpoints as draw order changes.
    fn raise(&mut self, selected: &[SharedString]) {
        if selected.len() <= self.cards.len()
            && self.cards[self.cards.len() - selected.len()..]
                .iter()
                .map(|card| &card.node_id)
                .eq(selected.iter())
        {
            return;
        }
        for id in selected {
            let Some(index) = self.cards.iter().position(|card| &card.node_id == id) else {
                continue;
            };
            let top = self.cards.len() - 1;
            if index == top {
                continue;
            }
            let card = self.cards.remove(index);
            self.cards.push(card);
            let remap = |old: usize| {
                if old == index {
                    top
                } else if old > index {
                    old - 1
                } else {
                    old
                }
            };
            for link in &mut self.links {
                link.from.0 = remap(link.from.0);
                link.to.0 = remap(link.to.0);
            }
        }
    }

    /// What the pointer at `at` (graph space) is over, topmost card first —
    /// cards are painted in order, so the last one containing the point is on
    /// top. Within a card the resolution is one linear scan of its regions,
    /// so the whole test stays O(cards) with a small constant. A wire is only
    /// consulted when no card claims the point; `zoom` converts its
    /// screen-space slack ([`WIRE_GRAB`]) into graph units.
    fn hit(&self, at: Point<f32>, zoom: f32) -> Hit {
        for (index, card) in self.cards.iter().enumerate().rev() {
            let local = point(at.x - card.origin.x, at.y - card.origin.y);
            // Socket grab regions extend beyond the card's boundary.
            for region in &card.regions {
                if let RegionKind::Port { port, output } = region.kind {
                    if region.contains(local) {
                        return Hit::Port {
                            card: index,
                            port,
                            output,
                        };
                    }
                }
            }
            if !card.contains(at) {
                continue;
            }
            for region in &card.regions {
                if !region.contains(local) {
                    continue;
                }
                return match region.kind {
                    RegionKind::Port { port, output } => Hit::Port {
                        card: index,
                        port,
                        output,
                    },
                    RegionKind::Header => Hit::Header { card: index },
                };
            }
            return Hit::Body { card: index };
        }
        let slack = WIRE_GRAB / zoom;
        for (index, link) in self.links.iter().enumerate().rev() {
            let (from, to) = self.ends(link);
            // The same four corner points `paint_wire` strokes through; the
            // fillets round the corners by less than the slack, so the
            // polyline is an honest stand-in for the drawn curve.
            let corners = [
                from,
                point(from.x + WIRE_STUB, from.y),
                point(to.x - WIRE_STUB, to.y),
                to,
            ];
            if corners
                .windows(2)
                .any(|pair| segment_distance(at, pair[0], pair[1]) <= slack)
            {
                return Hit::Wire { link: index };
            }
        }
        Hit::Empty
    }

    /// The box every card fits inside, in graph space — what a fit frames.
    /// `None` for an empty graph, which has no box and needs no framing.
    fn extent(&self) -> Option<(Point<f32>, Size<f32>)> {
        let mut min = point(f32::MAX, f32::MAX);
        let mut max = point(f32::MIN, f32::MIN);
        for card in &self.cards {
            min = point(min.x.min(card.origin.x), min.y.min(card.origin.y));
            max = point(
                max.x.max(card.origin.x + card.width),
                max.y.max(card.origin.y + card.height),
            );
        }
        (!self.cards.is_empty()).then(|| (min, size(max.x - min.x, max.y - min.y)))
    }

    /// Both ends of one wire, in graph space.
    fn ends(&self, link: &Link) -> (Point<f32>, Point<f32>) {
        let at = |(card, port): (usize, usize), output: bool| {
            let card = &self.cards[card];
            let port = if output {
                &card.outputs[port]
            } else {
                &card.inputs[port]
            };
            point(card.origin.x + port.at.x, card.origin.y + port.at.y)
        };
        (at(link.from, true), at(link.to, false))
    }
}

fn run_width(text: &SharedString, size: f32, weight: FontWeight, window: &Window) -> f32 {
    f32::from(paint::shape(text, size, weight, ladder::foreground(), window).width)
}

/// Break `text` to `width`, greedily, on spaces — the one thing a canvas has
/// to do for itself that a `<p>` does for free.
// -- rendering ----------------------------------------------------------------

fn viewport_controls(state: &Editor, app: &Entity<Luma>) -> Div {
    let mut row = div()
        .absolute()
        .bottom(px(12.))
        .left(px(12.))
        .flex()
        .gap(px(4.))
        .p(px(4.))
        .rounded(px(6.))
        .bg(ladder::background());
    for (label, name) in [
        ("−", "Zoom out"),
        ("100%", "Actual size"),
        ("+", "Zoom in"),
        ("Fit", "Fit graph"),
    ] {
        let app = app.clone();
        let target = state.target();
        row = row.child(
            luma_ui::button(label, luma_ui::Enabled::Yes)
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    app.update(cx, |this, cx| {
                        this.edit_graph_tab(&target, cx, |editor| {
                            let canvas = editor.canvas_size.get();
                            let origin = editor.origin.get();
                            let mut view = editor.view.get();
                            if name == "Fit graph" {
                                if let Some((at, extent)) = editor.scene.borrow().extent() {
                                    view = Viewport::fit(canvas, at, extent);
                                }
                            } else {
                                let center = origin + point(canvas.width / 2., canvas.height / 2.);
                                let factor = match name {
                                    "Actual size" => 1. / view.zoom,
                                    "Zoom in" => 1.25,
                                    _ => 0.8,
                                };
                                view.zoom_about(origin, center, factor);
                                if name == "Actual size" {
                                    if let Some(card) = editor
                                        .scene
                                        .borrow()
                                        .cards
                                        .iter()
                                        .find(|card| editor.selected.contains(&card.node_id))
                                    {
                                        view.pan = point(
                                            canvas.width / 2. - px(card.origin.x + card.width / 2.),
                                            canvas.height / 2.
                                                - px(card.origin.y + card.height / 2.),
                                        );
                                    }
                                }
                            }
                            editor.fit = false;
                            editor.view.set(view);
                        })
                    });
                })
                .agent_node(Role::Button, name),
        );
    }
    row
}

/// Render the screen: a toolbar strip over the canvas.
///
/// The toolbar is ordinary elements and the canvas is one painted element.
/// That split is the whole layout decision — a panel is a stack of boxes and
/// gpui already lays boxes out well, while a graph is a coordinate system and
/// nothing gpui lays out could express it without a box per node.
pub fn graph(
    state: &mut Editor,
    app: &Entity<Luma>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Div {
    controls::sync(state, window, cx);
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        .children(interaction::catalog(state, app))
        .when_some(state.error.clone(), |el, error| {
            el.child(luma_ui::silkscreen(error))
        })
        .child(match (&state.error, state.shown_graph()) {
            (Some(message), None) => luma_ui::plate(message.clone(), ladder::danger()),
            (None, None) => {
                luma_ui::plate("Loading graph…".to_string(), ladder::muted_foreground())
            }
            (_, Some(_)) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(canvas_element(state, app))
                .children(controls::panel(state, app))
                .into_any_element(),
        })
}

/// What is open, how big it is, and whether a write is in the air. Nothing
/// here is a control the canvas needs — the canvas is driven by the pointer —
/// so the strip stays a readout. Closing lives on the tab's chip.
fn toolbar(state: &Editor, app: &Entity<Luma>) -> Div {
    let preview_app = app.clone();
    let target = state.target();
    let nodes = state.scene.borrow().cards.len();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(16.))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .children(graph_breadcrumbs(state, app))
        .children(controls::add_button(state, app))
        .when(state.inspecting_builtin(), |el| {
            el.child(luma_ui::silkscreen("BUILT-IN · READ ONLY".to_owned()))
        })
        .child(luma_ui::silkscreen(format!("{nodes} NODES")))
        .child(
            luma_ui::button(
                if state.preview_running {
                    "Rendering…"
                } else {
                    "Refresh preview"
                },
                luma_ui::Enabled::Yes,
            )
            .w(px(128.))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                preview_app.update(cx, |this, cx| this.refresh_score_graph_preview(&target, cx))
            })
            .agent_node(Role::Button, "Refresh preview"),
        )
        // The resolved context, shown: implicit context that silently changes
        // what the plots mean is obscurity; a context that is never shown is
        // worse than none (§6).
        .child(luma_ui::silkscreen(format!(
            "TRACK {}",
            state.context.track_name.to_uppercase()
        )))
        .child(div().flex_1())
}

fn graph_breadcrumbs(state: &Editor, app: &Entity<Luma>) -> Vec<AnyElement> {
    let labels = std::iter::once(state.pattern_name().to_string()).chain(
        state.inspection.iter().map(|(id, _)| {
            state
                .types
                .get(&format!("{}{id}", luma_lib::node_graph::lighting::PREFIX))
                .map(|def| def.name.clone())
                .unwrap_or_else(|| id.clone())
        }),
    );
    labels
        .enumerate()
        .map(|(depth, label)| {
            let app = app.clone();
            let target = state.target();
            luma_ui::button(&label, luma_ui::Enabled::Yes)
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.edit_graph_tab(&target, cx, |editor| {
                            editor.inspection.truncate(depth);
                            editor.reframe();
                        })
                    });
                })
                .agent_node(Role::Button, format!("Graph: {label}"))
                .into_any_element()
        })
        .collect()
}

/// One element for the whole graph.
///
/// Everything the paint needs is shared by handle — one refcounted [`Scene`],
/// one cell of viewport, one selected id — so a frame draws a consistent
/// picture without reaching back into the app to ask what it looks like. The
/// pointer handlers do the reverse: they carry no picture at all, only the
/// entity to send the gesture to, because by the time one runs, the frame it
/// was registered in is already gone.
fn canvas_element(state: &Editor, app: &Entity<Luma>) -> impl IntoElement {
    let navigation = viewport_controls(state, app);
    let registered = Rc::clone(&state.scene);
    let painted = Rc::clone(&state.scene);
    let measured_view = Rc::clone(&state.view);
    let painted_view = Rc::clone(&state.view);
    let registered_selected = state.selected.clone();
    let selected = state.selected.clone();
    let selected_edge = state.selected_edge.clone();
    let registered_edge = state.selected_edge.clone();
    let wire = match &state.gesture {
        Some(Gesture::Wire(wire)) => Some(wire.clone()),
        _ => None,
    };
    let marquee = match &state.gesture {
        Some(Gesture::Marquee { from, to }) => Some((*from, *to)),
        _ => None,
    };
    let origin = Rc::clone(&state.origin);
    let canvas_size = Rc::clone(&state.canvas_size);
    let fit = state.fit;
    let fitted_size = Rc::clone(&state.fitted_size);
    // The tab the canvas belongs to: every handler this frame registers is
    // addressed to it, not to whatever tab is visible when the event lands.
    let target = state.target();
    let app = app.clone();

    div()
        .flex_1()
        .relative()
        .overflow_hidden()
        .child(
            canvas(
                move |bounds, window, cx| {
                    // Where the canvas ended up is what turns a window-space mouse
                    // position back into a graph one, and only prepaint knows it.
                    // A press can arrive before the next paint but never before the
                    // next prepaint, so this is also the only place it is safe to
                    // write.
                    origin.set(bounds.origin);
                    canvas_size.set(bounds.size);
                    {
                        let mut scene = registered.borrow_mut();
                        scene.raise(&registered_selected);
                        if !scene.measured {
                            scene.measure(window);
                        }
                        // The framing waits on that measure: a fit is a function
                        // of boxes that do not exist until the text system has
                        // been asked about them. Re-framed whenever the canvas
                        // itself changes size — the pane resizing under the tab —
                        // for as long as the user has not taken the view.
                        if fit && bounds.size != fitted_size.get() {
                            if let Some((at, extent)) = scene.extent() {
                                measured_view.set(Viewport::fit(bounds.size, at, extent));
                                fitted_size.set(bounds.size);
                            }
                        }
                    }
                    // Registered here, alongside every laid-out control, so the
                    // frame's node ids stay in tree order. A selected card reports
                    // `focused` — it is where the keyboard verbs land next.
                    let scene = registered.borrow();
                    let view = measured_view.get();
                    agent_paint_node(Role::Card, "Graph workspace", bounds, window, cx);
                    for link in &scene.links {
                        let (from, to) = scene.ends(link);
                        let at = view.to_window(
                            bounds.origin,
                            point((from.x + to.x) / 2., (from.y + to.y) / 2.),
                        );
                        let from_card = &scene.cards[link.from.0];
                        let to_card = &scene.cards[link.to.0];
                        agent_paint_node_focused(
                            Role::Button,
                            format!(
                                "Edge {}.{} → {}.{}",
                                from_card.node_id,
                                from_card.outputs[link.from.1].id,
                                to_card.node_id,
                                to_card.inputs[link.to.1].id
                            ),
                            Bounds {
                                origin: at - point(px(WIRE_GRAB), px(WIRE_GRAB)),
                                size: size(px(WIRE_GRAB * 2.), px(WIRE_GRAB * 2.)),
                            },
                            registered_edge.as_ref() == Some(&link.id),
                            window,
                            cx,
                        );
                    }
                    for card in &scene.cards {
                        let box_ = view.card_box(bounds.origin, card);
                        agent_paint_node_focused(
                            Role::Card,
                            card.title.clone(),
                            box_,
                            registered_selected.contains(&card.node_id),
                            window,
                            cx,
                        );
                        // The hit regions double as the harness's address book:
                        // one rect, one label, minted together in the measure
                        // pass, registered together here.
                        for region in &card.regions {
                            let role = match region.kind {
                                RegionKind::Port { .. } => Role::Button,
                                RegionKind::Header => continue,
                            };
                            let box_ = Bounds {
                                origin: view.to_window(
                                    bounds.origin,
                                    point(
                                        card.origin.x + region.origin.x,
                                        card.origin.y + region.origin.y,
                                    ),
                                ),
                                size: size(
                                    px(region.size.width * view.zoom),
                                    px(region.size.height * view.zoom),
                                ),
                            };
                            agent_paint_node(role, region.label.clone(), box_, window, cx);
                        }
                    }
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                move |bounds, hitbox, window, cx| {
                    paint(
                        bounds,
                        &painted.borrow(),
                        painted_view.get(),
                        &selected,
                        selected_edge.as_ref(),
                        wire.as_ref(),
                        marquee,
                        window,
                        cx,
                    );
                    listen(&app, target.clone(), &hitbox, window);
                },
            )
            .size_full(),
        )
        .child(navigation)
}

/// Register this frame's pointer handlers.
///
/// Press and scroll are scoped to the canvas's hitbox; move and release are
/// not. That asymmetry is deliberate: a drag that wanders off the canvas — or
/// off the window — must keep tracking, and must end when the button comes up
/// wherever that happens. A gesture that could only end inside the element it
/// started in is a gesture that can be left stuck on.
fn listen(app: &Entity<Luma>, target: Target, hitbox: &Hitbox, window: &mut Window) {
    let pressed = app.clone();
    let press_target = target.clone();
    let inside = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !inside.is_hovered(window) {
            return;
        }
        let at = event.position;
        let shift = event.modifiers.shift;
        pressed.update(cx, |this, cx| match event.button {
            MouseButton::Left => {
                this.focus.focus(window, cx);
                this.graph_press(&press_target, at, shift, event.click_count, cx);
            }
            MouseButton::Right => this.open_graph_catalog(&press_target, at, window, cx),
            _ => (),
        });
    });

    let dragged = app.clone();
    let drag_target = target.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            let at = event.position;
            dragged.update(cx, |this, cx| this.graph_drag(&drag_target, at, cx));
        }
    });

    let released = app.clone();
    let release_target = target.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            released.update(cx, |this, cx| {
                this.graph_release(&release_target, event.position, cx)
            });
        }
    });

    let zoomed = app.clone();
    let over = hitbox.clone();
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !over.is_hovered(window) {
            return;
        }
        let wheel = f32::from(event.delta.pixel_delta(window.line_height()).y);
        let at = event.position;
        zoomed.update(cx, |this, cx| this.graph_zoom(&target, at, wheel, cx));
    });
}

/// Paint the graph back to front: ground, wires, cards.
///
/// Wires go *under* the cards on purpose. Every wire ends at a port anchored
/// [`PORT_ANCHOR`] inside the card's edge, so its last few pixels run beneath
/// the card — which is what makes the visible wire stop cleanly at that edge
/// instead of ending in a gap short of the ring. The card then paints its own
/// ghost lead-in over that stretch, exactly as the web port row does.
#[allow(clippy::too_many_arguments)]
fn paint(
    bounds: Bounds<Pixels>,
    scene: &Scene,
    view: Viewport,
    selected: &[SharedString],
    selected_edge: Option<&SharedString>,
    wire: Option<&interaction::WireDrag>,
    marquee: Option<(Point<f32>, Point<f32>)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(fill(bounds, ladder::background()));

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for link in &scene.links {
            let (from, to) = scene.ends(link);
            let selected = selected_edge == Some(&link.id);
            paint_wire(
                bounds.origin,
                from,
                to,
                if selected {
                    ladder::primary()
                } else {
                    link.color
                },
                view,
                if selected {
                    WIRE_WIDTH + 1.5
                } else {
                    WIRE_WIDTH
                },
                window,
            );
        }
        for card in &scene.cards {
            let box_ = view.card_box(bounds.origin, card);
            // Culling is the only per-frame filter worth having: a graph is
            // small enough to walk, and large enough that drawing the
            // off-screen half of it would double every frame's shaping for
            // nothing.
            if !box_.intersects(&bounds) {
                continue;
            }
            paint_card(
                box_,
                card,
                view.zoom,
                selected.iter().any(|id| id == &card.node_id),
                window,
                cx,
            );
        }
        if let Some(wire) = wire {
            wire.paint(bounds, scene, view, window, cx);
        }
        // The marquee, over everything: a 1px `--primary` outline, nothing
        // filled, nothing animated. Constant 1px because the rect is a
        // pointing gesture — window-space chrome, not graph-space content.
        if let Some((a, b)) = marquee {
            let min = point(a.x.min(b.x), a.y.min(b.y));
            let max = point(a.x.max(b.x), a.y.max(b.y));
            window.paint_quad(quad(
                Bounds {
                    origin: view.to_window(bounds.origin, min),
                    size: size(
                        px((max.x - min.x) * view.zoom),
                        px((max.y - min.y) * view.zoom),
                    ),
                },
                Corners::default(),
                transparent_black(),
                Edges::all(px(1.)),
                ladder::primary(),
                BorderStyle::Solid,
            ));
        }
    });
}

/// One node card: a `--card` plate inside a 2px `--gutter` border, a `--trim`
/// header, two port columns, and whatever body the node type carries.
///
/// Square, where `base-node.tsx` says `rounded-lg` — because `--radius: 0rem`
/// makes `--radius-lg` zero, so the rendered web card is square too. The class
/// is decorative; the corner is not a divergence.
///
/// Selection is the one place this screen does *not* copy the web. React Flow
/// never styled a selected node and the app never overrode it, so on the web
/// the only way to tell a node is selected is to press Delete. Here the border
/// takes [`ladder::primary`] — the one hue this screen spends on a surface,
/// and it spends it on meaning.
fn paint_card(
    box_: Bounds<Pixels>,
    card: &Card,
    zoom: f32,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(quad(
        box_,
        Corners::default(),
        ladder::card(),
        Edges::all(px(CARD_BORDER * zoom)),
        if selected {
            ladder::primary()
        } else {
            ladder::trim()
        },
        BorderStyle::Solid,
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(
                box_.origin.x + px(CARD_BORDER * zoom),
                box_.origin.y + px(CARD_BORDER * zoom),
            ),
            size: size(
                box_.size.width - px(CARD_BORDER * 2. * zoom),
                px(HEADER_HEIGHT * zoom),
            ),
        },
        ladder::band(),
    ));

    // Ghosts and rings are shape, not text, so they are drawn at every zoom.
    for (ports, output) in [(&card.inputs, false), (&card.outputs, true)] {
        for port in ports {
            if port.connected {
                paint_ghost(box_.origin, port, zoom, output, window);
            }
            paint_ring(box_.origin, port, zoom, window);
        }
    }

    // Below the floor a label is a smudge, and shaping is by far the most
    // expensive thing on this canvas. Dropping the text keeps the shape, which
    // is all that is legible at that size anyway.
    if zoom < LABEL_FLOOR {
        return;
    }
    // The card clips its own text, which is what `overflow-hidden` does on the
    // web card: a title too long for the box is cut off at the edge.
    window.with_content_mask(Some(ContentMask { bounds: box_ }), |window| {
        paint::tracked(
            point(
                box_.origin.x + px((CARD_BORDER + PAD_H) * zoom),
                box_.origin.y + px((CARD_BORDER + PAD) * zoom),
            ),
            &card.title,
            TEXT_SIZE * zoom,
            FontWeight::MEDIUM,
            ladder::muted_foreground(),
            TITLE_TRACKING * zoom,
            window,
            cx,
        );
        if card.interface_input {
            return;
        }
        for (ports, output) in [(&card.inputs, false), (&card.outputs, true)] {
            for port in ports {
                paint_port_label(box_.origin, port, zoom, output, window, cx);
            }
        }
    });
}

/// The faint hidden segment of a wire, from the card's content edge in to the
/// port anchor (`base-node.tsx`'s ghost lead-in).
fn paint_ghost(card: Point<Pixels>, port: &Port, zoom: f32, output: bool, window: &mut Window) {
    let centre = point(card.x + px(port.at.x * zoom), card.y + px(port.at.y * zoom));
    let left = if output {
        centre.x
    } else {
        centre.x - px(PORT_ANCHOR * zoom)
    };
    let mut color: Hsla = port.color.into();
    color.a = PORT_GHOST_ALPHA;
    window.paint_quad(fill(
        Bounds {
            origin: point(left, centre.y - px(PORT_GHOST_H * zoom / 2.)),
            size: size(px(PORT_ANCHOR * zoom), px(PORT_GHOST_H * zoom)),
        },
        color,
    ));
}

/// Paths preserve the shared subpixel center. Rounded quads snap their outer
/// edges independently, shifting an odd-sized ring relative to its even-sized dot.
fn paint_ring(card: Point<Pixels>, port: &Port, zoom: f32, window: &mut Window) {
    let centre = point(card.x + px(port.at.x * zoom), card.y + px(port.at.y * zoom));
    for (visible, radius, mut path) in [
        (
            true,
            (PORT_RING - PORT_RING_BORDER) * zoom / 2.,
            PathBuilder::stroke(px(PORT_RING_BORDER * zoom)),
        ),
        (port.connected, PORT_DOT * zoom / 2., PathBuilder::fill()),
    ] {
        if !visible {
            continue;
        }
        let radius = px(radius);
        let left = point(centre.x - radius, centre.y);
        let right = point(centre.x + radius, centre.y);
        path.move_to(left);
        path.arc_to(point(radius, radius), px(0.), false, true, right);
        path.arc_to(point(radius, radius), px(0.), false, true, left);
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, port.color);
        }
    }
}

/// A port's name, set inboard of its ring. An output's is right-aligned
/// against that ring, so the line is shaped before it is placed rather than
/// drawn at a known point.
fn paint_port_label(
    card: Point<Pixels>,
    port: &Port,
    zoom: f32,
    output: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let centre = point(card.x + px(port.at.x * zoom), card.y + px(port.at.y * zoom));
    let line = paint::shape(
        &port.label,
        TEXT_SIZE * zoom,
        FontWeight::NORMAL,
        ladder::muted_foreground(),
        window,
    );
    // `pl-4` puts an input's label 16px from the content edge — 10px past the
    // 6px anchor — and `pr-4` mirrors it for an output.
    let inset = px((PORT_INSET - PORT_ANCHOR) * zoom);
    let left = if output {
        centre.x - inset - line.width
    } else {
        centre.x + inset
    };
    line.paint(
        point(left, centre.y - px(TEXT_LINE * zoom / 2.)),
        px(TEXT_LINE * zoom),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

/// Whatever the node type carries under its ports.
fn paint_wire(
    origin: Point<Pixels>,
    from: Point<f32>,
    to: Point<f32>,
    color: Rgba,
    view: Viewport,
    width: f32,
    window: &mut Window,
) {
    let corners = [
        from,
        point(from.x + WIRE_STUB, from.y),
        point(to.x - WIRE_STUB, to.y),
        to,
    ];
    let at = |p: Point<f32>| view.to_window(origin, p);
    let mut path = PathBuilder::stroke(px(width * view.zoom));
    path.move_to(at(corners[0]));
    for index in 1..corners.len() - 1 {
        let (previous, corner, next) = (corners[index - 1], corners[index], corners[index + 1]);
        let radius = WIRE_FILLET
            .min(distance(previous, corner) / 2.)
            .min(distance(corner, next) / 2.);
        if radius <= 0. {
            path.line_to(at(corner));
            continue;
        }
        // Straight in to `radius` before the corner, then a quadratic with the
        // corner itself as the control point out to `radius` past it.
        path.line_to(at(toward(corner, previous, radius)));
        path.curve_to(at(toward(corner, next, radius)), at(corner));
    }
    path.line_to(at(corners[corners.len() - 1]));
    // A degenerate path — two ports at the same point — has nothing to
    // tessellate and is not worth an error.
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn distance(from: Point<f32>, to: Point<f32>) -> f32 {
    ((to.x - from.x).powi(2) + (to.y - from.y).powi(2)).sqrt()
}

/// How far `at` is from the segment `a`–`b`: the distance to the projection of
/// `at` onto the segment, clamped to its ends.
fn segment_distance(at: Point<f32>, a: Point<f32>, b: Point<f32>) -> f32 {
    let length = distance(a, b);
    if length == 0. {
        return distance(at, a);
    }
    let t = (((at.x - a.x) * (b.x - a.x) + (at.y - a.y) * (b.y - a.y)) / (length * length))
        .clamp(0., 1.);
    distance(at, point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t))
}

/// Pull a point `by` away from `from`, toward `to`.
fn toward(from: Point<f32>, to: Point<f32>, by: f32) -> Point<f32> {
    let length = distance(from, to);
    if length == 0. {
        return from;
    }
    point(
        from.x + (to.x - from.x) / length * by,
        from.y + (to.y - from.y) / length * by,
    )
}
