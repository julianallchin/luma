//! The pattern graph editor: a custom-painted canvas over the authored graph
//! document.
//!
//! Mirrors `src/features/patterns/components/pattern-editor.tsx` and the
//! React Flow surface under `src/shared/lib/react-flow/` — the same node card
//! (a trim header over two port columns), the same fillet wire (a stub out of
//! each port, one diagonal, rounded corners), the same flat `bg-trim` ground,
//! and the same optimistic write through `save_pattern_graph_document`.
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
//! ([`Scene`]) is rebuilt when the graph changes, never per frame; the paint
//! culls to the viewport; and labels are dropped below the zoom where they
//! stop being legible.
//!
//! # What v1 is
//!
//! Open, read, look, move. Selection and drag-to-move persist; adding and
//! deleting nodes and edges, editing arguments, the graph agent and the
//! preview heatmap do not exist here yet. The seam already carries all of
//! them — this screen is what is missing, not the commands.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{agent_paint_node, Instrument, Role};

use luma_lib::models::node_graph::{Graph, NodeTypeDef, PortType};
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
    document: Option<Document>,
    /// Geometry derived from [`Document::graph`] and [`Self::types`], rebuilt
    /// on every change to either. Never derived during a draw — see the module
    /// docs.
    scene: Rc<Scene>,
    selected: Option<SharedString>,
    gesture: Option<Gesture>,
    view: Viewport,
    /// Where the canvas last painted, in window space. A mouse event arrives
    /// in window coordinates and has to be put back into graph coordinates,
    /// which needs this; the canvas knows it and the event handlers do not, so
    /// the canvas writes it down each frame. A `Cell` rather than a field
    /// assignment because the write happens inside `prepaint`, where updating
    /// the entity would mean notifying from inside a draw.
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
    /// Zoom bounds. The web editor's React Flow defaults are 0.5–2; the lower
    /// end is extended because a native canvas can afford to draw a whole
    /// graph at once and there is no DOM to thrash at the far end of it.
    const MIN_ZOOM: f32 = 0.2;
    const MAX_ZOOM: f32 = 2.5;

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

    fn card_box(&self, origin: Point<Pixels>, card: &Card) -> Bounds<Pixels> {
        Bounds {
            origin: self.to_window(origin, card.origin),
            size: size(px(CARD_WIDTH * self.zoom), px(card.height * self.zoom)),
        }
    }
}

impl Editor {
    /// The pattern being edited. The window title is the only reader outside
    /// this module.
    pub(crate) fn pattern_name(&self) -> &str {
        &self.pattern.name
    }

    /// Rebuild the resolved geometry from the document. Called on load and
    /// after every node move — a move changes a card's origin and both ends of
    /// every wire touching it, and one pass over the graph is cheaper to be
    /// right about than an incremental patch of the same facts.
    fn rebuild(&mut self) {
        let scene = match &self.document {
            Some(document) => Scene::build(&document.graph, &self.types),
            None => Scene::default(),
        };
        self.scene = Rc::new(scene);
    }

    /// The card under `at` (graph space), topmost first — cards are painted in
    /// order, so the last one that contains the point is the one on top.
    fn card_at(&self, at: Point<f32>) -> Option<&Card> {
        self.scene.cards.iter().rev().find(|card| {
            at.x >= card.origin.x
                && at.x <= card.origin.x + CARD_WIDTH
                && at.y >= card.origin.y
                && at.y <= card.origin.y + card.height
        })
    }

