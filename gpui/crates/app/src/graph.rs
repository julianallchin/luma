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
//! # What v1 is
//!
//! Open, read, look, move. Selection and drag-to-move persist; adding and
//! deleting nodes and edges, editing arguments, the graph agent and the
//! preview heatmap do not exist here yet. The seam already carries all of
//! them — this screen is what is missing, not the commands.
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
use luma_ui::node::{agent_paint_node, Instrument, Role};
use luma_ui::{fonts, ladder, paint};

use luma_lib::models::node_graph::{
    Graph, NodeTypeDef, ParamDef, ParamOption, ParamType, PatternArgDef, PatternArgType, PortDef,
    PortType, Signal,
};
use luma_lib::models::patterns::PatternSummary;

use crate::{Luma, Screen};

// -- state --------------------------------------------------------------------

/// The screen's whole state: the pattern it is editing, the document it read,
/// the resolved geometry of that document, and where the eye and the hand are.
pub struct Editor {
    pattern: PatternSummary,
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
    selected: Option<SharedString>,
    gesture: Option<Gesture>,
    /// Where the eye is. A `Cell` for the same reason [`Self::origin`] is one:
    /// the first framing is a `fitView`, and a fit cannot be computed until
    /// the canvas knows both its own size and the measured graph's — which is
    /// inside a draw.
    view: Rc<Cell<Viewport>>,
    /// Frame the whole graph the next time it is measured, as the web editor
    /// does with `fitView` on mount. Cleared once spent, so a later rebuild (a
    /// save coming back) does not yank the eye.
    fit: bool,
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
struct Document {
    implementation_id: String,
    revision: String,
    graph: Graph,
}

/// What the pointer is doing between a press and a release.
enum Gesture {
    /// Moving the eye. `last` is the previous pointer position, so the pan
    /// follows the pointer exactly regardless of zoom.
    Pan { last: Point<Pixels> },
    /// Moving a node. `grab` is where inside the card the pointer took hold,
    /// in graph space, so the card does not jump to centre itself on the
    /// pointer. `moved` distinguishes a drag from a click that selected.
    Move {
        node: SharedString,
        grab: Point<f32>,
        moved: bool,
    },
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
    }

    /// The card under `at` (graph space), topmost first — cards are painted in
    /// order, so the last one that contains the point is the one on top.
    /// Gives back the node's id and where inside the card the point fell,
    /// rather than a borrow that would outlive the one it was read through.
    fn card_at(&self, at: Point<f32>) -> Option<(SharedString, Point<f32>)> {
        let scene = self.scene.borrow();
        scene.cards.iter().rev().find_map(|card| {
            let inside = at.x >= card.origin.x
                && at.x <= card.origin.x + card.width
                && at.y >= card.origin.y
                && at.y <= card.origin.y + card.height;
            inside.then(|| {
                (
                    card.node_id.clone(),
                    point(at.x - card.origin.x, at.y - card.origin.y),
                )
            })
        })
    }

    /// Move one node to `origin` in graph space, in both the document and the
    /// geometry drawn from it. Wires follow for free: a wire is stored as the
    /// two ports it joins, not as two points.
    fn move_node(&mut self, node: &str, origin: Point<f32>) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(instance) = document.graph.nodes.iter_mut().find(|n| n.id == node) else {
            return;
        };
        instance.position_x = Some(origin.x as f64);
        instance.position_y = Some(origin.y as f64);
        if let Some(card) = self
            .scene
            .borrow_mut()
            .cards
            .iter_mut()
            .find(|card| card.node_id == node)
        {
            card.origin = origin;
        }
    }
}

// -- navigation and gestures --------------------------------------------------
//
// These hang off `Luma` because opening a pattern is a pair of `Library` calls
// plus a screen transition, and `Luma` owns both.

