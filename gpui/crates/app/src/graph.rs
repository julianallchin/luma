//! The pattern graph editor: a custom-painted canvas over the authored graph
//! document.
//!
//! Mirrors `src/features/patterns/components/pattern-editor.tsx` and the
//! React Flow surface under `src/shared/lib/react-flow/` — the same node card
//! (a trim header over two port columns, then a body of param controls), the
//! same fillet wire (a stub out of each port, one diagonal, rounded corners),
//! the same flat `bg-trim` ground, and the same optimistic write through
//! `save_pattern_graph_document`.
//!
//! Every measurement in the geometry section below is the *computed* value the
//! web card renders at zoom 1, not the Tailwind class it is spelled with —
//! `harness/gauntlet/style-spec.md` is the citation for each one, and
//! `harness/gauntlet/web-*.png` is what they add up to.
//!
//! # Painted, not laid out
//!
//! The web editor is a DOM/canvas hybrid, and most of what is clever about it
//! is defence against WKWebView's layout cost: nodes are absolutely positioned
//! divs kept out of React's render path, live results are routed around
//! `setNodes` so a 20 Hz stream does not rebuild the node array, and edge
//! objects preserve identity so an unrelated refresh does not re-render every
//! wire. None of that applies here. There is no layout engine between this
//! screen and the GPU: the canvas paints quads, paths and shaped lines
//! directly, so a pan is one repaint of one element and costs what it draws.
//!
//! What *does* apply is the reason those defences existed — per-frame work
//! should be proportional to what is on screen. So the resolved geometry
//! ([`Scene`]) is rebuilt when the graph changes and *measured* once after
//! that, never per frame; the paint culls to the viewport; and labels are
//! dropped below the zoom where they stop being legible.
//!
//! # Why a card's box is measured and not computed
//!
//! The web card is `min-w-[170px]` and grows to its content, so its width is a
//! function of shaped text: `Round` is 232px wide because a bare `<input>`'s
//! intrinsic width is twenty characters, `Math` is 172.77px wide because its
//! selector reserves room for `ABSOLUTE DIFFERENCE`. A port of that which
//! guessed at widths would put every wire in the wrong place. So [`Scene`] is
//! built without widths and [`Scene::measure`] resolves them the first time a
//! frame has a text system in hand — which is `prepaint`, and which is also
//! the last moment before a mouse event could ask what a card's box is.
//!
//! # What this is so far
//!
//! Open, read, look, move, select, delete, undo. The pointer resolves through
//! [`Scene::hit`] — ports, widget slots, headers, wires — the selection is a
//! set (shift-click and marquee both feed it), Delete removes it through
//! [`Edit::RemoveNode`], and every command is a step on a [`History`] of
//! whole-document snapshots. Wire creation, param editing, the palette and
//! live preview are the phases still ahead (`docs/design/
//! graph-editor-interaction.md` §8); the seam already carries all of them —
//! this screen is what is missing, not the commands.
//!
//! A `view_signal` node draws whatever signal the view-data store
//! ([`ViewData`]) holds for it: the traces, the min/max axis readings and the
//! legend chips, exactly as `view-channel-node.tsx` strokes them into its 2D
//! context. The store is a global rather than a field on the screen for the
//! same reason the web side keeps `use-view-data-store` outside React state —
//! signals arrive from *running* the graph, at a rate that has nothing to do
//! with the document, and the editor is one reader among several. Nothing in
//! this host runs a graph yet, so today the publisher is the gauntlet capture
//! (`tests/gauntlet.rs`), which seeds the same fixture signals the web capture
//! page seeds its store with. With no signal, the plot is drawn at its full
//! 720 × 140 box with the web node's own empty-state line in it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::node::{agent_paint_node, agent_paint_node_focused, Instrument, Role};
use luma_ui::{fonts, ladder, paint};

use luma_lib::dispatch::CommandError;
use luma_lib::models::node_graph::edit::{apply, pattern_args_def, Edit};
use luma_lib::models::node_graph::{
    Graph, NodeTypeDef, ParamDef, ParamOption, ParamType, PortDef, Signal,
};
use luma_lib::models::patterns::PatternSummary;

use crate::history::History;
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
    /// Unread until the preview runner lands (phase 4b) — carried from day
    /// one because `run_graph` needs it and an editor that resolved only half
    /// its context would have to go asking again at run time.
    #[allow(dead_code)]
    pub(crate) venue: String,
    /// For the toolbar readout — ids are for the seam, not the eye.
    pub(crate) track_name: SharedString,
}

/// The screen's whole state: the pattern it is editing, the document it read,
/// the resolved geometry of that document, and where the eye and the hand are.
pub struct Editor {
    pattern: PatternSummary,
    /// See [`TrackContext`]. A field rather than part of the tab's identity:
    /// the tab stays keyed on the pattern alone, so one document never has two
    /// writers racing through the CAS.
    context: TrackContext,
    /// The node catalogue, keyed by `typeId`. A graph is only titles and ports
    /// once this is in hand, so the screen draws nothing until it is.
    types: Rc<HashMap<String, NodeTypeDef>>,
    /// The signals this screen's view nodes are plotting, snapshotted from
    /// [`ViewData`] when it opened. A publish that lands while the screen is
    /// up is one `rebuild` away from being picked up; nothing publishes
    /// mid-session yet, so wiring that subscription now would be designing
    /// against a guess.
    views: Rc<HashMap<String, Signal>>,
    document: Option<Document>,
    /// Geometry derived from [`Document::graph`] and [`Self::types`], rebuilt
    /// on every change to either and measured once per rebuild. Behind a
    /// `RefCell` because the measure pass needs a text system and therefore
    /// has to run inside the frame — see the module docs.
    scene: Rc<RefCell<Scene>>,
    /// The nodes the keyboard verbs act on. A set, not an option: delete,
    /// undo's snapshot and the marquee all want "these nodes", and widening
    /// later would have meant widening every reader twice.
    selected: Vec<SharedString>,
    /// Where the document has been. Whole-graph snapshots behind an `Rc`, one
    /// checkpoint per command at the gesture boundary — see [`GraphSnapshot`].
    history: History<GraphSnapshot>,
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
    /// A save is in flight. Writes are serialized rather than concurrent: two
    /// in flight against one `base_revision` means the second is a conflict by
    /// construction.
    saving: bool,
    /// An edit landed while a save was in flight, so the document on disk is
    /// behind what is on screen. Flushed when the save returns.
    dirty: bool,
    error: Option<String>,
}

/// The view-data store: the latest [`Signal`] each view node has been handed,
/// keyed by the node id that produced it.
///
/// The GPUI twin of `use-view-data-store` (`src/features/patterns/stores/`),
/// and kept outside the screen's state for the same reason: a run publishes
/// every view at once, at its own rate, and the editor is a reader of that —
/// not its owner. Whoever ends up calling `run_graph` in this host publishes
/// here and every open plot picks it up on its next rebuild.
#[derive(Default)]
pub struct ViewData(Rc<HashMap<String, Signal>>);

impl Global for ViewData {}

impl ViewData {
    /// Replace the store with one run's views. A run is a whole picture of the
    /// graph, so it replaces rather than merges — a stale trace left behind by
    /// a node that no longer outputs one would be a lie the reader cannot
    /// detect.
    pub fn publish(cx: &mut App, views: HashMap<String, Signal>) {
        cx.set_global(Self(Rc::new(views)));
    }

    /// What the store holds right now, cheap to hand to a screen.
    fn snapshot(cx: &App) -> Rc<HashMap<String, Signal>> {
        cx.try_global::<Self>()
            .map(|store| Rc::clone(&store.0))
            .unwrap_or_default()
    }
}

/// The authored document, and the two tokens needed to write it back.
///
/// The graph is behind an `Rc` so a history checkpoint is one clone of the
/// spine: [`Editor::checkpoint`] clones the handle, and the first edit after
/// it un-shares through [`Rc::make_mut`] — clone-on-write, paid once per
/// command instead of once per snapshot.
struct Document {
    implementation_id: String,
    revision: String,
    graph: Rc<Graph>,
}

/// One step of [`Editor::history`]: the whole document, because the document
/// is already the unit that gets written — a command's inverse is the graph it
/// replaced, and every [`Edit`] gets undo for free instead of owing one.
///
/// The selection rides along for the reason the track editor's does: an undo
/// that restored a node but left the selection naming something else puts the
/// next command somewhere the eye is not.
struct GraphSnapshot {
    graph: Rc<Graph>,
    selected: Vec<SharedString>,
}