    /// Move one node to `origin` in graph space, in both the document and the
    /// geometry drawn from it.
    fn move_node(&mut self, node: &str, origin: Point<f32>) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(instance) = document.graph.nodes.iter_mut().find(|n| n.id == node) else {
            return;
        };
        instance.position_x = Some(origin.x as f64);
        instance.position_y = Some(origin.y as f64);
        self.rebuild();
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
            document: None,
            scene: Rc::new(Scene::default()),
            selected: None,
            gesture: None,
            view: Viewport {
                pan: point(px(CANVAS_MARGIN), px(CANVAS_MARGIN)),
                zoom: 1.,
            },
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
            let cursor = editor.view.to_graph(origin, at);
            match editor.card_at(cursor) {
                Some(card) => {
                    let node = card.node_id.clone();
                    let grab = point(cursor.x - card.origin.x, cursor.y - card.origin.y);
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
            match &mut editor.gesture {
                Some(Gesture::Pan { last }) => {
                    let delta = point(at.x - last.x, at.y - last.y);
                    *last = at;
                    editor.view.pan =
                        point(editor.view.pan.x + delta.x, editor.view.pan.y + delta.y);
                }
                Some(Gesture::Move {
                    node,
                    grab,
                    moved: dragged,
                }) => {
                    *dragged = true;
                    let cursor = editor.view.to_graph(origin, at);
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
            // Exponential in the scroll distance, so a fast flick and a slow
            // one over the same distance land in the same place.
            editor
                .view
                .zoom_about(origin, at, (wheel * ZOOM_PER_PIXEL).exp());
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
// One card shape, in graph space, derived from the node's definition. Sized in
// whole numbers rather than measured text: hit testing runs in a mouse handler
// where there is no text system to ask, and a card whose box depended on a
// measurement would be a different box there than the one it drew.

/// `min-w-[170px]` on the web card, taken here as the width outright.
const CARD_WIDTH: f32 = 170.;
/// The `bg-trim` title strip: `px-2 pt-1 pb-1` around a 12px line.
const HEADER_HEIGHT: f32 = 22.;
/// One port row: a 12px line with the web's `gap-1.5` between rows.
const PORT_ROW: f32 = 18.;
/// The body's `py-1`, top and bottom.
const BODY_PAD: f32 = 6.;
/// Distance from the card's edge to a port's centre. Everything a port draws
/// is centred on this one anchor, exactly as `PORT_ANCHOR` is on the web —
/// which is what makes a wire land in the dot rather than beside it.
const PORT_ANCHOR: f32 = 6.;
const PORT_RING: f32 = 9.;
const PORT_DOT: f32 = 4.;
/// The horizontal run a wire leaves a port on before it turns.
const WIRE_STUB: f32 = 16.;
/// The corner radius where that stub meets the diagonal.
const WIRE_FILLET: f32 = 10.;
const WIRE_WIDTH: f32 = 1.5;
/// Where the graph's origin sits when a pattern opens: far enough in that a
/// node at (0, 0) is not against the window edge.
const CANVAS_MARGIN: f32 = 40.;
/// Below this, a label is a smudge — so the paint drops the text and keeps the
/// shape. The cheapest kind of level of detail, and the one that matters:
/// shaping is the most expensive thing on this canvas.
const LABEL_FLOOR: f32 = 0.55;
/// Scroll-to-zoom rate, per logical pixel of wheel travel.
const ZOOM_PER_PIXEL: f32 = 0.004;
/// `text-xs` on the web card's title.
const TITLE_SIZE: f32 = 12.;
/// One notch down for the port names, which share a 170px card with a second
/// column of them. The web side sets both at `text-xs` and lets the card grow;
/// this canvas holds the card at one width instead, so the labels give way.
const PORT_LABEL_SIZE: f32 = 10.;
/// Leading, as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.3;

/// The graph with every position, colour and connection resolved: what the
/// canvas draws and what the pointer hits, in graph space.
#[derive(Default)]
struct Scene {
    cards: Vec<Card>,
    wires: Vec<Wire>,
}

struct Card {
    node_id: SharedString,
    title: SharedString,
    origin: Point<f32>,
    height: f32,
    inputs: Vec<Port>,
    outputs: Vec<Port>,
}

struct Port {
    id: SharedString,
    label: SharedString,
    color: Rgba,
    /// Centre of the port, as an offset from the card's origin.
    at: Point<f32>,
    /// Drawn as a dot inside its ring when something is wired to it, and as a
    /// bare ring when nothing is.
    connected: bool,
}

struct Wire {
    from: Point<f32>,
    to: Point<f32>,
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

        let cards: Vec<Card> = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let definition = types.get(&instance.type_id);
                let ports = |defs: &[luma_lib::models::node_graph::PortDef], output: bool| {
                    defs.iter()
                        .enumerate()
                        .map(|(row, port)| Port {
                            id: port.id.clone().into(),
                            label: port.name.clone().into(),
                            color: ladder::port(port_key(&port.port_type)),
                            at: point(
                                if output {
                                    CARD_WIDTH - PORT_ANCHOR
                                } else {
                                    PORT_ANCHOR
                                },
                                HEADER_HEIGHT + BODY_PAD + row as f32 * PORT_ROW + PORT_ROW / 2.,
                            ),
                            connected: wired.contains(&(
                                instance.id.as_str(),
                                port.id.as_str(),
                                output,
                            )),
                        })
                        .collect::<Vec<_>>()
                };
                let inputs = definition
                    .map(|d| ports(&d.inputs, false))
                    .unwrap_or_default();
                let outputs = definition
                    .map(|d| ports(&d.outputs, true))
                    .unwrap_or_default();
                let rows = inputs.len().max(outputs.len()) as f32;
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
                    height: HEADER_HEIGHT + BODY_PAD * 2. + rows * PORT_ROW,
                    inputs,
                    outputs,
                }
            })
            .collect();

        let by_id: HashMap<&str, &Card> = cards
            .iter()
            .map(|card| (card.node_id.as_ref(), card))
            .collect();
        let wires = graph
            .edges
            .iter()
            .filter_map(|edge| {
                let from = by_id.get(edge.from_node.as_str())?;
                let to = by_id.get(edge.to_node.as_str())?;
                let (start, color) = anchor(from, &from.outputs, &edge.from_port)?;
                let (end, _) = anchor(to, &to.inputs, &edge.to_port)?;
                // The wire carries the *source*'s hue, as on the web side: an
                // edge is one signal, and the port it came out of is what says
                // which kind.
                Some(Wire {
                    from: start,
                    to: end,
                    color,
                })
            })
            .collect();

        Self { cards, wires }
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

/// Where a wire meets a card, and the hue it takes from that port. An edge
/// naming a port the node does not have draws nothing — the document is ahead
/// of the catalogue, and half a wire would be a worse answer than none.
fn anchor(card: &Card, ports: &[Port], port_id: &str) -> Option<(Point<f32>, Rgba)> {
    let port = ports.iter().find(|port| port.id == port_id)?;
    Some((
        point(card.origin.x + port.at.x, card.origin.y + port.at.y),
        port.color,
    ))
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
        .child(silkscreen(format!("{} NODES", state.scene.cards.len())))
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
/// Everything the paint needs is captured by value — one refcounted [`Scene`],
/// one `Copy` viewport, one selected id — so a frame draws a consistent
/// picture without reaching back into the app to ask what it looks like. The
/// pointer handlers do the reverse: they carry no picture at all, only the
/// entity to send the gesture to, because by the time one runs, the frame it
/// was registered in is already gone.
fn canvas_element(state: &Editor, app: &Entity<Luma>) -> impl IntoElement {
    let registered = Rc::clone(&state.scene);
    let painted = Rc::clone(&state.scene);
    let view = state.view;
    let selected = state.selected.clone();
    let origin = Rc::clone(&state.origin);
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
                // Registered here, alongside every laid-out control, so the
                // frame's node ids stay in tree order.
                for card in &registered.cards {
                    let box_ = view.card_box(bounds.origin, card);
                    agent_paint_node(Role::Card, card.title.clone(), box_, window, cx);
                }
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, cx| {
                paint(bounds, &painted, view, selected.as_ref(), window, cx);
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
/// instead of ending in a gap short of the ring. The web side draws a faint
/// "ghost" stub over the card for the same reason; painting in order gets it
/// for free.
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
        for wire in &scene.wires {
            paint_wire(bounds.origin, wire, view, window);
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
                view,
                selected.is_some_and(|id| id == &card.node_id),
                window,
                cx,
            );
        }
    });
}

/// One node card: a header plate over two port columns, inside a border that
/// turns [`ladder::primary`] when selected — the one hue this screen spends on
/// a surface, and it spends it on meaning.
///
/// Square, where the web card is `rounded-lg`. CLAUDE.md's ladder says corners
/// are square everywhere; React Flow's node is the standing exception to that,
/// and copying an exception is not the same as keeping a contract.
fn paint_card(
    box_: Bounds<Pixels>,
    card: &Card,
    view: Viewport,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let (border, width) = if selected {
        (ladder::primary(), px(2.))
    } else {
        (ladder::gutter(), px(1.))
    };
    window.paint_quad(quad(
        box_,
        Corners::default(),
        ladder::background(),
        Edges::all(width),
        border,
        BorderStyle::Solid,
    ));
    window.paint_quad(fill(
        Bounds {
            origin: box_.origin,
            size: size(box_.size.width, px(HEADER_HEIGHT * view.zoom)),
        },
        ladder::trim(),
    ));

    // Below the floor a label is a smudge, and shaping is by far the most
    // expensive thing on this canvas. Dropping the text keeps the shape, which
    // is all that is legible at that size anyway.
    if view.zoom < LABEL_FLOOR {
        return;
    }
    // The card clips its own text, which is what `overflow-hidden` does on the
    // web card: a title too long for 170px is cut off at the edge.
    window.with_content_mask(Some(ContentMask { bounds: box_ }), |window| {
        paint_text(
            point(
                box_.origin.x + px(8. * view.zoom),
                box_.origin.y + px(4. * view.zoom),
            ),
            &card.title,
            TITLE_SIZE * view.zoom,
            FontWeight::MEDIUM,
            ladder::foreground(),
            window,
            cx,
        );
        for port in &card.inputs {
            paint_port(box_.origin, port, view.zoom, false, window, cx);
        }
        for port in &card.outputs {
            paint_port(box_.origin, port, view.zoom, true, window, cx);
        }
    });
}

/// A port: a ring on the card's edge, filled with a dot when something is
/// wired to it, and its name set inboard of that.
///
/// An output's name is right-aligned against its ring, so the line is shaped
/// before it is placed rather than drawn at a known point.
fn paint_port(
    card: Point<Pixels>,
    port: &Port,
    zoom: f32,
    output: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let centre = point(card.x + px(port.at.x * zoom), card.y + px(port.at.y * zoom));
    ring(
        centre,
        px(PORT_RING * zoom),
        port.color,
        port.connected,
        window,
    );

    let font_size = PORT_LABEL_SIZE * zoom;
    let inset = px((PORT_ANCHOR + PORT_RING) * zoom);
    let line = shape(
        &port.label,
        font_size,
        FontWeight::NORMAL,
        ladder::muted_foreground(),
        window,
    );
    let left = if output {
        centre.x - inset - line.width()
    } else {
        centre.x + inset
    };
    line.paint(
        point(left, centre.y - px(font_size * LINE_HEIGHT / 2.)),
        px(font_size * LINE_HEIGHT),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

/// A port mark: a ring, and a dot inside it when the port is wired. gpui has
/// no circle primitive, so both are fully rounded quads — which is exactly
/// what the web side draws them as (`rounded-full`).
fn ring(centre: Point<Pixels>, diameter: Pixels, color: Rgba, filled: bool, window: &mut Window) {
    window.paint_quad(quad(
        Bounds::centered_at(centre, size(diameter, diameter)),
        Corners::all(diameter / 2.),
        transparent_black(),
        Edges::all(px(1.5)),
        color,
        BorderStyle::Solid,
    ));
    if filled {
        let dot = diameter * (PORT_DOT / PORT_RING);
        window.paint_quad(quad(
            Bounds::centered_at(centre, size(dot, dot)),
            Corners::all(dot / 2.),
            color,
            Edges::default(),
            transparent_black(),
            BorderStyle::Solid,
        ));
    }
}

/// The wire shape, once: a horizontal stub out of each port, one diagonal
/// between them, and a fixed-radius arc at each corner. Mirrors
/// `buildFilletPath` in `src/shared/lib/react-flow/fillet-edge.tsx` — same
/// stub, same radius, same clamp to half of each adjoining segment — so a
/// graph reads the same in both hosts.
fn paint_wire(origin: Point<Pixels>, wire: &Wire, view: Viewport, window: &mut Window) {
    let corners = [
        wire.from,
        point(wire.from.x + WIRE_STUB, wire.from.y),
        point(wire.to.x - WIRE_STUB, wire.to.y),
        wire.to,
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
        window.paint_path(path, wire.color);
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

/// Draw one line of text with its top-left at `at`.
fn paint_text(
    at: Point<Pixels>,
    text: &SharedString,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    shape(text, font_size, weight, color, window)
        .paint(
            at,
            px(font_size * LINE_HEIGHT),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
}

/// Shape one line in the app's face. The text system caches layouts, so
/// shaping a label that has not changed size is a lookup — which is what makes
/// it affordable to shape before deciding where to put the result.
fn shape(
    text: &SharedString,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    window: &Window,
) -> ShapedLine {
    let mut font = gpui::font(luma_ui::fonts::FAMILY);
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.clone(), px(font_size), &[run], None)
}