impl Luma {
    /// Navigate to a pattern's graph. The catalogue and the document are read
    /// together — a graph without the catalogue is a list of opaque type ids,
    /// so there is nothing worth drawing until both have landed.
    pub(crate) fn open_pattern(&mut self, pattern: PatternSummary, cx: &mut Context<Self>) {
        let types = self.library.node_types();
        let document = self.library.pattern_graph(&pattern.id);
        self.screen = Screen::Graph(Box::new(Editor {
            pattern,
            types: Rc::new(HashMap::new()),
            views: ViewData::snapshot(cx),
            document: None,
            scene: Rc::new(RefCell::new(Scene::default())),
            selected: None,
            gesture: None,
            view: Rc::new(Cell::new(Viewport {
                pan: point(px(0.), px(0.)),
                zoom: 1.,
            })),
            fit: true,
            origin: Rc::new(Cell::new(Bounds::default().origin)),
            saving: false,
            dirty: false,
            error: None,
        }));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let types = types.await;
            let document = document.await;
            this.update(cx, |this, cx| {
                this.with_editor(cx, |editor| match (types, document) {
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
                            graph: document.graph,
                        });
                        editor.rebuild();
                    }
                    (Err(message), _) | (_, Err(message)) => editor.error = Some(message),
                });
            })
            .ok();
        })
        .detach();
    }

    /// A press on the canvas: take hold of a card, or of the background.
    fn graph_press(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_editor(cx, |editor| {
            let origin = editor.origin.get();
            let cursor = editor.view.get().to_graph(origin, at);
            match editor.card_at(cursor) {
                Some((node, grab)) => {
                    editor.selected = Some(node.clone());
                    editor.gesture = Some(Gesture::Move {
                        node,
                        grab,
                        moved: false,
                    });
                }
                // A press on the background clears the selection as well as
                // starting a pan: the web editor does the same, and a
                // selection that survived a click elsewhere would be a second
                // way to have one.
                None => {
                    editor.selected = None;
                    editor.gesture = Some(Gesture::Pan { last: at });
                }
            }
        });
    }

    /// A pointer move. Registered on the window rather than on the canvas, so
    /// it arrives for every move over the app whether or not a gesture is
    /// running — hence the early return, which is what keeps an idle mouse
    /// from notifying (and so redrawing) once per event.
    fn graph_drag(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        match &self.screen {
            Screen::Graph(editor) if editor.gesture.is_some() => {}
            _ => return,
        }
        let mut moved = None;
        self.with_editor(cx, |editor| {
            let origin = editor.origin.get();
            let mut view = editor.view.get();
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
                }) => {
                    *dragged = true;
                    let cursor = view.to_graph(origin, at);
                    moved = Some((node.clone(), point(cursor.x - grab.x, cursor.y - grab.y)));
                }
                None => {}
            }
        });
        if let Some((node, origin)) = moved {
            self.with_editor(cx, |editor| editor.move_node(&node, origin));
        }
    }

    /// A release. A node that actually moved is written back; a press that
    /// only selected is not, because the document did not change.
    ///
    /// Registered on the window like [`Self::graph_drag`], and guarded the same
    /// way: a click anywhere else in the app must not redraw this screen.
    fn graph_release(&mut self, cx: &mut Context<Self>) {
        match &self.screen {
            Screen::Graph(editor) if editor.gesture.is_some() => {}
            _ => return,
        }
        let mut save = false;
        self.with_editor(cx, |editor| {
            if let Some(Gesture::Move { moved: true, .. }) = editor.gesture.take() {
                save = true;
            }
        });
        if save {
            self.save_graph(cx);
        }
    }

    fn graph_zoom(&mut self, at: Point<Pixels>, wheel: f32, cx: &mut Context<Self>) {
        self.with_editor(cx, |editor| {
            let origin = editor.origin.get();
            let mut view = editor.view.get();
            // Exponential in the scroll distance, so a fast flick and a slow
            // one over the same distance land in the same place.
            view.zoom_about(origin, at, (wheel * ZOOM_PER_PIXEL).exp());
            editor.view.set(view);
        });
    }

    /// Write the graph back, optimistically.
    ///
    /// One write is in flight at a time. A second edit made while the first is
    /// running marks the document dirty and is flushed on its return — two
    /// concurrent writes against one `base_revision` would make the later one
    /// a conflict by construction, which is a fight with the seam rather than
    /// a use of it.
    fn save_graph(&mut self, cx: &mut Context<Self>) {
        let Screen::Graph(editor) = &mut self.screen else {
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
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut flush = false;
                this.with_editor(cx, |editor| {
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
                                    document.graph = saved.graph;
                                    editor.rebuild();
                                }
                            }
                        }
                        // Includes the conflict case, which the seam does not
                        // type on the wire — see the module note in the
                        // pattern handler. There is nothing to merge in a
                        // layout-only editor, so the honest recovery is to say
                        // so and let the reload below re-read the truth.
                        Err(message) => editor.error = Some(message),
                    }
                    flush = editor.dirty;
                    editor.dirty = false;
                });
                if flush {
                    this.save_graph(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Run `edit` against the graph screen, if that is still what is showing.
    /// A load or a save that lands after the user navigated away is a no-op.
    fn with_editor(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Editor)) {
        if let Screen::Graph(editor) = &mut self.screen {
            edit(editor);
            cx.notify();
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
    /// `None` only for the bodies that lay their own label out (the falloff
    /// sliders); every catalogue param labels its control.
    label: Option<SharedString>,
    control: Control,
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
    label: SharedString,
    value: f32,
    min: f32,
    max: f32,
    help: SharedString,
    /// `help`, broken to the body width. Resolved by [`Scene::measure`].
    help_lines: Vec<SharedString>,
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
                            color: ladder::port(port_key(&port.port_type)),
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
        }
        self.measured = true;
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
                for param in params.iter_mut() {
                    if let Some(label) = &param.label {
                        width = width.max(
                            PAD_H * 2.
                                + run_width(label, PARAM_LABEL_SIZE, FontWeight::NORMAL, window),
                        );
                        height += PARAM_LABEL_LINE + PAD;
                    }
                    match &mut param.control {
                        Control::Field(_) => {
                            width = width.max(PAD_H * 2. + FIELD_WIDTH);
                            height += FIELD_HEIGHT;
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
                        }
                    }
                    height += PAD;
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
                let mut height = PAD_H * 2.
                    + wrap(blurb, BODY_TEXT, inner, window).len() as f32 * BODY_TEXT_LINE;
                for row in rows.iter_mut() {
                    row.help_lines = wrap(&row.help, HELP_TEXT, inner, window);
                    // `space-y-2` above each group, `space-y-1` within it.
                    height += PAD_H
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
            rows: vec![
                SliderRow {
                    label: "Width".into(),
                    value: number(values, "width", 1.),
                    min: 0.,
                    max: 4.,
                    help: "Higher = tighter pill; lower = wider falloff.".into(),
                    help_lines: Vec::new(),
                },
                SliderRow {
                    label: "Curve".into(),
                    value: number(values, "curve", 0.),
                    min: -1.,
                    max: 1.,
                    help: "Negative = softer edges, positive = snappier edges.".into(),
                    help_lines: Vec::new(),
                },
            ],
        },
        _ if params.is_empty() => Body::None,
        _ => Body::Params(
            params
                .iter()
                .map(|param| Param {
                    label: Some(param.name.clone().into()),
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

/// The synthetic `pattern_args` definition. Mirrors `pattern-args-node-def.ts`
/// exactly, including the rule that palettes and gradients both surface as
/// `Stops`. `None` when the pattern has no arguments, which leaves the card on
/// the "unknown type" path and draws a bare header.
fn pattern_args_def(args: &[PatternArgDef]) -> Option<NodeTypeDef> {
    if args.is_empty() {
        return None;
    }
    Some(NodeTypeDef {
        id: "pattern_args".to_string(),
        name: "Pattern Args".to_string(),
        description: None,
        category: Some("Input".to_string()),
        inputs: Vec::new(),
        outputs: args
            .iter()
            .map(|arg| PortDef {
                id: arg.id.clone(),
                name: arg.name.clone(),
                port_type: match arg.arg_type {
                    PatternArgType::Selection => PortType::Selection,
                    PatternArgType::Palette | PatternArgType::Gradient => PortType::Stops,
                    PatternArgType::Color | PatternArgType::Scalar => PortType::Signal,
                },
            })
            .collect(),
        params: Vec::new(),
    })
}

/// A node's stored position, or the same fallback grid the web editor lays out
/// when one is missing: five across, 200 × 150 apart.
fn placement(x: Option<f64>, y: Option<f64>, index: usize) -> Point<f32> {
    point(
        x.map(|x| x as f32).unwrap_or((index % 5) as f32 * 200.),
        y.map(|y| y as f32).unwrap_or((index / 5) as f32 * 150.),
    )
}

/// `PortType`'s wire spelling — the key `ladder::port` is a palette over.
/// Matched exhaustively so that a new port type is a compile error here rather
/// than a silently grey wire.
fn port_key(port_type: &PortType) -> &'static str {
    match port_type {
        PortType::Intensity => "Intensity",
        PortType::Audio => "Audio",
        PortType::BeatGrid => "BeatGrid",
        PortType::Series => "Series",
        PortType::Color => "Color",
        PortType::Selection => "Selection",
        PortType::Signal => "Signal",
        PortType::Events => "Events",
        PortType::Stops => "Stops",
    }
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
            (Some(message), _) => plate(message.clone(), ladder::danger().into()),
            (None, None) => plate(
                "Loading graph…".to_string(),
                ladder::muted_foreground().into(),
            ),
            (None, Some(_)) => canvas_element(state, app).into_any_element(),
        })
}

/// The way back, what is open, how big it is, and whether a write is in the
/// air. Nothing here is a control the canvas needs — the canvas is driven by
/// the pointer — so the strip stays a readout with one button on it.
fn toolbar(state: &Editor, app: &Entity<Luma>) -> Div {
    let back = app.clone();
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
            luma_ui::luma_button("Back", false)
                .id("back")
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.show_patterns(cx)))
                .agent_node(Role::Button, "Back"),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.pattern.name.clone())
                .agent_node(Role::Text, state.pattern.name.clone()),
        )
        .child(silkscreen(format!("{nodes} NODES")))
        .child(div().flex_1())
        // Named rather than merely drawn: "did the write land" is the question
        // this screen exists to answer, and inferring it from a node's
        // coordinates would be guessing.
        .when(state.saving || state.dirty, |el| {
            el.child(silkscreen("SAVING".to_string()))
        })
}