/// What the pointer is doing between a press and a release.
///
/// A gesture is anchored to the tab it started in: every handler
/// [`listen`] registers carries that tab's [`Target`] and routes through
/// [`Luma::edit_graph_tab`], so switching tabs mid-drag cannot strand a
/// gesture in a tab the handlers can no longer see (§11.5 of the design doc).
enum Gesture {
    /// Moving the eye. `last` is the previous pointer position, so the pan
    /// follows the pointer exactly regardless of zoom.
    Pan { last: Point<Pixels> },
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
    Marquee { from: Point<f32>, to: Point<f32> },
}

/// Where the eye is: a pan in window pixels and a zoom about it.
#[derive(Clone, Copy)]
struct Viewport {
    pan: Point<Pixels>,
    zoom: f32,
}

impl Viewport {
    /// Zoom bounds. The web editor's React Flow range is 0.5–8; the lower end
    /// is extended because a native canvas can afford to draw a whole graph at
    /// once and there is no DOM to thrash at the far end of it.
    const MIN_ZOOM: f32 = 0.2;
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
        &self.pattern.name
    }

    /// The pattern and implementation this editor is showing, once the
    /// document has landed — the two halves of what a `pattern_graph`
    /// conversation is about (see `crate::agent`).
    pub(crate) fn subject(&self) -> Option<(String, String)> {
        let document = self.document.as_ref()?;
        Some((self.pattern.id.clone(), document.implementation_id.clone()))
    }

    /// Rebuild the resolved geometry from the document. Called on load, and
    /// when a save hands back a canonicalized graph — but *not* on a node
    /// move, which writes one card's origin in place, because a rebuild would
    /// throw away the measure pass along with it.
    fn rebuild(&mut self) {
        let scene = match &self.document {
            Some(document) => Scene::build(&document.graph, &self.types, &self.views),
            None => Scene::default(),
        };
        *self.scene.borrow_mut() = scene;
        // A rebuild is where the document last changed hands (a delete, an
        // undo, a canonicalized save coming back), so it is also where the
        // selection sheds ids the document no longer has — a selection naming
        // a dead node would aim the next delete at nothing.
        if let Some(document) = &self.document {
            self.selected
                .retain(|id| document.graph.nodes.iter().any(|node| node.id == *id));
        }
    }

    /// Move one node to `origin` in graph space, in both the document and the
    /// geometry drawn from it. Wires follow for free: a wire is stored as the
    /// two ports it joins, not as two points.
    ///
    /// Routed through [`apply`] like every other mutation — position is the
    /// one edit cheap enough to run per drag tick, but it is still an edit.
    fn move_node(&mut self, node: &SharedString, origin: Point<f32>) {
        let types = Rc::clone(&self.types);
        let Some(document) = &mut self.document else {
            return;
        };
        let edit = Edit::MoveNode {
            id: node.to_string(),
            to: (f64::from(origin.x), f64::from(origin.y)),
        };
        if apply(Rc::make_mut(&mut document.graph), &types, edit).is_err() {
            return;
        }
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

    /// Where the document is now, as something an undo could return to.
    /// `None` before the document has landed — there is then nothing to step
    /// back to, and nothing worth recording.
    fn snapshot(&self) -> Option<GraphSnapshot> {
        Some(GraphSnapshot {
            graph: Rc::clone(&self.document.as_ref()?.graph),
            selected: self.selected.clone(),
        })
    }

    /// Mark the point an undo comes back to, before running an edit. One per
    /// command, at the gesture boundary: a node drag checkpoints at the press
    /// and its ticks mutate in place, so the whole drag is one step.
    fn checkpoint(&mut self) {
        if let Some(now) = self.snapshot() {
            self.history.record(now);
        }
    }

    /// Forget the last checkpoint when the edit it was taken for changed
    /// nothing. Pointer identity answers "did anything run": every mutation
    /// goes through [`Rc::make_mut`], and the checkpoint's own clone keeps the
    /// graph shared, so the first real edit is also a fresh allocation.
    fn abandon_checkpoint(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        let graph = Rc::clone(&document.graph);
        self.history
            .abandon_if(|was| Rc::ptr_eq(&was.graph, &graph));
    }

    /// Remove every selected node from the document — one command, one
    /// checkpoint, however many nodes. `pattern_args` refuses its removal
    /// inside [`apply`] and simply stays; `false` when nothing was removed at
    /// all, which is also no step on the undo stack.
    fn delete_selected(&mut self) -> bool {
        let Some(before) = self.snapshot() else {
            return false;
        };
        if self.selected.is_empty() {
            return false;
        }
        let types = Rc::clone(&self.types);
        let Some(document) = &mut self.document else {
            return false;
        };
        let graph = Rc::make_mut(&mut document.graph);
        let mut removed = false;
        for id in self.selected.clone() {
            removed |= apply(graph, &types, Edit::RemoveNode { id: id.to_string() }).is_ok();
        }
        if removed {
            // Recorded after the fact, from the snapshot taken before it —
            // which is what lets a command that did nothing leave no step.
            self.history.record(before);
            self.rebuild();
        }
        removed
    }

    /// Step back, or forward. `false` when there is nowhere to go. The step
    /// itself records no checkpoint — it is already moving along the stack.
    fn undo(&mut self) -> bool {
        let Some(now) = self.snapshot() else {
            return false;
        };
        let Some(was) = self.history.undo(now) else {
            return false;
        };
        self.restore(was);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(now) = self.snapshot() else {
            return false;
        };
        let Some(next) = self.history.redo(now) else {
            return false;
        };
        self.restore(next);
        true
    }

    /// Put the document back to a snapshot — a rewrite of the working copy
    /// like any other, and it owes a write like any other (the caller saves).
    fn restore(&mut self, snapshot: GraphSnapshot) {
        if let Some(document) = &mut self.document {
            document.graph = snapshot.graph;
        }
        self.selected = snapshot.selected;
        self.rebuild();
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

/// What a lost race says. The reload it describes is automatic, so the
/// sentence has to account for the edit that went missing with it.
const SAVE_CONFLICT: &str =
    "another writer saved this pattern first — reloaded, and this change was not kept";

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

    /// Open a pattern's graph as a workspace tab, against the workspace's
    /// resolved track context. The catalogue and the document are read
    /// together — a graph without the catalogue is a list of opaque type ids,
    /// so there is nothing worth drawing until both have landed. Reopening a
    /// pattern that already has a tab reveals it and **re-points its
    /// context** — a retarget, not a reload, so one document never has two
    /// tabs.
    pub(crate) fn open_pattern(&mut self, pattern: PatternSummary, cx: &mut Context<Self>) {
        // No track, no door: the surfaces that lead here are inert with a
        // stated reason before this is reachable, so a straggler call (a
        // stale click racing a tab close) opens nothing rather than opening
        // an editor that could not preview.
        let Some(context) = self.graph_track_context() else {
            return;
        };
        self.selected_pattern = Some(pattern.clone());
        // The picker's job ends the moment it names a pattern.
        if matches!(
            self.overlay.as_open(),
            Some(crate::shell::Overlay::Patterns(_))
        ) {
            self.close_overlay(cx);
        }
        let target = Target::Graph {
            pattern: pattern.id.clone(),
        };
        if self.workspace.body_mut(&target).is_some() {
            self.edit_graph_tab(&target, cx, |editor| editor.context = context);
            self.workspace.select(&target);
            cx.notify();
            return;
        }
        let types = self.library.node_types();
        let document = self.library.pattern_graph(&pattern.id);
        let state = Box::new(Editor {
            pattern,
            context,
            types: Rc::new(HashMap::new()),
            views: ViewData::snapshot(cx),
            document: None,
            scene: Rc::new(RefCell::new(Scene::default())),
            selected: Vec::new(),
            history: History::default(),
            gesture: None,
            view: Rc::new(Cell::new(Viewport {
                pan: point(px(0.), px(0.)),
                zoom: 1.,
            })),
            fit: true,
            fitted_size: Rc::new(Cell::new(gpui::Size::default())),
            origin: Rc::new(Cell::new(Bounds::default().origin)),
            saving: false,
            dirty: false,
            error: None,
        });
        self.open_tab(target.clone(), move || TabBody::Graph(state), cx);
        cx.spawn(async move |this, cx| {
            let types = types.await;
            let document = document.await;
            this.update(cx, |this, cx| {
                // Addressed to the tab the load was started for, not to
                // whichever tab is visible when it lands.
                this.edit_graph_tab(&target, cx, |editor| match (types, document) {
                    (Ok(types), Ok(document)) => {
                        editor.types = Rc::new(
                            types
                                .into_iter()
                                .map(|definition| (definition.id.clone(), definition))
                                .collect(),
                        );
                        editor.document = Some(Document {
                            implementation_id: document.implementation_id,
                            revision: document.revision,
                            graph: Rc::new(document.graph),
                        });
                        editor.rebuild();
                    }
                    (Err(error), _) | (_, Err(error)) => editor.error = Some(error.to_string()),
                });
            })
            .ok();
        })
        .detach();
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
        cx: &mut Context<Self>,
    ) {
        self.edit_graph_tab(target, cx, |editor| {
            let origin = editor.origin.get();
            let view = editor.view.get();
            let cursor = view.to_graph(origin, at);
            let scene = Rc::clone(&editor.scene);
            let scene = scene.borrow();
            match scene.hit(cursor, view.zoom) {
                // Phase 1 of the design doc: a port press names its node. The
                // wire drag starts here in phase 2.
                Hit::Port { card, .. } => {
                    editor.selected = vec![scene.cards[card].node_id.clone()];
                }
                Hit::Header { card } | Hit::Body { card } | Hit::Widget { card, .. } => {
                    let node = scene.cards[card].node_id.clone();
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
                    // `captureBeforeDrag`: the point an undo comes back to is
                    // where the nodes stood when the pointer took hold.
                    // Abandoned on release if the gesture turned out to be a
                    // press.
                    editor.checkpoint();
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
                // A wire is not draggable until phase 2, so a press on one is
                // a press on the ground under it.
                Hit::Wire { .. } | Hit::Empty => {
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
                None => {}
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
    fn graph_release(&mut self, target: &Target, cx: &mut Context<Self>) {
        match self.workspace.body_mut(target) {
            Some(TabBody::Graph(editor)) if editor.gesture.is_some() => {}
            _ => return,
        }
        let mut save = false;
        self.edit_graph_tab(target, cx, |editor| match editor.gesture.take() {
            Some(Gesture::Move { moved, .. }) => {
                if moved {
                    save = true;
                } else {
                    editor.abandon_checkpoint();
                }
            }
            _ => {}
        });
        if save {
            self.save_graph(target, cx);
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
            .filter(|target| matches!(target, Target::Graph { .. }))
            .cloned()
    }

    /// Delete: remove the selection through [`Edit::RemoveNode`], one
    /// command, one checkpoint, one save.
    pub(crate) fn graph_delete(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        let mut removed = false;
        self.edit_graph_tab(&target, cx, |editor| removed = editor.delete_selected());
        if removed {
            self.save_graph(&target, cx);
        }
    }

    /// `Cmd+Z` / `Cmd+Shift+Z`: step the document back, or forward again.
    /// An undo is a write like any other — it goes through the same save.
    pub(crate) fn graph_undo(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        let mut stepped = false;
        self.edit_graph_tab(&target, cx, |editor| stepped = editor.undo());
        if stepped {
            self.save_graph(&target, cx);
        }
    }

    pub(crate) fn graph_redo(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        let mut stepped = false;
        self.edit_graph_tab(&target, cx, |editor| stepped = editor.redo());
        if stepped {
            self.save_graph(&target, cx);
        }
    }

    /// Write the graph back, optimistically.
    ///
    /// One write is in flight at a time. A second edit made while the first is
    /// running marks the document dirty and is flushed on its return — two
    /// concurrent writes against one `base_revision` would make the later one
    /// a conflict by construction, which is a fight with the seam rather than
    /// a use of it.
    fn save_graph(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return;
        };
        if editor.saving {
            editor.dirty = true;
            return;
        }
        let Some(document) = &editor.document else {
            return;
        };
        editor.saving = true;
        editor.error = None;
        // Minted per *edit*, not per attempt: this id is what lets the seam
        // replay a durable outcome rather than guess from a later snapshot.
        let operation = uuid::Uuid::new_v4().to_string();
        let pending = self.library.save_pattern_graph(
            &editor.pattern.id,
            &document.implementation_id,
            &operation,
            &document.revision,
            &document.graph,
        );
        cx.notify();
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut flush = false;
                let mut reload = false;
                this.edit_graph_tab(&target, cx, |editor| {
                    editor.saving = false;
                    match result {
                        Ok(saved) => {
                            if let Some(document) = &mut editor.document {
                                document.revision = saved.revision;
                                // The seam canonicalizes what it stores, so
                                // the authoritative graph is the one to hold —
                                // except when the screen is already ahead of
                                // it, in which case adopting it would undo a
                                // move the user has already seen.
                                if !editor.dirty && editor.gesture.is_none() {
                                    document.graph = Rc::new(saved.graph);
                                    editor.rebuild();
                                }
                            }
                        }
                        // A lost race is the one failure with a recovery. The
                        // working copy is now based on a revision that would
                        // refuse every later write, and a layout-only editor
                        // has nothing to merge — so the truth is re-read and
                        // this edit is dropped, said plainly rather than left
                        // for the user to discover by reopening.
                        Err(error) => match error.command() {
                            Some(CommandError::Conflict { .. }) => {
                                reload = true;
                                editor.error = Some(SAVE_CONFLICT.into());
                            }
                            _ => editor.error = Some(error.to_string()),
                        },
                    }
                    // A queued edit is dropped with everything else the stale
                    // base carried; flushing it would only lose the same race.
                    flush = editor.dirty && !reload;
                    editor.dirty = false;
                });
                if reload {
                    this.reload_graph(&target, cx);
                } else if flush {
                    this.save_graph(&target, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the document into the open editor, after a write lost a race.
    ///
    /// Unlike [`Self::open_pattern`] this keeps the screen — the catalogue,
    /// the viewport and the message explaining what happened all survive; only
    /// the document is replaced.
    fn reload_graph(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return;
        };
        let pending = self.library.pattern_graph(&editor.pattern.id);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.edit_graph_tab(&target, cx, |editor| match result {
                    Ok(document) => {
                        editor.document = Some(Document {
                            implementation_id: document.implementation_id,
                            revision: document.revision,
                            graph: Rc::new(document.graph),
                        });
                        editor.rebuild();
                    }
                    Err(error) => editor.error = Some(error.to_string()),
                });
            })
            .ok();
        })
        .detach();
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
/// `text-[10px] text-gray-400` — the label over a param control. The arbitrary
/// size resets the inherited leading to `normal`, hence 13.3 rather than the
/// card's 16.
const PARAM_LABEL_SIZE: f32 = 10.;
const PARAM_LABEL_LINE: f32 = 13.3;
/// `h-7` on the node's `<Input>` — the one control in the app that is not
/// `h-6`, and a smell the web side owns (`style-spec.md` §6).
const FIELD_HEIGHT: f32 = 28.;
/// A bare `<input>`'s intrinsic width — `size=20` at this face and size. The
/// class list says `w-full`, but a shrink-to-fit card sizes to max-content, so
/// this is what actually sets the width of every card carrying a text field.
const FIELD_WIDTH: f32 = 212.;
/// `h-6` on the `<Selector>` trigger, and its `text-[9px] uppercase
/// tracking-wider font-bold` face.
const SELECT_HEIGHT: f32 = 24.;
const SELECT_TEXT: f32 = 9.;
const SELECT_TRACKING: f32 = 0.45;
/// `size-3` on the trigger's chevron.
const SELECT_CHEVRON: f32 = 12.;
/// `border` — one pixel, on every control.
const CONTROL_BORDER: f32 = 1.;
/// `max-w-48` on the audio-input and falloff bodies.
const BODY_MAX_WIDTH: f32 = 192.;
/// `text-[11px]` — the audio node's track name and the falloff blurb.
const BODY_TEXT: f32 = 11.;
const BODY_TEXT_LINE: f32 = 14.6;
/// `text-[9px]` — the falloff help lines.
const HELP_TEXT: f32 = 9.;
const HELP_TEXT_LINE: f32 = 12.;
/// The view node's canvas, in CSS pixels.
const PLOT_WIDTH: f32 = 720.;
const PLOT_HEIGHT: f32 = 140.;
/// The inset `drawSignal` keeps around the trace, and the corner the axis
/// readings are written into.
const PLOT_INSET: f32 = 6.;
/// `ctx.lineWidth` on a trace, and the mono face its axis is labelled in.
const TRACE_WIDTH: f32 = 1.5;
const AXIS_TEXT: f32 = 10.;
const AXIS_TEXT_LINE: f32 = 13.;
/// The web node caps its legend at eight series — past that the chips wrap
/// into a second row and stop being a legend.
const LEGEND_LIMIT: usize = 8;
/// `p-1` around the legend row, `gap-1` between chips.
const LEGEND_PAD: f32 = 4.;
const LEGEND_GAP: f32 = 4.;
/// One chip: `border` + `py-0.5` around a 12px line box, `px-1` inside it.
const CHIP_HEIGHT: f32 = 18.;
const CHIP_BORDER: f32 = 1.;
const CHIP_PAD_H: f32 = 4.;
/// `size-2` on the chip's dot, `text-[9px]` on both of its runs, and `w-8` on
/// the reading — a fixed box, so a column of chips lines its numbers up.
const CHIP_DOT: f32 = 8.;
const CHIP_TEXT: f32 = 9.;
const CHIP_VALUE_WIDTH: f32 = 32.;
/// `h-7` on the slider slab, matching the field it sits beside.
const SLIDER_HEIGHT: f32 = 28.;
const SLIDER_VALUE_TEXT: f32 = 10.;
/// Below this, a label is a smudge — so the paint drops the text and keeps the
/// shape. The cheapest kind of level of detail, and the one that matters:
/// shaping is the most expensive thing on this canvas. Under the web's 0.5
/// minimum zoom, because this canvas is allowed further out than that.
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
    node_id: SharedString,
    title: SharedString,
    origin: Point<f32>,
    /// Outer box, borders included. Resolved by [`Scene::measure`].
    width: f32,
    height: f32,
    /// Top of [`Card::body`], relative to the card's outer origin.
    body_top: f32,
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    body: Body,
    /// The card's interactive regions, card-local, in resolution order —
    /// ports first (their grab boxes out-rank everything they overlap), then
    /// widget slots, then the header. Resolved by [`Scene::measure`], which is
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
        // Slabs sit `PAD_H` in from the content box and — bar the
        // self-sized select trigger — run to its far edge, exactly where
        // `paint_body` draws them.
        let slab_left = CARD_BORDER + PAD_H;
        let slab_width = self.width - (CARD_BORDER + PAD_H) * 2.;
        let widget = |index: usize,
                      slot_y: f32,
                      width: f32,
                      height: f32,
                      kind: WidgetKind,
                      id: &SharedString| Region {
            origin: point(slab_left, self.body_top + slot_y),
            size: size(width, height),
            kind: RegionKind::Widget { param: index, kind },
            label: format!("{} param {id}", self.node_id).into(),
        };
        match &self.body {
            Body::Params(params) => {
                for (index, param) in params.iter().enumerate() {
                    regions.push(match &param.control {
                        Control::Field(_) => widget(
                            index,
                            param.slot_y,
                            slab_width,
                            FIELD_HEIGHT,
                            WidgetKind::Field,
                            &param.id,
                        ),
                        Control::Select { width, .. } => widget(
                            index,
                            param.slot_y,
                            *width,
                            SELECT_HEIGHT,
                            WidgetKind::Select,
                            &param.id,
                        ),
                    });
                }
            }
            Body::Falloff { rows, .. } => {
                for (index, row) in rows.iter().enumerate() {
                    regions.push(widget(
                        index,
                        row.slot_y,
                        slab_width,
                        SLIDER_HEIGHT,
                        WidgetKind::Slider,
                        &row.id,
                    ));
                }
            }
            Body::None | Body::Notice { .. } | Body::Plot(_) => {}
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
    Widget { param: usize, kind: WidgetKind },
    Header,
}

/// What kind of control a widget slot is a picture of — which decides both
/// the harness role it registers under and, in phase 3, which tier edits it.
#[derive(Clone, Copy)]
enum WidgetKind {
    Field,
    Select,
    Slider,
}

/// What the pointer is over, resolved by [`Scene::hit`]: the one question
/// every press, hover and future gesture asks of the canvas.
enum Hit {
    Port {
        card: usize,
        #[allow(dead_code)] // the wire drag (phase 2) starts from these two
        port: usize,
        #[allow(dead_code)]
        output: bool,
    },
    Widget {
        card: usize,
        #[allow(dead_code)] // param editing (phase 3) routes on these two
        param: usize,
        #[allow(dead_code)]
        kind: WidgetKind,
    },
    Header {
        card: usize,
    },
    Body {
        card: usize,
    },
    Wire {
        #[allow(dead_code)] // disconnect (phase 2) names the edge with this
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

/// What a card carries under its port block. The web side spells these as one
/// custom node component each (`standard-node.tsx`, `falloff-node.tsx`, …);
/// they are one closed vocabulary here because a canvas has no component tree
/// to hang them off, and because four shapes is the whole set this catalogue
/// needs.
enum Body {
    None,
    /// A stack of labelled controls — `StandardNode`, `MathNode`,
    /// `GetAttributeNode`.
    Params(Vec<Param>),
    /// `AudioInputNode`: a track name over a time range.
    Notice {
        title: SharedString,
        subtitle: SharedString,
    },
    /// `ViewSignalNode`: the signal plot, and the trace the view-data store
    /// had for this node — `None` while it is still waiting for one.
    Plot(Option<Trace>),
    /// `FalloffNode`: a wrapped blurb over labelled value slabs.
    Falloff {
        blurb: SharedString,
        rows: Vec<SliderRow>,
    },
}

struct Param {
    /// The catalogue param id — what the region and the write path both name
    /// the param by.
    id: SharedString,
    /// `None` only for the bodies that lay their own label out (the falloff
    /// sliders); every catalogue param labels its control.
    label: Option<SharedString>,
    control: Control,
    /// Top of the control's slab, relative to the body's top. Resolved by
    /// [`Body::measure`], whose walk is the one place that knows it.
    slot_y: f32,
}

enum Control {
    /// `<Input>` — a value in a control-fill slab.
    Field(SharedString),
    /// `<Selector>` — an uppercase value with a chevron, sized to the widest
    /// option by the ghost stack (`select.tsx`), which is why the option set
    /// travels with the value.
    Select {
        value: SharedString,
        options: Vec<ParamOption>,
        /// Trigger width, resolved by [`Scene::measure`].
        width: f32,
    },
}

struct SliderRow {
    /// The catalogue param id, as on [`Param::id`].
    id: SharedString,
    label: SharedString,
    value: f32,
    min: f32,
    max: f32,
    help: SharedString,
    /// `help`, broken to the body width. Resolved by [`Scene::measure`].
    help_lines: Vec<SharedString>,
    /// Top of the slider slab, relative to the body's top — see
    /// [`Param::slot_y`].
    slot_y: f32,
}

/// One view node's plot, resolved from the [`Signal`] the store handed it.
///
/// Everything a signal decides is decided here, once per publish, rather than
/// per frame: the scan for the range, the per-sample transform into plot
/// coordinates, the two axis readings and the legend. A repaint of a plot is
/// then one stroked path per series and a handful of runs — which is the same
/// bargain `drawSignal` makes by living inside `requestAnimationFrame`, minus
/// the frame it is throttled to.
struct Trace {
    /// One polyline per series, in plot-local pixels (origin at the plot box's
    /// top-left, y already flipped).
    lines: Vec<Vec<Point<f32>>>,
    /// The range's ends, as the axis writes them: two decimals, max at the top
    /// of the plot and min at the bottom.
    max: SharedString,
    min: SharedString,
    /// One chip per series, up to the web node's limit of eight.
    legend: Vec<Chip>,
}

/// One legend chip: a dot in the series hue, its name, and its reading at the
/// last time step.
struct Chip {
    label: SharedString,
    value: SharedString,
    color: Rgba,
    /// The chip's outer width, resolved by [`Scene::measure`] — it is a
    /// shrink-to-fit box around a shaped label.
    width: f32,
}

struct Link {
    from: (usize, usize),
    to: (usize, usize),
    color: Rgba,
}

impl Scene {
    fn build(
        graph: &Graph,
        types: &HashMap<String, NodeTypeDef>,
        views: &HashMap<String, Signal>,
    ) -> Self {
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

        // `pattern_args` has no catalogue entry: its ports *are* the pattern's
        // argument list, so the definition is synthesized here exactly as
        // `pattern-args-node-def.ts` synthesizes it on the web side.
        let synthetic = pattern_args_def(&graph.args);

        let cards: Vec<Card> = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let definition = match instance.type_id.as_str() {
                    "pattern_args" => synthetic.as_ref(),
                    other => types.get(other),
                };
                let ports = |defs: &[PortDef], output: bool| {
                    defs.iter()
                        .map(|port| Port {
                            id: port.id.clone().into(),
                            label: port.name.clone().into(),
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
                    node_id: instance.id.clone().into(),
                    // A type the catalogue does not know still gets a card: it
                    // is in the document, so hiding it would be a graph the
                    // editor cannot show and cannot fix.
                    title: definition
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| instance.type_id.clone())
                        .into(),
                    origin: placement(instance.position_x, instance.position_y, index),
                    width: CARD_MIN_WIDTH,
                    height: 0.,
                    body_top: 0.,
                    inputs: definition
                        .map(|d| ports(&d.inputs, false))
                        .unwrap_or_default(),
                    outputs: definition
                        .map(|d| ports(&d.outputs, true))
                        .unwrap_or_default(),
                    body: body_for(
                        &instance.type_id,
                        definition.map(|d| d.params.as_slice()).unwrap_or_default(),
                        &instance.params,
                        views.get(&instance.id),
                    ),
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

            let (body_width, body_height) = card.body.measure(window);
            let content = ports_width
                .max(body_width)
                .max(CARD_MIN_WIDTH - CARD_BORDER * 2.);

            card.width = content + CARD_BORDER * 2.;
            card.body_top = CARD_BORDER + HEADER_HEIGHT + ports_height;
            card.height = card.body_top + body_height + CARD_BORDER;

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
        self.measured = true;
    }

    /// What the pointer at `at` (graph space) is over, topmost card first —
    /// cards are painted in order, so the last one containing the point is on
    /// top. Within a card the resolution is one linear scan of its regions,
    /// so the whole test stays O(cards) with a small constant. A wire is only
    /// consulted when no card claims the point; `zoom` converts its
    /// screen-space slack ([`WIRE_GRAB`]) into graph units.
    fn hit(&self, at: Point<f32>, zoom: f32) -> Hit {
        for (index, card) in self.cards.iter().enumerate().rev() {
            if !card.contains(at) {
                continue;
            }
            let local = point(at.x - card.origin.x, at.y - card.origin.y);
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
                    RegionKind::Widget { param, kind } => Hit::Widget {
                        card: index,
                        param,
                        kind,
                    },
                    RegionKind::Header => Hit::Header { card: index },
                };
            }
            return Hit::Body { card: index };
        }
        let slack = WIRE_GRAB / zoom;
        for (index, link) in self.links.iter().enumerate() {
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

impl Body {
    /// The body's max-content width and its height, both in graph space.
    fn measure(&mut self, window: &Window) -> (f32, f32) {
        match self {
            Body::None => (0., 0.),
            Body::Params(params) => {
                let mut width: f32 = 0.;
                let mut height = PAD * 2.;
                // A second accumulator rather than `height - PAD`: the height
                // sum must stay term-for-term what it always was (a card's
                // pixel height is pinned by fixtures, and float addition is
                // not associative), while the slot wants the running y the
                // paint walk will reach — the same sequence `paint_body`
                // steps through.
                let mut y = PAD;
                for param in params.iter_mut() {
                    if let Some(label) = &param.label {
                        width = width.max(
                            PAD_H * 2.
                                + run_width(label, PARAM_LABEL_SIZE, FontWeight::NORMAL, window),
                        );
                        height += PARAM_LABEL_LINE + PAD;
                        y += PARAM_LABEL_LINE + PAD;
                    }
                    param.slot_y = y;
                    match &mut param.control {
                        Control::Field(_) => {
                            width = width.max(PAD_H * 2. + FIELD_WIDTH);
                            height += FIELD_HEIGHT;
                            y += FIELD_HEIGHT;
                        }
                        Control::Select {
                            options,
                            width: trigger,
                            ..
                        } => {
                            // The ghost stack: the trigger is as wide as its
                            // widest option, so it never resizes on a change.
                            let widest = options
                                .iter()
                                .map(|option| {
                                    paint::tracked_width(
                                        &SharedString::from(option.label.to_uppercase()),
                                        SELECT_TEXT,
                                        FontWeight::BOLD,
                                        SELECT_TRACKING,
                                        window,
                                    )
                                })
                                .fold(0., f32::max);
                            *trigger = CONTROL_BORDER * 2.
                                + PAD_H * 2.
                                + widest
                                + COLUMN_GAP
                                + SELECT_CHEVRON;
                            width = width.max(PAD_H * 2. + *trigger);
                            height += SELECT_HEIGHT;
                            y += SELECT_HEIGHT;
                        }
                    }
                    height += PAD;
                    y += PAD;
                }
                (width, height)
            }
            Body::Notice { title, subtitle } => (
                (PAD_H * 2.
                    + run_width(title, BODY_TEXT, FontWeight::MEDIUM, window).max(run_width(
                        subtitle,
                        PARAM_LABEL_SIZE,
                        FontWeight::NORMAL,
                        window,
                    )))
                .min(BODY_MAX_WIDTH),
                BODY_TEXT_LINE + PARAM_LABEL_LINE + PAD_H,
            ),
            Body::Plot(trace) => {
                let legend = match trace {
                    Some(trace) => {
                        for chip in &mut trace.legend {
                            chip.width = CHIP_BORDER * 2.
                                + CHIP_PAD_H * 2.
                                + CHIP_DOT
                                + LEGEND_GAP
                                + run_width(&chip.label, CHIP_TEXT, FontWeight::NORMAL, window)
                                + LEGEND_GAP
                                + CHIP_VALUE_WIDTH;
                        }
                        trace.legend_height()
                    }
                    None => 0.,
                };
                (PLOT_WIDTH, PLOT_HEIGHT + legend)
            }
            Body::Falloff { blurb, rows } => {
                let inner = BODY_MAX_WIDTH - PAD_H * 2.;
                let blurb_lines = wrap(blurb, BODY_TEXT, inner, window).len() as f32;
                let mut height = PAD_H * 2. + blurb_lines * BODY_TEXT_LINE;
                // The paint walk's y, alongside the height sum — see the
                // twin comment in the `Params` arm.
                let mut y = PAD_H + blurb_lines * BODY_TEXT_LINE;
                for row in rows.iter_mut() {
                    row.help_lines = wrap(&row.help, HELP_TEXT, inner, window);
                    row.slot_y = y + PAD_H + PARAM_LABEL_LINE + PAD;
                    // `space-y-2` above each group, `space-y-1` within it.
                    height += PAD_H
                        + PARAM_LABEL_LINE
                        + PAD
                        + SLIDER_HEIGHT
                        + PAD
                        + row.help_lines.len() as f32 * HELP_TEXT_LINE;
                    y += PAD_H
                        + PARAM_LABEL_LINE
                        + PAD
                        + SLIDER_HEIGHT
                        + PAD
                        + row.help_lines.len() as f32 * HELP_TEXT_LINE;
                }
                (BODY_MAX_WIDTH, height)
            }
        }
    }
}

/// How wide one run is in the app's face, in logical pixels.
fn run_width(text: &SharedString, size: f32, weight: FontWeight, window: &Window) -> f32 {
    f32::from(paint::shape(text, size, weight, ladder::foreground(), window).width)
}

/// Break `text` to `width`, greedily, on spaces — the one thing a canvas has
/// to do for itself that a `<p>` does for free.
fn wrap(text: &SharedString, size: f32, width: f32, window: &Window) -> Vec<SharedString> {
    let mut lines: Vec<SharedString> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate: SharedString = if line.is_empty() {
            word.to_string().into()
        } else {
            format!("{line} {word}").into()
        };
        if run_width(&candidate, size, FontWeight::NORMAL, window) > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line).into());
            line = word.to_string();
        } else {
            line = candidate.to_string();
        }
    }
    if !line.is_empty() {
        lines.push(line.into());
    }
    lines
}

impl Trace {
    /// Resolve a signal into what the plot draws. Mirrors `drawSignal` and the
    /// legend memo in `view-channel-node.tsx` sample for sample: the same
    /// choice of series axis, the same flat-buffer indexing, the same
    /// normalization against the signal's own range, and the same reading of
    /// the last time step for the legend.
    ///
    /// `None` for a signal there is nothing to draw from — no samples, or a
    /// range that is not finite — which is the web canvas's early return, and
    /// leaves the node in its waiting state rather than drawing a flat line
    /// that would look like data.
    fn from_signal(signal: &Signal) -> Option<Self> {
        let Signal { n, t, c, data } = signal;
        let (n, t, c) = (*n, *t, *c);
        if data.is_empty() || t == 0 || c == 0 {
            return None;
        }
        // A signal with a spatial dimension plots one line per primitive;
        // otherwise the channels are the series.
        let spatial = n > 1;
        let series = if spatial { n } else { c };
        let (min, max) = data
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), value| {
                (lo.min(*value), hi.max(*value))
            });
        if !min.is_finite() || !max.is_finite() {
            return None;
        }
        let range = (max - min).max(1e-6);
        let width = PLOT_WIDTH - PLOT_INSET * 2.;
        let height = PLOT_HEIGHT - PLOT_INSET * 2.;
        let at = |line: usize, step: usize| {
            let index = if spatial {
                line * (t * c) + step * c
            } else {
                step * c + line
            };
            let value = data.get(index).copied().unwrap_or(0.);
            PLOT_HEIGHT - PLOT_INSET - ((value - min) / range).clamp(0., 1.) * height
        };

        let lines = (0..series)
            .map(|line| {
                // One sample in time is a level, not a curve: the web canvas
                // rules it across the whole plot rather than plotting a point
                // nobody could see.
                if t == 1 {
                    let y = at(line, 0);
                    return vec![point(PLOT_INSET, y), point(PLOT_WIDTH - PLOT_INSET, y)];
                }
                (0..t)
                    .map(|step| {
                        point(
                            PLOT_INSET + step as f32 / (t - 1) as f32 * width,
                            at(line, step),
                        )
                    })
                    .collect()
            })
            .collect();

        let last = t - 1;
        let legend = (0..series.min(LEGEND_LIMIT))
            .map(|line| {
                let index = if spatial {
                    line * (t * c) + last * c
                } else {
                    last * c + line
                };
                Chip {
                    label: if spatial {
                        format!("Prim {line}")
                    } else {
                        format!("Ch {line}")
                    }
                    .into(),
                    value: format!("{:.2}", data.get(index).copied().unwrap_or(0.)).into(),
                    color: ladder::plot_trace(line),
                    width: 0.,
                }
            })
            .collect();

        Some(Self {
            lines,
            max: format!("{max:.2}").into(),
            min: format!("{min:.2}").into(),
            legend,
        })
    }

    /// The legend's chips, broken into rows at the plot's width — `flex-wrap`,
    /// with the same 4px gap between rows as between chips. Pure over the
    /// resolved widths, so the measure pass and the paint agree by
    /// construction rather than by both getting it right.
    fn legend_rows(&self) -> Vec<&[Chip]> {
        let room = PLOT_WIDTH - LEGEND_PAD * 2.;
        let mut rows = Vec::new();
        let mut start = 0;
        let mut used = 0.;
        for (index, chip) in self.legend.iter().enumerate() {
            let step = if index == start {
                chip.width
            } else {
                LEGEND_GAP + chip.width
            };
            if index > start && used + step > room {
                rows.push(&self.legend[start..index]);
                start = index;
                used = chip.width;
            } else {
                used += step;
            }
        }
        if start < self.legend.len() {
            rows.push(&self.legend[start..]);
        }
        rows
    }

    /// How tall the legend block under the plot is, borders and padding
    /// included. Zero when there are no chips — the web node omits the whole
    /// `<div>` in that case, and the card is that much shorter.
    fn legend_height(&self) -> f32 {
        let rows = self.legend_rows().len();
        if rows == 0 {
            0.
        } else {
            LEGEND_PAD * 2. + rows as f32 * CHIP_HEIGHT + (rows - 1) as f32 * LEGEND_GAP
        }
    }
}

/// The body a node type renders, from its param definitions and the instance's
/// values. Mirrors the web side's node-component registry (`nodes.tsx`): the
/// named types render their own body, everything else falls through to
/// `StandardNode`'s labelled controls.
fn body_for(
    type_id: &str,
    params: &[ParamDef],
    values: &HashMap<String, serde_json::Value>,
    signal: Option<&Signal>,
) -> Body {
    match type_id {
        "view_signal" | "uv_view" => Body::Plot(signal.and_then(Trace::from_signal)),
        "audio_input" => Body::Notice {
            // No annotation context in this host yet, so both lines are the
            // web node's own "nothing selected" copy rather than invented
            // track metadata.
            title: "Select an annotation".into(),
            subtitle: "Pick an instance from the left pane".into(),
        },
        "falloff" => Body::Falloff {
            blurb: "Softly tightens a 0..1 signal (or distance) into a pill-shaped falloff.".into(),
            // Bounds and defaults come from the catalogue (`ParamDef::range`);
            // a number the catalogue gives no bounds draws no slider. Only the
            // help copy — view prose the catalogue does not carry — stays here.
            rows: params
                .iter()
                .filter_map(|param| {
                    let (min, max) = param.range?;
                    Some(SliderRow {
                        id: param.id.clone().into(),
                        label: param.name.clone().into(),
                        value: number(values, &param.id, param.default_number.unwrap_or(0.)),
                        min,
                        max,
                        help: match param.id.as_str() {
                            "width" => "Higher = tighter pill; lower = wider falloff.",
                            "curve" => "Negative = softer edges, positive = snappier edges.",
                            _ => "",
                        }
                        .into(),
                        help_lines: Vec::new(),
                        slot_y: 0.,
                    })
                })
                .collect(),
        },
        _ if params.is_empty() => Body::None,
        _ => Body::Params(
            params
                .iter()
                .map(|param| Param {
                    id: param.id.clone().into(),
                    label: Some(param.name.clone().into()),
                    slot_y: 0.,
                    control: match &param.param_type {
                        ParamType::Number => Control::Field(
                            decimal(number(
                                values,
                                &param.id,
                                param.default_number.unwrap_or(0.),
                            ))
                            .into(),
                        ),
                        ParamType::Text => Control::Field(
                            text(values, &param.id, param).unwrap_or_default().into(),
                        ),
                        ParamType::Enum { options } => {
                            let chosen = text(values, &param.id, param);
                            Control::Select {
                                value: options
                                    .iter()
                                    .find(|option| Some(option.id.as_str()) == chosen.as_deref())
                                    .map(|option| option.label.to_uppercase())
                                    .unwrap_or_else(|| "SELECT…".to_string())
                                    .into(),
                                options: options.clone(),
                                width: 0.,
                            }
                        }
                    },
                })
                .collect(),
        ),
    }
}

fn number(values: &HashMap<String, serde_json::Value>, id: &str, fallback: f32) -> f32 {
    values
        .get(id)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
        .unwrap_or(fallback)
}

fn text(values: &HashMap<String, serde_json::Value>, id: &str, param: &ParamDef) -> Option<String> {
    values
        .get(id)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| param.default_text.clone())
}

/// A number as `<input type="number">` shows it — no trailing `.0`, which is
/// what a JavaScript `toString` would never produce.
fn decimal(value: f32) -> String {
    if value.fract() == 0. {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// A node's stored position, or the same fallback grid the web editor lays out
/// when one is missing: five across, 200 × 150 apart.
fn placement(x: Option<f64>, y: Option<f64>, index: usize) -> Point<f32> {
    point(
        x.map(|x| x as f32).unwrap_or((index % 5) as f32 * 200.),
        y.map(|y| y as f32).unwrap_or((index / 5) as f32 * 150.),
    )
}

// -- rendering ----------------------------------------------------------------

/// Render the screen: a toolbar strip over the canvas.
///
/// The toolbar is ordinary elements and the canvas is one painted element.
/// That split is the whole layout decision — a panel is a stack of boxes and
/// gpui already lays boxes out well, while a graph is a coordinate system and
/// nothing gpui lays out could express it without a box per node.
pub fn graph(state: &Editor, app: &Entity<Luma>) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        .child(match (&state.error, &state.document) {
            (Some(message), _) => luma_ui::plate(message.clone(), ladder::danger()),
            (None, None) => {
                luma_ui::plate("Loading graph…".to_string(), ladder::muted_foreground())
            }
            (None, Some(_)) => canvas_element(state, app).into_any_element(),
        })
}

/// What is open, how big it is, and whether a write is in the air. Nothing
/// here is a control the canvas needs — the canvas is driven by the pointer —
/// so the strip stays a readout. Closing lives on the tab's chip.
fn toolbar(state: &Editor, _app: &Entity<Luma>) -> Div {
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
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.pattern.name.clone())
                .agent_node(Role::Text, state.pattern.name.clone()),
        )
        .child(luma_ui::silkscreen(format!("{nodes} NODES")))
        // The resolved context, shown: implicit context that silently changes
        // what the plots mean is obscurity; a context that is never shown is
        // worse than none (§6).
        .child(luma_ui::silkscreen(format!(
            "TRACK {}",
            state.context.track_name.to_uppercase()
        )))
        .child(div().flex_1())
        // Named rather than merely drawn: "did the write land" is the question
        // this screen exists to answer, and inferring it from a node's
        // coordinates would be guessing.
        .when(state.saving || state.dirty, |el| {
            el.child(luma_ui::silkscreen("SAVING".to_string()))
        })
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
    let registered = Rc::clone(&state.scene);
    let painted = Rc::clone(&state.scene);
    let measured_view = Rc::clone(&state.view);
    let painted_view = Rc::clone(&state.view);
    let registered_selected = state.selected.clone();
    let selected = state.selected.clone();
    let marquee = match &state.gesture {
        Some(Gesture::Marquee { from, to }) => Some((*from, *to)),
        _ => None,
    };
    let origin = Rc::clone(&state.origin);
    let fit = state.fit;
    let fitted_size = Rc::clone(&state.fitted_size);
    // The tab the canvas belongs to: every handler this frame registers is
    // addressed to it, not to whatever tab is visible when the event lands.
    let target = Target::Graph {
        pattern: state.pattern.id.clone(),
    };
    let app = app.clone();

    div().flex_1().overflow_hidden().child(
        canvas(
            move |bounds, window, cx| {
                // Where the canvas ended up is what turns a window-space mouse
                // position back into a graph one, and only prepaint knows it.
                // A press can arrive before the next paint but never before the
                // next prepaint, so this is also the only place it is safe to
                // write.
                origin.set(bounds.origin);
                {
                    let mut scene = registered.borrow_mut();
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
                            RegionKind::Widget { kind, .. } => match kind {
                                WidgetKind::Field => Role::Input,
                                WidgetKind::Select => Role::Select,
                                WidgetKind::Slider => Role::Slider,
                            },
                            // The card node already names the whole card; a
                            // header node would be a second spelling of it.
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
                    marquee,
                    window,
                    cx,
                );
                listen(&app, target.clone(), &hitbox, window);
            },
        )
        .size_full(),
    )
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
        if phase != DispatchPhase::Bubble
            || event.button != MouseButton::Left
            || !inside.is_hovered(window)
        {
            return;
        }
        let at = event.position;
        let shift = event.modifiers.shift;
        pressed.update(cx, |this, cx| {
            this.graph_press(&press_target, at, shift, cx)
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
            released.update(cx, |this, cx| this.graph_release(&release_target, cx));
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
fn paint(
    bounds: Bounds<Pixels>,
    scene: &Scene,
    view: Viewport,
    selected: &[SharedString],
    marquee: Option<(Point<f32>, Point<f32>)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(fill(bounds, ladder::background()));

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for link in &scene.links {
            let (from, to) = scene.ends(link);
            paint_wire(bounds.origin, from, to, link.color, view, window);
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
        for (ports, output) in [(&card.inputs, false), (&card.outputs, true)] {
            for port in ports {
                paint_port_label(box_.origin, port, zoom, output, window, cx);
            }
        }
        paint_body(box_, card, zoom, window, cx);
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

/// A port mark: a ring, and a dot inside it when the port is wired. gpui has
/// no circle primitive, so both are fully rounded quads — which is exactly
/// what the web side draws them as (`rounded-full`).
fn paint_ring(card: Point<Pixels>, port: &Port, zoom: f32, window: &mut Window) {
    let centre = point(card.x + px(port.at.x * zoom), card.y + px(port.at.y * zoom));
    let diameter = px(PORT_RING * zoom);
    window.paint_quad(quad(
        Bounds::centered_at(centre, size(diameter, diameter)),
        Corners::all(diameter / 2.),
        transparent_black(),
        Edges::all(px(PORT_RING_BORDER * zoom)),
        port.color,
        BorderStyle::Solid,
    ));
    if port.connected {
        let dot = px(PORT_DOT * zoom);
        window.paint_quad(quad(
            Bounds::centered_at(centre, size(dot, dot)),
            Corners::all(dot / 2.),
            port.color,
            Edges::default(),
            transparent_black(),
            BorderStyle::Solid,
        ));
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
fn paint_body(box_: Bounds<Pixels>, card: &Card, zoom: f32, window: &mut Window, cx: &mut App) {
    let left = box_.origin.x + px(CARD_BORDER * zoom);
    let top = box_.origin.y + px(card.body_top * zoom);
    let inner_width = box_.size.width - px(CARD_BORDER * 2. * zoom);
    let text_left = left + px(PAD_H * zoom);

    match &card.body {
        Body::None => {}
        Body::Params(params) => {
            let mut y = top + px(PAD * zoom);
            for param in params {
                if let Some(label) = &param.label {
                    paint::line(
                        point(text_left, y),
                        label,
                        PARAM_LABEL_SIZE * zoom,
                        FontWeight::NORMAL,
                        ladder::param_label(),
                        window,
                        cx,
                    );
                    y += px((PARAM_LABEL_LINE + PAD) * zoom);
                }
                match &param.control {
                    Control::Field(value) => {
                        let slab = Bounds {
                            origin: point(text_left, y),
                            size: size(
                                inner_width - px(PAD_H * 2. * zoom),
                                px(FIELD_HEIGHT * zoom),
                            ),
                        };
                        paint_control(slab, zoom, window);
                        centred_line(
                            slab,
                            px((CONTROL_BORDER + PAD_H) * zoom),
                            value,
                            TEXT_SIZE * zoom,
                            FontWeight::NORMAL,
                            ladder::foreground(),
                            window,
                            cx,
                        );
                        y += px(FIELD_HEIGHT * zoom);
                    }
                    Control::Select { value, width, .. } => {
                        let slab = Bounds {
                            origin: point(text_left, y),
                            size: size(px(width * zoom), px(SELECT_HEIGHT * zoom)),
                        };
                        paint_control(slab, zoom, window);
                        paint::tracked(
                            point(
                                slab.origin.x + px((CONTROL_BORDER + PAD_H) * zoom),
                                slab.origin.y
                                    + (slab.size.height
                                        - px(SELECT_TEXT * zoom * paint::LINE_HEIGHT))
                                        / 2.,
                            ),
                            value,
                            SELECT_TEXT * zoom,
                            FontWeight::BOLD,
                            ladder::foreground_90(),
                            SELECT_TRACKING * zoom,
                            window,
                            cx,
                        );
                        paint_chevron(
                            point(
                                slab.origin.x + slab.size.width
                                    - px((CONTROL_BORDER + PAD_H + SELECT_CHEVRON / 2.) * zoom),
                                slab.origin.y + slab.size.height / 2.,
                            ),
                            zoom,
                            window,
                        );
                        y += px(SELECT_HEIGHT * zoom);
                    }
                }
                y += px(PAD * zoom);
            }
        }
        Body::Notice { title, subtitle } => {
            paint::line(
                point(text_left, top),
                title,
                BODY_TEXT * zoom,
                FontWeight::MEDIUM,
                ladder::foreground(),
                window,
                cx,
            );
            paint::line(
                point(text_left, top + px(BODY_TEXT_LINE * zoom)),
                subtitle,
                PARAM_LABEL_SIZE * zoom,
                FontWeight::NORMAL,
                ladder::muted_foreground(),
                window,
                cx,
            );
        }
        Body::Plot(trace) => {
            let plot = Bounds {
                origin: point(left, top),
                size: size(px(PLOT_WIDTH * zoom), px(PLOT_HEIGHT * zoom)),
            };
            window.paint_quad(fill(plot, ladder::background()));
            match trace {
                Some(trace) => paint_plot(plot, trace, zoom, window, cx),
                None => centred_line(
                    plot,
                    (plot.size.width
                        - run_width(&WAITING, BODY_TEXT * zoom, FontWeight::NORMAL, window).into())
                        / 2.,
                    &WAITING,
                    BODY_TEXT * zoom,
                    FontWeight::NORMAL,
                    ladder::plot_empty(),
                    window,
                    cx,
                ),
            }
        }
        Body::Falloff { blurb, rows } => {
            let mut y = top + px(PAD_H * zoom);
            for line in wrap(
                blurb,
                BODY_TEXT * zoom,
                (BODY_MAX_WIDTH - PAD_H * 2.) * zoom,
                window,
            ) {
                paint::line(
                    point(text_left, y),
                    &line,
                    BODY_TEXT * zoom,
                    FontWeight::NORMAL,
                    ladder::muted_foreground(),
                    window,
                    cx,
                );
                y += px(BODY_TEXT_LINE * zoom);
            }
            for row in rows {
                y += px(PAD_H * zoom);
                paint::line(
                    point(text_left, y),
                    &row.label,
                    PARAM_LABEL_SIZE * zoom,
                    FontWeight::NORMAL,
                    ladder::muted_foreground(),
                    window,
                    cx,
                );
                let reading: SharedString = format!("{:.2}", row.value).into();
                let shaped = paint::shape_in(
                    fonts::MONO,
                    &reading,
                    PARAM_LABEL_SIZE * zoom,
                    FontWeight::NORMAL,
                    ladder::muted_foreground(),
                    window,
                );
                let right = left + inner_width - px(PAD_H * zoom) - shaped.width;
                shaped
                    .paint(
                        point(right, y),
                        px(PARAM_LABEL_LINE * zoom),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
                y += px((PARAM_LABEL_LINE + PAD) * zoom);

                let slab = Bounds {
                    origin: point(text_left, y),
                    size: size(
                        inner_width - px(PAD_H * 2. * zoom),
                        px(SLIDER_HEIGHT * zoom),
                    ),
                };
                paint_slider(slab, row, zoom, window, cx);
                y += px((SLIDER_HEIGHT + PAD) * zoom);

                for line in &row.help_lines {
                    paint::line(
                        point(text_left, y),
                        line,
                        HELP_TEXT * zoom,
                        FontWeight::NORMAL,
                        ladder::muted_foreground(),
                        window,
                        cx,
                    );
                    y += px(HELP_TEXT_LINE * zoom);
                }
            }
        }
    }
}

/// The view node's empty state, in the web node's own words.
const WAITING: SharedString = SharedString::new_static("waiting for signal data…");

/// A view node's readout: the traces, the two axis readings written into the
/// plot's corners, and the legend row under it.
///
/// `plot` is the plot box only — the legend hangs below it, inside the card,
/// which is what the extra height [`Body::measure`] reserves is for.
fn paint_plot(plot: Bounds<Pixels>, trace: &Trace, zoom: f32, window: &mut Window, cx: &mut App) {
    let at = |p: Point<f32>| {
        point(
            plot.origin.x + px(p.x * zoom),
            plot.origin.y + px(p.y * zoom),
        )
    };
    for (index, line) in trace.lines.iter().enumerate() {
        let Some((first, rest)) = line.split_first() else {
            continue;
        };
        let mut path = PathBuilder::stroke(px(TRACE_WIDTH * zoom));
        path.move_to(at(*first));
        for step in rest {
            path.line_to(at(*step));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, ladder::plot_trace(index));
        }
    }

    // `textBaseline = "top"` for the maximum and `"bottom"` for the minimum,
    // both `PLOT_INSET` in from the plot's left edge — so the pair brackets
    // the range the trace was normalized against.
    let inset = px(PLOT_INSET * zoom);
    let line_height = px(AXIS_TEXT_LINE * zoom);
    for (reading, top) in [
        (&trace.max, plot.origin.y + inset),
        (
            &trace.min,
            plot.origin.y + plot.size.height - inset - line_height,
        ),
    ] {
        paint::shape_in(
            fonts::MONO,
            reading,
            AXIS_TEXT * zoom,
            FontWeight::NORMAL,
            ladder::plot_axis(),
            window,
        )
        .paint(
            point(plot.origin.x + inset, top),
            line_height,
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
    }

    let mut y = plot.origin.y + plot.size.height + px(LEGEND_PAD * zoom);
    for row in trace.legend_rows() {
        let mut x = plot.origin.x + px(LEGEND_PAD * zoom);
        for chip in row {
            paint_chip(
                Bounds {
                    origin: point(x, y),
                    size: size(px(chip.width * zoom), px(CHIP_HEIGHT * zoom)),
                },
                chip,
                zoom,
                window,
                cx,
            );
            x += px((chip.width + LEGEND_GAP) * zoom);
        }
        y += px((CHIP_HEIGHT + LEGEND_GAP) * zoom);
    }
}

/// One legend chip: a `bg-white/5` slab inside a `border-white/5` hairline,
/// carrying a dot in the series hue, the series name, and its reading
/// right-aligned in a fixed 32px box.
fn paint_chip(box_: Bounds<Pixels>, chip: &Chip, zoom: f32, window: &mut Window, cx: &mut App) {
    window.paint_quad(quad(
        box_,
        Corners::default(),
        ladder::white_5(),
        Edges::all(px(CHIP_BORDER * zoom)),
        ladder::white_5(),
        BorderStyle::Solid,
    ));
    let dot = px(CHIP_DOT * zoom);
    let left = box_.origin.x + px((CHIP_BORDER + CHIP_PAD_H) * zoom);
    window.paint_quad(quad(
        Bounds::centered_at(
            point(left + dot / 2., box_.origin.y + box_.size.height / 2.),
            size(dot, dot),
        ),
        Corners::all(dot / 2.),
        chip.color,
        Edges::default(),
        transparent_black(),
        BorderStyle::Solid,
    ));
    centred_line(
        box_,
        left - box_.origin.x + dot + px(LEGEND_GAP * zoom),
        &chip.label,
        CHIP_TEXT * zoom,
        FontWeight::NORMAL,
        ladder::legend_label(),
        window,
        cx,
    );
    // `text-right` inside a `w-8` box: the reading is placed against the
    // chip's inner right edge rather than after the label, so a column of
    // chips lines its numbers up.
    let value = paint::shape_in(
        fonts::MONO,
        &chip.value,
        CHIP_TEXT * zoom,
        FontWeight::NORMAL,
        ladder::legend_value(),
        window,
    );
    let right = box_.origin.x + box_.size.width - px((CHIP_BORDER + CHIP_PAD_H) * zoom);
    value
        .paint(
            point(
                right - value.width,
                box_.origin.y + (box_.size.height - px(CHIP_TEXT * zoom * paint::LINE_HEIGHT)) / 2.,
            ),
            px(CHIP_TEXT * zoom * paint::LINE_HEIGHT),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
}

/// One line, vertically centred in `slab` and `indent` in from its left edge —
/// what `flex items-center px-2` does to the value inside a control.
#[allow(clippy::too_many_arguments)]
fn centred_line(
    slab: Bounds<Pixels>,
    indent: Pixels,
    text: &SharedString,
    size: f32,
    weight: FontWeight,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    paint::line(
        point(
            slab.origin.x + indent,
            slab.origin.y + (slab.size.height - px(size * paint::LINE_HEIGHT)) / 2.,
        ),
        text,
        size,
        weight,
        color,
        window,
        cx,
    );
}

/// The one control shape: `--control` fill inside a 1px `--control-border`,
/// square, no focus ring.
fn paint_control(box_: Bounds<Pixels>, zoom: f32, window: &mut Window) {
    window.paint_quad(quad(
        box_,
        Corners::default(),
        ladder::control(),
        Edges::all(px(CONTROL_BORDER * zoom)),
        ladder::control_border(),
        BorderStyle::Solid,
    ));
}

/// The `<Selector>` trigger's chevron: two strokes at `opacity-50` in a 12px
/// box. Drawn rather than iconified because the canvas has no element to hang
/// an SVG on.
fn paint_chevron(centre: Point<Pixels>, zoom: f32, window: &mut Window) {
    let arm = px(SELECT_CHEVRON * 0.29 * zoom);
    let mut path = PathBuilder::stroke(px(1.5 * zoom));
    path.move_to(point(centre.x - arm, centre.y - arm / 2.));
    path.line_to(point(centre.x, centre.y + arm / 2.));
    path.line_to(point(centre.x + arm, centre.y - arm / 2.));
    if let Ok(path) = path.build() {
        window.paint_path(path, ladder::foreground_alpha(0.5));
    }
}

/// The Ableton-style value slab `<Slider>` renders: a recessed `--input` box
/// with a `--primary` fill at `opacity-20` covering value%, and the number in
/// 10px mono over it. Mirrors [`luma_ui::luma_slider`], which is the laid-out
/// version of the same three layers.
fn paint_slider(
    box_: Bounds<Pixels>,
    row: &SliderRow,
    zoom: f32,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(quad(
        box_,
        Corners::default(),
        ladder::apex(),
        Edges::all(px(CONTROL_BORDER * zoom)),
        ladder::control_border(),
        BorderStyle::Solid,
    ));
    let fraction = ((row.value - row.min) / (row.max - row.min)).clamp(0., 1.);
    let mut filled: Hsla = ladder::primary().into();
    filled.a = 0.2;
    window.paint_quad(fill(
        Bounds {
            origin: box_.origin,
            size: size(box_.size.width * fraction, box_.size.height),
        },
        filled,
    ));
    let reading: SharedString = format!("{:.2}", row.value).into();
    paint::shape_in(
        fonts::MONO,
        &reading,
        SLIDER_VALUE_TEXT * zoom,
        FontWeight::NORMAL,
        ladder::primary(),
        window,
    )
    .paint(
        point(
            box_.origin.x + px(PAD_H * zoom),
            box_.origin.y
                + (box_.size.height - px(SLIDER_VALUE_TEXT * zoom * paint::LINE_HEIGHT)) / 2.,
        ),
        px(SLIDER_VALUE_TEXT * zoom * paint::LINE_HEIGHT),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

/// The wire shape, once: a horizontal stub out of each port, one diagonal
/// between them, and a fixed-radius arc at each corner. Mirrors
/// `buildFilletPath` in `src/shared/lib/react-flow/fillet-edge.tsx` — same
/// stub, same radius, same clamp to half of each adjoining segment — so a
/// graph reads the same in both hosts.
fn paint_wire(
    origin: Point<Pixels>,
    from: Point<f32>,
    to: Point<f32>,
    color: Rgba,
    view: Viewport,
    window: &mut Window,
) {
    let corners = [
        from,
        point(from.x + WIRE_STUB, from.y),
        point(to.x - WIRE_STUB, to.y),
        to,
    ];
    let at = |p: Point<f32>| view.to_window(origin, p);
    let mut path = PathBuilder::stroke(px(WIRE_WIDTH * view.zoom));
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