/// 9px uppercase silkscreen, the panel's one label style.
fn silkscreen(label: String) -> impl IntoElement {
    div()
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(ladder::muted_foreground())
        .child(label.clone())
        .agent_node(Role::Text, label)
}

fn plate(message: String, color: Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .text_color(color)
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
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
    let selected = state.selected.clone();
    let origin = Rc::clone(&state.origin);
    let fit = state.fit;
    let app = app.clone();
    let fitted = app.clone();

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
                        // The first framing waits on that measure: a fit is a
                        // function of boxes that do not exist until the text
                        // system has been asked about them.
                        if let (true, Some((at, extent))) = (fit, scene.extent()) {
                            measured_view.set(Viewport::fit(bounds.size, at, extent));
                            fitted.update(cx, |this, _| {
                                if let Screen::Graph(editor) = &mut this.screen {
                                    editor.fit = false;
                                }
                            });
                        }
                    }
                }
                // Registered here, alongside every laid-out control, so the
                // frame's node ids stay in tree order.
                let scene = registered.borrow();
                let view = measured_view.get();
                for card in &scene.cards {
                    let box_ = view.card_box(bounds.origin, card);
                    agent_paint_node(Role::Card, card.title.clone(), box_, window, cx);
                }
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, cx| {
                paint(
                    bounds,
                    &painted.borrow(),
                    painted_view.get(),
                    selected.as_ref(),
                    window,
                    cx,
                );
                listen(&app, &hitbox, window);
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
fn listen(app: &Entity<Luma>, hitbox: &Hitbox, window: &mut Window) {
    let pressed = app.clone();
    let inside = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble
            || event.button != MouseButton::Left
            || !inside.is_hovered(window)
        {
            return;
        }
        let at = event.position;
        pressed.update(cx, |this, cx| this.graph_press(at, cx));
    });

    let dragged = app.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            let at = event.position;
            dragged.update(cx, |this, cx| this.graph_drag(at, cx));
        }
    });

    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            released.update(cx, |this, cx| this.graph_release(cx));
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
        zoomed.update(cx, |this, cx| this.graph_zoom(at, wheel, cx));
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
    selected: Option<&SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(fill(bounds, ladder::trim()));

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
                selected.is_some_and(|id| id == &card.node_id),
                window,
                cx,
            );
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
        ladder::background(),
        Edges::all(px(CARD_BORDER * zoom)),
        if selected {
            ladder::primary()
        } else {
            ladder::gutter()
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
        ladder::trim(),
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
        ladder::legend_chip(),
        Edges::all(px(CHIP_BORDER * zoom)),
        ladder::legend_chip(),
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
        ladder::input(),
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
