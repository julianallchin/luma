//! The clip graph as a node-and-wire canvas: columns of node cards, sources
//! on the left and the output on the right, with a wire from each node's
//! output port into every input it feeds.
//!
//! The layout is automatic and the same graph always gives the same picture
//! ([`edit::columns`]); the stored graph has no positions. Cards are real
//! elements, so their controls are the sheet's own widgets; the wires are
//! painted under them from the port centres the cards record while they
//! prepaint, in the same frame.
//!
//! Zoom draws the cards under a scaled rem ([`luma_ui::rem_scaled`]): every
//! card length is [`rpx`], so the whole card, text and controls included,
//! scales, and the wires follow the ports. Each wire and both its ports take
//! the colour of what it carries ([`ladder::signal`]).
//!
//! Pointer conventions, as node editors have them: a mouse wheel zooms about
//! the pointer, as does a pinch or a scroll with Ctrl or Cmd; a trackpad
//! scroll pans, and so does dragging the ground with the left or middle
//! button. The keyboard, once the canvas has focus: `+` and `-` zoom, `0` is
//! actual size, Delete removes the selected node, Escape drops a wire drag.
//!
//! The pan, the wire drag and the pointer handling come from the graph editor
//! this app had before clip graphs (`graph.rs`, removed in 6369a3b5): the
//! wire's stub-and-fillet path, the press/move/release split with move and
//! release on the window, zoom about an anchor, and the drop onto the
//! nearest port.

use luma_ui::ladder::Signal;
use luma_ui::{glass, rpx};

use super::*;

/// A node card's width, and a curve's when its strip is widened.
pub(super) const NODE_W: f32 = 264.;
const NODE_W_WIDE: f32 = 440.;
/// The card's inner padding.
pub(super) const NODE_PAD: f32 = 12.;
/// A value field's width inside a card.
pub(super) const NODE_FIELD_W: f32 = NODE_W - 2. * NODE_PAD;
/// Air between columns, which the wires cross, and between cards in one.
const COLUMN_GAP: f32 = 64.;
const NODE_GAP: f32 = 16.;
/// Air between the canvas edge and the cards at rest; the top leaves the
/// toolbar its own band. Screen pixels: the anchor does not zoom.
const CANVAS_PAD: f32 = 20.;
const CANVAS_TOP: f32 = 52.;
/// A port's ring.
const PORT: f32 = 10.;
/// How near a dragged wire's end snaps to a port that takes it, and how near
/// the pointer must be to a wire to hover it, in screen pixels.
const SNAP: f32 = 28.;
const WIRE_HOVER: f32 = 6.;
/// The horizontal run a wire leaves a port on before it turns, and the
/// radius of that turn, at zoom 1.
const WIRE_STUB: f32 = 16.;
const WIRE_FILLET: f32 = 10.;
const WIRE_WIDTH: f32 = 1.5;
/// The zoom range, and one keyboard or button step.
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 1.5;
const ZOOM_STEP: f32 = 1.2;
/// Zoom per pixel of wheel travel, and per line of a mouse wheel.
const ZOOM_PER_PIXEL: f32 = 0.004;
const PIXELS_PER_LINE: f32 = 20.;

/// Where a wire comes from: a node in the graph, or a new node not yet
/// wired, which lives only on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::track_editor) enum Source {
    Node(String),
    Draft(usize),
}

/// A node added from the canvas menu. The graph may hold only nodes the
/// output reaches, so it waits here until its output is wired.
#[derive(Clone, Debug)]
pub(in crate::track_editor) struct Draft {
    pub kind: Kind,
    /// Its top-left from the cards' anchor (the top-right corner at rest),
    /// at zoom 1.
    pub at: Point<f32>,
}

pub(in crate::track_editor) enum Gesture {
    /// Moving the view; `last` is the previous pointer position.
    Pan { last: Point<Pixels> },
    /// Dragging a wire out of `from`. `detach` is the input it was picked up
    /// from, which loses it on the drop. `snap` is the port that takes it
    /// nearest the pointer, where the wire's end sits.
    Wire {
        from: Source,
        detach: Option<(String, String)>,
        at: Point<Pixels>,
        snap: Option<(String, String)>,
    },
}

/// An input, by node and input name.
type Port = (String, String);

/// Where the ports were drawn this frame, in window space.
#[derive(Default)]
pub(in crate::track_editor) struct Geometry {
    canvas: Bounds<Pixels>,
    /// The cards' box, at the zoom they were drawn at.
    content: Bounds<Pixels>,
    outputs: Vec<(Source, Point<Pixels>)>,
    inputs: Vec<(Port, Point<Pixels>)>,
}

impl Geometry {
    fn output(&self, from: &Source) -> Option<Point<Pixels>> {
        self.outputs
            .iter()
            .find(|(source, _)| source == from)
            .map(|(_, at)| *at)
    }

    fn input(&self, to: &Port) -> Option<Point<Pixels>> {
        self.inputs
            .iter()
            .find(|(key, _)| key == to)
            .map(|(_, at)| *at)
    }
}

/// The canvas's own state, owned by the sheet. Reset when the subject
/// changes.
pub(in crate::track_editor) struct State {
    /// How far the view moved from rest, where the output sits at the top
    /// right, in screen pixels.
    pub pan: Point<Pixels>,
    pub zoom: f32,
    pub selected: Option<String>,
    pub gesture: Option<Gesture>,
    /// The wire under the pointer, by the input it feeds.
    pub hover: Option<Port>,
    pub drafts: Vec<Draft>,
    /// Curves whose strip is widened.
    pub wide: BTreeSet<String>,
    /// The add menu, at this window point.
    pub menu: Option<Point<Pixels>>,
    /// A card's menu: its node, at this window point.
    pub card_menu: Option<(String, Point<Pixels>)>,
    /// The node whose name is being typed.
    pub rename: Option<Rename>,
    /// Why the last rename was refused, by the node it was for.
    pub rename_error: Option<(String, String)>,
    pub geometry: Rc<RefCell<Geometry>>,
    /// Keyboard focus for the canvas's keys, made on the first sync.
    pub focus: Option<FocusHandle>,
}

/// A node's name being typed on its card.
pub(in crate::track_editor) struct Rename {
    pub id: String,
    pub field: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            pan: Point::default(),
            zoom: 1.,
            selected: None,
            gesture: None,
            hover: None,
            drafts: Vec::new(),
            wide: BTreeSet::new(),
            menu: None,
            card_menu: None,
            rename: None,
            rename_error: None,
            geometry: Rc::default(),
            focus: None,
        }
    }
}

impl State {
    fn dragging(&self) -> Option<(&Source, Option<&Port>)> {
        match &self.gesture {
            Some(Gesture::Wire { from, detach, .. }) => Some((from, detach.as_ref())),
            _ => None,
        }
    }

    /// Drop a wire being dragged, as if it never started.
    pub(in crate::track_editor) fn cancel(&mut self) -> bool {
        matches!(self.gesture.take(), Some(Gesture::Wire { .. }))
    }

    /// Where the cards' anchor is: the top-right corner of their box.
    fn anchor(&self) -> Point<Pixels> {
        let canvas = self.geometry.borrow().canvas;
        point(
            canvas.origin.x + canvas.size.width - px(CANVAS_PAD) + self.pan.x,
            canvas.origin.y + px(CANVAS_TOP) + self.pan.y,
        )
    }

    /// Zoom by `factor`, keeping what is under `about` where it is.
    fn zoom_about(&mut self, about: Point<Pixels>, factor: f32) {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let ratio = zoom / self.zoom;
        let anchor = self.anchor();
        self.pan = self.pan + (about - anchor) * (1. - ratio);
        self.zoom = zoom;
    }

    fn centre(&self) -> Point<Pixels> {
        self.geometry.borrow().canvas.center()
    }

    /// Frame the whole graph: the largest zoom in range it fits at, the
    /// output kept in view when it cannot fit.
    fn fit(&mut self) {
        let (canvas, content) = {
            let geometry = self.geometry.borrow();
            (geometry.canvas, geometry.content)
        };
        let width = f32::from(content.size.width) / self.zoom;
        let height = f32::from(content.size.height) / self.zoom;
        if width <= 0. || height <= 0. {
            return;
        }
        let room_w = f32::from(canvas.size.width) - 2. * CANVAS_PAD;
        let room_h = f32::from(canvas.size.height) - CANVAS_TOP - CANVAS_PAD;
        self.zoom = (room_w / width).min(room_h / height).clamp(MIN_ZOOM, 1.);
        let spare = room_w - width * self.zoom;
        self.pan = point(px(-(spare / 2.).max(0.)), px(0.));
    }
}

/// What an input takes, as a signal: the colour of its port and of the wire
/// into it.
fn input_signal(graph: &ClipGraph, id: &str, input: &str) -> Option<Signal> {
    Some(match edit::spec(graph, id, input)?.ty {
        Ty::Number => Signal::Number,
        Ty::Vector => Signal::Vector,
        Ty::Color => Signal::Color,
        Ty::Clock => Signal::Clock,
        Ty::Heads => Signal::Heads,
        Ty::Coordinate => Signal::Coordinate,
        Ty::Points | Ty::Gradient => return None,
    })
}

/// What a node of `kind` gives. A curve gives what its kind setting says.
fn output_signal(kind: Kind, curve: Option<&str>) -> Signal {
    match kind {
        Kind::Clock => Signal::Clock,
        Kind::Time | Kind::Space | Kind::Noise | Kind::Audio => Signal::Coordinate,
        Kind::Curve => match curve {
            Some("vector") => Signal::Vector,
            Some("color") => Signal::Color,
            _ => Signal::Number,
        },
        _ => Signal::Heads,
    }
}

fn source_signal(graph: &ClipGraph, drafts: &[Draft], from: &Source) -> Signal {
    match from {
        Source::Node(id) => graph.nodes.get(id).map_or(Signal::Number, |node| {
            output_signal(node.kind, node.setting("kind"))
        }),
        Source::Draft(index) => output_signal(
            drafts.get(*index).map_or(Kind::Curve, |draft| draft.kind),
            None,
        ),
    }
}

/// A color curve's gradient ends, as display colours, for its wire.
fn gradient_ends(graph: &ClipGraph, id: &str) -> Option<(Hsla, Hsla)> {
    let Some(Input::Gradient(gradient)) = graph.nodes.get(id)?.inputs.get("gradient") else {
        return None;
    };
    let display = |stop: &p::ColorStop| -> Hsla { Light::opaque(stop.color).display().into() };
    Some((
        display(gradient.stops.first()?),
        display(gradient.stops.last()?),
    ))
}

/// The kinds the add menu offers: every kind but the outputs, since a graph
/// has exactly one.
const ADDABLE: [Kind; 10] = [
    Kind::Clock,
    Kind::Time,
    Kind::Space,
    Kind::Noise,
    Kind::Audio,
    Kind::Curve,
    Kind::Mirror,
    Kind::Shuffle,
    Kind::Group,
    Kind::Split,
];

/// Whether a wire from `from` may drop on an input.
fn fits(graph: &ClipGraph, drafts: &[Draft], from: &Source, id: &str, input: &str) -> bool {
    match from {
        Source::Node(from) => edit::link_candidates(graph, id, input)
            .iter()
            .any(|c| c == from),
        Source::Draft(index) => drafts
            .get(*index)
            .is_some_and(|draft| edit::accepts(graph, id, input, draft.kind)),
    }
}

// -- ports ----------------------------------------------------------------------

/// How a port draws: its signal's colour, filled while wired, brighter and
/// larger while lit (the hovered wire's ends, a drag's source and the port
/// it snaps to), faint while a drag is on that it cannot take.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Rest,
    Lit,
    Faint,
}

/// A port's ring. It records its centre as it prepaints, which is where the
/// wires meet it and what a drop is measured against.
fn ring(signal: Signal, filled: bool, tone: Tone, record: impl Fn(Point<Pixels>) + 'static) -> Div {
    let mut edge = ladder::signal(signal);
    match tone {
        Tone::Faint => edge.a = 0.25,
        Tone::Lit => edge.l = (edge.l + 0.12).min(0.95),
        Tone::Rest => {}
    }
    let size = if tone == Tone::Lit { PORT + 2. } else { PORT };
    div()
        .size(rpx(size))
        .flex_none()
        .rounded_full()
        .border(rpx(1.5))
        .border_color(edge)
        .bg(if filled || tone == Tone::Lit {
            edge
        } else {
            ladder::card().into()
        })
        .child(canvas(move |bounds, _, _| record(bounds.center()), |_, _, _, _| {}).size_full())
}

/// An input's port, on the card's left edge. A press on a wired one picks
/// its wire up.
pub(super) fn input_port(cx: &Ctx, id: &str, input: &str) -> Option<AnyElement> {
    let signal = input_signal(cx.graph, id, input)?;
    let canvas = &cx.state.sheet.canvas;
    let key: Port = (id.to_owned(), input.to_owned());
    let held = cx.graph.nodes[id].inputs.get(input);
    let wired = held.is_some_and(|held| held.sources().next().is_some());
    // A single wire can be picked up; a list's wires come out one by one
    // from the card.
    let source = held.and_then(Input::source).map(str::to_owned);
    let detached = canvas
        .dragging()
        .is_some_and(|(_, detach)| detach == Some(&key));
    let tone = match (&canvas.gesture, canvas.dragging()) {
        (Some(Gesture::Wire { snap, .. }), _) if snap.as_ref() == Some(&key) => Tone::Lit,
        (_, Some((from, _))) if fits(cx.graph, &canvas.drafts, from, id, input) => Tone::Rest,
        (_, Some(_)) => Tone::Faint,
        _ if canvas.hover.as_ref() == Some(&key) => Tone::Lit,
        _ => Tone::Rest,
    };
    let geometry = canvas.geometry.clone();
    let app = cx.app.clone();
    let label = format!(
        "{} {} port",
        edit::label(id),
        input_label(input).to_lowercase()
    );
    let recorded = key.clone();
    Some(
        ring(signal, wired && !detached, tone, move |at| {
            geometry.borrow_mut().inputs.push((recorded.clone(), at));
        })
        .id(SharedString::from(format!("port-{id}-{input}")))
        .occlude()
        .when_some(source, |port, source| {
            port.cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                    cx.stop_propagation();
                    let from = Source::Node(source.clone());
                    let at = event.position;
                    let detach = Some(key.clone());
                    app.update(cx, |this, cx| this.canvas_wire_press(from, detach, at, cx));
                })
        })
        .agent_node(Role::Button, label)
        .into_any_element(),
    )
}

/// A node's output port, on the card's right edge. A press drags a new wire
/// out of it.
fn output_port(cx: &Ctx, from: Source, label: String) -> AnyElement {
    let canvas = &cx.state.sheet.canvas;
    let wired = match &from {
        Source::Node(id) => !edit::references(cx.graph, id).is_empty(),
        Source::Draft(_) => false,
    };
    let hovered = canvas.hover.as_ref().is_some_and(|(id, input)| {
        matches!(&from, Source::Node(node)
            if cx.graph.nodes[id].inputs.get(input)
                .is_some_and(|held| held.sources().any(|source| source == node)))
    });
    let tone = match canvas.dragging() {
        Some((dragged, _)) if *dragged == from => Tone::Lit,
        Some(_) => Tone::Faint,
        None if hovered => Tone::Lit,
        None => Tone::Rest,
    };
    let signal = source_signal(cx.graph, &canvas.drafts, &from);
    let geometry = canvas.geometry.clone();
    let recorded = from.clone();
    let app = cx.app.clone();
    let key = match &from {
        Source::Node(id) => format!("out-{id}"),
        Source::Draft(index) => format!("out-draft-{index}"),
    };
    ring(signal, wired, tone, move |at| {
        geometry.borrow_mut().outputs.push((recorded.clone(), at));
    })
    .id(SharedString::from(key))
    .occlude()
    .cursor_pointer()
    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
        cx.stop_propagation();
        let (from, at) = (from.clone(), event.position);
        app.update(cx, |this, cx| this.canvas_wire_press(from, None, at, cx));
    })
    .agent_node(Role::Button, format!("{label} output port"))
    .into_any_element()
}

// -- cards ------------------------------------------------------------------------

/// A card's plate: fixed width, rounded, the selected one ringed. It takes
/// presses, so the canvas behind it does not pan under them, but lets the
/// wheel through.
fn plate(width: f32, selected: bool) -> Div {
    div()
        .relative()
        .w(rpx(width))
        .flex_none()
        .flex()
        .flex_col()
        .gap(rpx(ROW_GAP))
        .p(rpx(NODE_PAD))
        .rounded(rpx(luma_ui::radius::CARD))
        .bg(ladder::card())
        .border_1()
        .border_color(if selected {
            ladder::primary().into()
        } else {
            glass::hairline(0.1)
        })
        .text_size(rpx(12.))
        .block_mouse_except_scroll()
}

/// A card's title text, which takes the row up to the buttons.
fn title_text(label: &str) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_size(rpx(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ladder::foreground())
        .child(label.to_owned())
}

/// A card's title row: its name, then its buttons, and on a node that feeds
/// something its output port past the right edge.
fn title(name: AnyElement, buttons: Vec<AnyElement>, port: Option<AnyElement>) -> Div {
    div()
        .relative()
        .w_full()
        .h(rpx(CONTROL_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(rpx(4.))
        .child(name)
        .children(buttons)
        .children(port.map(|port| {
            div()
                .absolute()
                .right(rpx(-NODE_PAD - PORT / 2. - 1.))
                .top(rpx((CONTROL_HEIGHT - PORT) / 2.))
                .child(port)
        }))
}

/// Where an input's port sits in its row's header line: on the card's left
/// edge.
pub(super) fn port_slot(port: AnyElement) -> Div {
    div()
        .absolute()
        .left(rpx(-NODE_PAD - PORT / 2. - 1.))
        .top(rpx((CONTROL_HEIGHT - PORT) / 2.))
        .child(port)
}

/// One node's card: title, settings, one row per input, and a noise node's
/// preview.
fn node_card(cx: &Ctx, id: &str) -> AnyElement {
    let node = &cx.graph.nodes[id];
    let canvas = &cx.state.sheet.canvas;
    let label = edit::label(id);
    let wide = canvas.wide.contains(id);
    let mut buttons = Vec::new();
    if node.kind == Kind::Curve {
        let app = cx.app.clone();
        let at = id.to_owned();
        let name = if wide { "Narrow" } else { "Widen" };
        buttons.push(
            icon_button(
                if wide {
                    IconName::Minimize
                } else {
                    IconName::Expand
                },
                Enabled::Yes,
            )
            .id(SharedString::from(format!("widen-{id}")))
            .on_click(move |_, _, cx| {
                let at = at.clone();
                app.update(cx, |this, cx| {
                    this.with_track_editor(cx, |editor| {
                        let wide = &mut editor.sheet.canvas.wide;
                        if !wide.remove(&at) {
                            wide.insert(at);
                        }
                    })
                });
            })
            .agent_node(Role::Button, format!("{name} {label}"))
            .into_any_element(),
        );
    }
    let port = (!node.kind.is_output()).then(|| {
        let app = cx.app.clone();
        let at = id.to_owned();
        buttons.push(
            icon_button(IconName::Close, Enabled::Yes)
                .id(SharedString::from(format!("delete-{id}")))
                .on_click(move |_, _, cx| {
                    let at = at.clone();
                    app.update(cx, |this, cx| this.graph_delete_node(&at, cx));
                })
                .agent_node(Role::Button, format!("Delete {label}"))
                .into_any_element(),
        );
        output_port(cx, Source::Node(id.to_owned()), label.clone())
    });
    let rows: Vec<AnyElement> = definition(node.kind)
        .inputs
        .iter()
        .filter(|(input, _)| visible(node, input))
        .map(|(input, _)| row(cx, id, input))
        .collect();
    let selected = canvas.selected.as_deref() == Some(id);
    let (select, grab, menu) = (cx.app.clone(), cx.app.clone(), cx.app.clone());
    let (at, grabbed, menu_at) = (id.to_owned(), id.to_owned(), id.to_owned());
    let refused = canvas
        .rename_error
        .as_ref()
        .filter(|(at, _)| at == id)
        .map(|(_, error)| {
            div()
                .text_size(rpx(12.))
                .text_color(ladder::danger())
                .child(error.clone())
                .agent_node(Role::Text, error.clone())
        });
    plate(if wide { NODE_W_WIDE } else { NODE_W }, selected)
        .id(SharedString::from(format!("node-{id}")))
        // A press anywhere on the card selects it; only its title takes the
        // keyboard, so a press into a field keeps the field's focus.
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            let at = at.clone();
            select.update(cx, |this, cx| this.canvas_select(Some(at), false, cx));
        })
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            cx.stop_propagation();
            let place = (menu_at.clone(), event.position);
            menu.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.canvas.card_menu = Some(place);
                })
            });
        })
        .child(title(name(cx, id, &label), buttons, port).on_mouse_down(
            MouseButton::Left,
            move |_, _, cx| {
                let at = grabbed.clone();
                grab.update(cx, |this, cx| this.canvas_select(Some(at), true, cx));
            },
        ))
        .children(refused)
        .children(settings(cx, id, node))
        .children(rows)
        .children(cx.controls.noise.get(id).cloned())
        .agent_node(Role::Card, label)
        .into_any_element()
}

/// A card's name: its title, which a double-click opens for typing, or the
/// field while it is being typed. Presses in the field stay there, so the
/// card does not take the keyboard from it.
fn name(cx: &Ctx, id: &str, label: &str) -> AnyElement {
    let rename = cx.state.sheet.canvas.rename.as_ref();
    if let Some(rename) = rename.filter(|rename| rename.id == id) {
        return luma_ui::float::field()
            .id(SharedString::from(format!("rename-{id}")))
            .flex_1()
            .min_w_0()
            .key_context(text_input::DRAFT_CONTEXT)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().w_full().child(rename.field.clone()))
            .agent_node(Role::Input, format!("Rename {label}"))
            .into_any_element();
    }
    let app = cx.app.clone();
    let at = id.to_owned();
    title_text(label)
        .id(SharedString::from(format!("title-{id}")))
        .on_click(move |event, window, cx| {
            if event.click_count() != 2 {
                return;
            }
            cx.stop_propagation();
            let at = at.clone();
            app.update(cx, |this, cx| this.graph_rename_start(&at, window, cx));
        })
        .agent_node(Role::Text, label.to_owned())
        .into_any_element()
}

/// A node added from the menu and not yet wired: its kind, a note on what to
/// do with it, and its output port.
fn draft_card(cx: &Ctx, index: usize, draft: &Draft) -> AnyElement {
    let label = format!("New {}", draft.kind.label().to_lowercase());
    let app = cx.app.clone();
    let discard = icon_button(IconName::Close, Enabled::Yes)
        .id(SharedString::from(format!("discard-draft-{index}")))
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    let drafts = &mut editor.sheet.canvas.drafts;
                    if index < drafts.len() {
                        drafts.remove(index);
                    }
                })
            });
        })
        .agent_node(Role::Button, format!("Discard {label}"))
        .into_any_element();
    let port = output_port(cx, Source::Draft(index), label.clone());
    div()
        .absolute()
        .left(rpx(draft.at.x))
        .top(rpx(draft.at.y))
        .child(
            plate(NODE_W, false)
                .child(title(
                    title_text(&label).into_any_element(),
                    vec![discard],
                    Some(port),
                ))
                .child(luma_ui::caption(
                    "Drag its output onto an input".to_string(),
                )),
        )
        .agent_node(Role::Card, label)
        .into_any_element()
}

// -- the canvas -----------------------------------------------------------------

/// The canvas under the name, selection and blend rows. `wide` is whether
/// the panel is widened, or `None` where it already fills its box.
pub(super) fn view(cx: &Ctx, wide: Option<bool>) -> AnyElement {
    let state = &cx.state.sheet.canvas;
    let columns = edit::columns(cx.graph);
    let geometry = state.geometry.clone();
    // Right to left in `columns`, so the sources come first on screen.
    let content = columns.iter().rev().fold(
        div()
            .absolute()
            .top_0()
            .right_0()
            .flex()
            .flex_row()
            .items_start()
            .gap(rpx(COLUMN_GAP))
            .child(
                canvas(
                    move |bounds, _, _| geometry.borrow_mut().content = bounds,
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            ),
        |row, column| {
            row.child(
                column
                    .iter()
                    .fold(div().flex().flex_col().gap(rpx(NODE_GAP)), |col, id| {
                        col.child(node_card(cx, id))
                    }),
            )
        },
    );
    let drafts: Vec<AnyElement> = state
        .drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| draft_card(cx, index, draft))
        .collect();
    // A zero-size anchor at the cards' top-right corner: the cards hang left
    // and down from it, and the drafts sit at their place from it.
    let world = div()
        .absolute()
        .top(px(CANVAS_TOP) + state.pan.y)
        .right(px(CANVAS_PAD) - state.pan.x)
        .size_0()
        .child(content)
        .children(drafts);
    let mut root = div()
        .id("clip-graph-canvas")
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_hidden()
        .bg(ladder::background())
        .child(wires(cx))
        .child(luma_ui::rem_scaled(
            px(luma_ui::BASE_REM * state.zoom),
            world,
        ))
        .child(toolbar(cx, wide))
        .children(add_menu(cx))
        .children(card_menu(cx));
    if let Some(focus) = &state.focus {
        let app = cx.app.clone();
        let own = focus.clone();
        root = root
            .key_context(crate::keymap::context::CLIP_GRAPH)
            .track_focus(focus)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                // Keys typed into a field on a card are the field's.
                if !own.is_focused(window) {
                    return;
                }
                let key = event.keystroke.key.clone();
                let handled = app.update(cx, |this, cx| this.canvas_key(&key, cx));
                if handled {
                    cx.stop_propagation();
                }
            });
    }
    root.agent_node(Role::Card, "Graph canvas")
        .into_any_element()
}

/// The buttons over the canvas's top-right corner: add a node, the zoom
/// steps and actual size, fit, and widening the panel.
fn toolbar(cx: &Ctx, wide: Option<bool>) -> Div {
    let state = &cx.state.sheet.canvas;
    let add = cx.app.clone();
    let mut bar = div()
        .absolute()
        .top(px(8.))
        .right(px(8.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .child(
            luma_ui::button("Add node", Enabled::Yes)
                .id("graph-add-node")
                .on_click(move |event, _, cx| {
                    let at = event.position();
                    add.update(cx, |this, cx| {
                        this.with_track_editor(cx, |editor| editor.sheet.canvas.menu = Some(at))
                    });
                })
                .agent_node(Role::Button, "Add node"),
        );
    let percent = format!("{:.0}%", state.zoom * 100.);
    bar = bar.child(
        div()
            .size_0()
            .agent_node(Role::Text, format!("Zoom {percent}")),
    );
    for (icon, text, name, key) in [
        (Some(IconName::Minus), "", "Zoom out", "-"),
        (None, percent.as_str(), "Actual size", "0"),
        (Some(IconName::Plus), "", "Zoom in", "+"),
        (None, "Fit", "Fit graph", "fit"),
    ] {
        let app = cx.app.clone();
        let control = match icon {
            Some(icon) => icon_button(icon, Enabled::Yes),
            None => luma_ui::button(text, Enabled::Yes),
        };
        bar = bar.child(
            control
                .id(SharedString::from(format!("graph-{key}")))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.canvas_key(key, cx);
                    });
                })
                .agent_node(Role::Button, name),
        );
    }
    if let Some(wide) = wide {
        let app = cx.app.clone();
        let name = if wide {
            "Narrow graph panel"
        } else {
            "Widen graph panel"
        };
        bar = bar.child(
            icon_button(
                if wide {
                    IconName::Minimize
                } else {
                    IconName::Expand
                },
                Enabled::Yes,
            )
            .id("graph-widen")
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    this.inspector_wide = !this.inspector_wide;
                    cx.notify();
                })
            })
            .agent_node(Role::Button, name),
        );
    }
    bar
}

/// The add menu, where the canvas was right-clicked or under the button.
fn add_menu(cx: &Ctx) -> Option<AnyElement> {
    let at = cx.state.sheet.canvas.menu?;
    let menu = ADDABLE.iter().fold(
        luma_ui::menu::ContextMenu::new("graph-add-menu", at),
        |menu, kind| {
            let app = cx.app.clone();
            let kind = *kind;
            menu.item(kind.label(), move |_, cx| {
                app.update(cx, |this, cx| this.canvas_add(kind, at, cx));
            })
        },
    );
    let app = cx.app.clone();
    Some(menu.render(move |_, cx| {
        app.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| editor.sheet.canvas.menu = None)
        });
    }))
}

/// A card's menu, where the card was right-clicked: rename the node, and
/// delete it unless it is the output.
fn card_menu(cx: &Ctx) -> Option<AnyElement> {
    let (id, at) = cx.state.sheet.canvas.card_menu.clone()?;
    let node = cx.graph.nodes.get(&id)?;
    let close = |this: &mut Luma, cx: &mut Context<Luma>| {
        this.with_track_editor(cx, |editor| editor.sheet.canvas.card_menu = None)
    };
    let (app, at_id) = (cx.app.clone(), id.clone());
    let mut menu =
        luma_ui::menu::ContextMenu::new("graph-card-menu", at).item("Rename", move |window, cx| {
            let at = at_id.clone();
            app.update(cx, |this, cx| {
                close(this, cx);
                this.graph_rename_start(&at, window, cx);
            });
        });
    if !node.kind.is_output() {
        let app = cx.app.clone();
        menu = menu.destructive("Delete", move |_, cx| {
            let at = id.clone();
            app.update(cx, |this, cx| {
                close(this, cx);
                this.graph_delete_node(&at, cx);
            });
        });
    }
    let app = cx.app.clone();
    Some(menu.render(move |_, cx| app.update(cx, |this, cx| close(this, cx))))
}

/// One wire as painted: its ends' ports and its colour, or its colour ends.
struct Link {
    from: Source,
    to: Port,
    color: Hsla,
    gradient: Option<(Hsla, Hsla)>,
}

/// The wires, under the cards, from the port centres the cards recorded
/// this frame; the wire being dragged; and the canvas's pointer handling.
fn wires(cx: &Ctx) -> AnyElement {
    let state = &cx.state.sheet.canvas;
    let geometry = state.geometry.clone();
    let painted = state.geometry.clone();
    let detach = state.dragging().and_then(|(_, detach)| detach.cloned());
    let graph = cx.graph;
    let links: Vec<Link> = graph
        .nodes
        .iter()
        .flat_map(|(id, node)| {
            node.wires().map(move |(input, from)| {
                let signal = input_signal(graph, id, input).unwrap_or(Signal::Number);
                Link {
                    from: Source::Node(from.to_owned()),
                    to: (id.clone(), input.to_owned()),
                    color: ladder::signal(signal),
                    gradient: (signal == Signal::Color)
                        .then(|| gradient_ends(graph, from))
                        .flatten(),
                }
            })
        })
        .filter(|link| detach.as_ref() != Some(&link.to))
        .collect();
    let dragged = match &state.gesture {
        Some(Gesture::Wire { from, at, snap, .. }) => Some((
            from.clone(),
            *at,
            snap.clone(),
            ladder::signal(source_signal(graph, &state.drafts, from)),
        )),
        _ => None,
    };
    let hover = state.hover.clone();
    let zoom = state.zoom;
    let app = cx.app.clone();
    canvas(
        move |bounds, window, _| {
            let mut geometry = geometry.borrow_mut();
            *geometry = Geometry {
                canvas: bounds,
                ..Geometry::default()
            };
            window.insert_hitbox(bounds, HitboxBehavior::Normal)
        },
        move |bounds, hitbox, window, cx| {
            let geometry = painted.borrow();
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for link in &links {
                    let (Some(start), Some(end)) =
                        (geometry.output(&link.from), geometry.input(&link.to))
                    else {
                        continue;
                    };
                    let lit = hover.as_ref() == Some(&link.to);
                    let width = if lit { WIRE_WIDTH + 1.5 } else { WIRE_WIDTH };
                    let path = wire_path(start, end, width * zoom, zoom);
                    let paint: Background = match link.gradient {
                        Some((a, b)) => {
                            linear_gradient(90., linear_color_stop(a, 0.), linear_color_stop(b, 1.))
                        }
                        None if lit => link.color.into(),
                        None => {
                            let mut color = link.color;
                            color.a = 0.85;
                            color.into()
                        }
                    };
                    if let Some(path) = path {
                        window.paint_path(path, paint);
                    }
                    let Source::Node(from) = &link.from else {
                        continue;
                    };
                    agent_paint_node(
                        Role::Text,
                        format!(
                            "{} → {} {}",
                            edit::label(from),
                            edit::label(&link.to.0),
                            input_label(&link.to.1).to_lowercase()
                        ),
                        Bounds::centered_at(
                            point((start.x + end.x) / 2., (start.y + end.y) / 2.),
                            size(px(4.), px(4.)),
                        ),
                        window,
                        cx,
                    );
                }
                if let Some((from, at, snap, color)) = &dragged {
                    let end = snap
                        .as_ref()
                        .and_then(|to| geometry.input(to))
                        .unwrap_or(*at);
                    if let Some(start) = geometry.output(from) {
                        if let Some(path) = wire_path(start, end, (WIRE_WIDTH + 1.) * zoom, zoom) {
                            window.paint_path(path, *color);
                        }
                    }
                }
            });
            drop(geometry);
            listen(&app, &hitbox, bounds, window);
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// This frame's pointer handlers. Press and wheel belong to the canvas's
/// own ground (the middle button and the wheel reach through the cards);
/// move and release are the window's, so a drag that leaves the canvas
/// keeps tracking and ends wherever the button comes up.
fn listen(app: &Entity<Luma>, hitbox: &Hitbox, bounds: Bounds<Pixels>, window: &mut Window) {
    let pressed = app.clone();
    let inside = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let at = event.position;
        match event.button {
            MouseButton::Middle if bounds.contains(&at) => {
                pressed.update(cx, |this, cx| this.canvas_pan_press(at, false, cx));
            }
            MouseButton::Left if inside.is_hovered(window) => {
                pressed.update(cx, |this, cx| this.canvas_pan_press(at, true, cx));
            }
            MouseButton::Right if inside.is_hovered(window) => pressed.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.canvas.menu = Some(at))
            }),
            _ => {}
        }
    });
    let moved = app.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            let at = event.position;
            moved.update(cx, |this, cx| {
                this.canvas_move(at, bounds.contains(&at), cx)
            });
        }
    });
    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble
            && matches!(event.button, MouseButton::Left | MouseButton::Middle)
        {
            let at = event.position;
            released.update(cx, |this, cx| this.canvas_release(at, cx));
        }
    });
    let wheeled = app.clone();
    let over = hitbox.clone();
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !over.should_handle_scroll(window) {
            return;
        }
        let at = event.position;
        let zoom_key = event.modifiers.control || event.modifiers.platform;
        match event.delta {
            // A mouse wheel: zoom, or pan across with Shift.
            ScrollDelta::Lines(lines) if event.modifiers.shift => {
                let across = lines.y * PIXELS_PER_LINE;
                wheeled.update(cx, |this, cx| {
                    this.canvas_pan_by(point(px(across), px(0.)), cx)
                });
            }
            ScrollDelta::Lines(lines) => {
                let factor = (lines.y * PIXELS_PER_LINE * ZOOM_PER_PIXEL).exp();
                wheeled.update(cx, |this, cx| this.canvas_zoom(at, factor, cx));
            }
            // A trackpad: pan, or zoom with Ctrl or Cmd (a pinch arrives so
            // on some platforms).
            ScrollDelta::Pixels(delta) if zoom_key => {
                let factor = (f32::from(delta.y) * ZOOM_PER_PIXEL).exp();
                wheeled.update(cx, |this, cx| this.canvas_zoom(at, factor, cx));
            }
            ScrollDelta::Pixels(delta) => {
                wheeled.update(cx, |this, cx| this.canvas_pan_by(delta, cx));
            }
        }
    });
    let pinched = app.clone();
    let under = hitbox.clone();
    window.on_mouse_event(move |event: &PinchEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble && under.should_handle_scroll(window) {
            let (at, factor) = (event.position, 1. + event.delta);
            pinched.update(cx, |this, cx| this.canvas_zoom(at, factor, cx));
        }
    });
}

/// A wire's corner points: out along a short stub, across, and in along
/// another. The same polyline is painted (rounded) and hovered.
fn corners(from: Point<Pixels>, to: Point<Pixels>, zoom: f32) -> [Point<Pixels>; 4] {
    let stub = px(WIRE_STUB * zoom);
    [
        from,
        point(from.x + stub, from.y),
        point(to.x - stub, to.y),
        to,
    ]
}

/// A wire from an output port to an input port, the two turns rounded.
fn wire_path(
    from: Point<Pixels>,
    to: Point<Pixels>,
    width: f32,
    zoom: f32,
) -> Option<Path<Pixels>> {
    let corners = corners(from, to, zoom);
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(corners[0]);
    for index in 1..corners.len() - 1 {
        let (previous, corner, next) = (corners[index - 1], corners[index], corners[index + 1]);
        let radius = (WIRE_FILLET * zoom)
            .min(distance(previous, corner) / 2.)
            .min(distance(corner, next) / 2.);
        if radius <= 0. {
            path.line_to(corner);
            continue;
        }
        // Straight in to `radius` before the corner, then a quadratic with
        // the corner as its control point out to `radius` past it.
        path.line_to(toward(corner, previous, radius));
        path.curve_to(toward(corner, next, radius), corner);
    }
    path.line_to(corners[corners.len() - 1]);
    // Two ports at one point leave nothing to draw.
    path.build().ok()
}

fn distance(from: Point<Pixels>, to: Point<Pixels>) -> f32 {
    f32::from(to.x - from.x).hypot(f32::from(to.y - from.y))
}

/// How far `at` is from the segment `a`–`b`.
fn segment_distance(at: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>) -> f32 {
    let length = distance(a, b);
    if length == 0. {
        return distance(at, a);
    }
    let t = (f32::from(at.x - a.x) * f32::from(b.x - a.x)
        + f32::from(at.y - a.y) * f32::from(b.y - a.y))
        / (length * length);
    distance(at, toward(a, b, t.clamp(0., 1.) * length))
}

/// `from`, moved `by` toward `to`.
fn toward(from: Point<Pixels>, to: Point<Pixels>, by: f32) -> Point<Pixels> {
    let length = distance(from, to);
    if length == 0. {
        return from;
    }
    let t = by / length;
    point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
}

/// The input port a wire from `from` would land on from `at`: the nearest
/// that takes it within snapping reach.
fn snap_target(
    geometry: &Geometry,
    graph: &ClipGraph,
    drafts: &[Draft],
    from: &Source,
    at: Point<Pixels>,
) -> Option<Port> {
    geometry
        .inputs
        .iter()
        .filter(|((id, input), _)| fits(graph, drafts, from, id, input))
        .map(|(key, centre)| (key, distance(*centre, at)))
        .filter(|(_, reach)| *reach <= SNAP)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(key, _)| key.clone())
}

// -- gestures ---------------------------------------------------------------------

/// The primary clip's graph, as the canvas shows it.
fn shown_graph(editor: &Editor) -> Option<ClipGraph> {
    Some(primary_clip(editor)?.core.as_ref()?.graph.clone())
}

impl Luma {
    fn canvas_pan_press(&mut self, at: Point<Pixels>, clear: bool, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            if clear {
                canvas.selected = None;
            }
            canvas.gesture = Some(Gesture::Pan { last: at });
        });
        self.canvas_focus(cx);
    }

    /// Give the canvas the keyboard.
    fn canvas_focus(&mut self, cx: &mut Context<Self>) {
        let focus = self
            .track_editor_ref()
            .and_then(|editor| editor.sheet.canvas.focus.clone());
        if let (Some(focus), Some(window)) = (focus, cx.active_window()) {
            window
                .update(cx, |_, window, cx| focus.focus(window, cx))
                .ok();
        }
    }

    fn canvas_select(&mut self, node: Option<String>, focus: bool, cx: &mut Context<Self>) {
        let changed = self
            .track_editor_ref()
            .is_some_and(|editor| editor.sheet.canvas.selected != node);
        if changed {
            self.with_track_editor(cx, |editor| editor.sheet.canvas.selected = node);
        }
        if focus {
            self.canvas_focus(cx);
        }
    }

    fn canvas_wire_press(
        &mut self,
        from: Source,
        detach: Option<Port>,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.with_track_editor(cx, |editor| {
            editor.sheet.canvas.gesture = Some(Gesture::Wire {
                from,
                detach,
                at,
                snap: None,
            });
        });
        self.canvas_focus(cx);
    }

    /// A pointer move anywhere in the window. A running gesture follows it;
    /// otherwise, over the canvas, the wire under it lights. Nothing redraws
    /// unless something changed.
    fn canvas_move(&mut self, at: Point<Pixels>, over: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.track_editor_ref() else {
            return;
        };
        let canvas = &editor.sheet.canvas;
        match &canvas.gesture {
            Some(Gesture::Wire { from, .. }) => {
                let snap = shown_graph(editor).and_then(|graph| {
                    snap_target(&canvas.geometry.borrow(), &graph, &canvas.drafts, from, at)
                });
                self.with_track_editor(cx, |editor| {
                    if let Some(Gesture::Wire {
                        at: to, snap: s, ..
                    }) = &mut editor.sheet.canvas.gesture
                    {
                        *to = at;
                        *s = snap;
                    }
                });
            }
            Some(Gesture::Pan { .. }) => self.with_track_editor(cx, |editor| {
                let canvas = &mut editor.sheet.canvas;
                if let Some(Gesture::Pan { last }) = &mut canvas.gesture {
                    canvas.pan = canvas.pan + (at - *last);
                    *last = at;
                }
            }),
            None => {
                let hover = over
                    .then(|| {
                        let graph = shown_graph(editor)?;
                        let geometry = canvas.geometry.borrow();
                        let zoom = canvas.zoom;
                        graph
                            .nodes
                            .iter()
                            .flat_map(|(id, node)| {
                                node.wires().map(move |(input, from)| {
                                    (from.to_owned(), (id.clone(), input.to_owned()))
                                })
                            })
                            .filter_map(|(from, to)| {
                                let start = geometry.output(&Source::Node(from))?;
                                let end = geometry.input(&to)?;
                                let corners = corners(start, end, zoom);
                                let near = corners
                                    .windows(2)
                                    .map(|pair| segment_distance(at, pair[0], pair[1]))
                                    .fold(f32::MAX, f32::min);
                                (near <= WIRE_HOVER).then_some((to, near))
                            })
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(to, _)| to)
                    })
                    .flatten();
                if hover != canvas.hover {
                    self.with_track_editor(cx, |editor| editor.sheet.canvas.hover = hover);
                }
            }
        }
    }

    fn canvas_pan_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            canvas.pan = canvas.pan + delta;
        });
    }

    fn canvas_zoom(&mut self, about: Point<Pixels>, factor: f32, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| editor.sheet.canvas.zoom_about(about, factor));
    }

    /// A canvas key or toolbar button: `+`/`=` and `-` zoom about the
    /// centre, `0` is actual size at rest, `fit` frames the graph, Delete or
    /// Backspace removes the selected node, Escape drops a wire drag or the
    /// selection. Returns whether the key meant something here.
    fn canvas_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        match key {
            "+" | "=" | "-" => {
                let factor = if key == "-" {
                    1. / ZOOM_STEP
                } else {
                    ZOOM_STEP
                };
                self.with_track_editor(cx, |editor| {
                    let canvas = &mut editor.sheet.canvas;
                    let centre = canvas.centre();
                    canvas.zoom_about(centre, factor);
                });
            }
            "0" => self.with_track_editor(cx, |editor| {
                let canvas = &mut editor.sheet.canvas;
                canvas.zoom = 1.;
                canvas.pan = Point::default();
            }),
            "fit" => self.with_track_editor(cx, |editor| editor.sheet.canvas.fit()),
            "delete" | "backspace" => {
                let selected = self
                    .track_editor_ref()
                    .and_then(|editor| editor.sheet.canvas.selected.clone());
                match selected {
                    Some(id) => self.graph_delete_node(&id, cx),
                    None => return false,
                }
            }
            "escape" => {
                let mut handled = false;
                self.with_track_editor(cx, |editor| {
                    let canvas = &mut editor.sheet.canvas;
                    handled = canvas.cancel() || canvas.selected.take().is_some();
                });
                return handled;
            }
            _ => return false,
        }
        true
    }

    /// The button came up: a wire lands on the port it snapped to, or the
    /// input port under it; or, when it was picked up from an input and
    /// dropped on nothing, that input goes back to its last value.
    fn canvas_release(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(editor) = self.track_editor_ref() else {
            return;
        };
        let canvas = &editor.sheet.canvas;
        let Some(gesture) = &canvas.gesture else {
            return;
        };
        let wire = match gesture {
            Gesture::Pan { .. } => None,
            Gesture::Wire {
                from, detach, snap, ..
            } => {
                let under = canvas
                    .geometry
                    .borrow()
                    .inputs
                    .iter()
                    .map(|(key, centre)| (key.clone(), distance(*centre, at)))
                    .filter(|(_, reach)| *reach <= PORT * canvas.zoom.max(1.))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(key, _)| key);
                let target = snap.clone().or(under);
                Some((from.clone(), detach.clone(), target, canvas.drafts.clone()))
            }
        };
        self.with_track_editor(cx, |editor| editor.sheet.canvas.gesture = None);
        let Some((from, detach, target, drafts)) = wire else {
            return;
        };
        let Some(graph) = self.track_editor_ref().and_then(shown_graph) else {
            return;
        };
        match target {
            Some((id, input)) if fits(&graph, &drafts, &from, &id, &input) => {
                self.canvas_connect(&from, &drafts, &id, &input, detach, cx);
            }
            Some(target) if Some(&target) == detach.as_ref() => {}
            Some((id, input)) => {
                let what = match &from {
                    Source::Node(from) => edit::label(from),
                    Source::Draft(index) => drafts[*index].kind.label().to_owned(),
                };
                let into = format!(
                    "{} {}",
                    edit::label(&id),
                    input_label(&input).to_lowercase()
                );
                self.with_track_editor(cx, |editor| {
                    editor.sheet.error = Some(format!("{what} cannot feed {into}"));
                });
            }
            None => {
                if let Some((id, input)) = detach {
                    self.graph_pick(&id, &input, Pick::Value, cx);
                }
            }
        }
    }

    /// Wire `from` into an input: link a node already in the graph, or add
    /// a draft with what it needs to check. An input that holds a list takes
    /// the wire as one more item. A wire picked up from another input leaves
    /// it in the same edit.
    fn canvas_connect(
        &mut self,
        from: &Source,
        drafts: &[Draft],
        id: &str,
        input: &str,
        detach: Option<Port>,
        cx: &mut Context<Self>,
    ) {
        let key = (id.to_owned(), input.to_owned());
        let last = self
            .track_editor_ref()
            .map(|editor| editor.sheet.last.clone());
        let last = last.unwrap_or_default();
        let mut held: Option<Input> = None;
        let held_out = &mut held;
        let draft = match from {
            Source::Draft(index) => drafts.get(*index).map(|draft| (*index, draft.kind)),
            Source::Node(_) => None,
        };
        self.graph_live(cx, |graph| {
            *held_out = graph
                .nodes
                .get(id)
                .and_then(|node| node.inputs.get(input))
                .filter(|value| !matches!(value, Input::Wire(_) | Input::List(_)))
                .cloned();
            match (from, draft) {
                (Source::Node(from), _) => edit::link(graph, id, input, from),
                (Source::Draft(_), Some((_, kind))) => {
                    edit::attach(graph, id, input, kind);
                }
                (Source::Draft(_), None) => {}
            }
            if let Some((at, name)) = &detach {
                let back = restore(graph, at, name, last.get(&(at.clone(), name.clone())));
                edit::set_input(graph, at, name, back);
            }
        });
        let linked = self
            .track_editor_ref()
            .and_then(primary_clip)
            .and_then(|clip| clip.core.as_ref())
            .and_then(|core| core.graph.nodes.get(id)?.inputs.get(input).cloned())
            .is_some_and(|held| held.sources().next().is_some());
        self.with_track_editor(cx, |editor| {
            if !linked {
                return;
            }
            if let Some(value) = held {
                editor.sheet.last.insert(key, value);
            }
            if let Some((index, _)) = draft {
                if index < editor.sheet.canvas.drafts.len() {
                    editor.sheet.canvas.drafts.remove(index);
                }
            }
        });
    }

    /// A draft from the add menu, placed where the menu opened.
    fn canvas_add(&mut self, kind: Kind, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            let from = at - canvas.anchor();
            let zoom = canvas.zoom;
            canvas.menu = None;
            canvas.drafts.push(Draft {
                kind,
                at: point(f32::from(from.x) / zoom, f32::from(from.y) / zoom),
            });
        });
    }

    /// Open node `id`'s name for typing on its card. Enter or a press
    /// elsewhere commits it; Escape leaves it as it was.
    fn graph_rename_start(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| {
            let mut input = TextInput::search("Node name", cx);
            input.set_text(id.to_owned(), cx);
            input
        });
        let keys = cx.subscribe_in(
            &field,
            window,
            |this: &mut Luma, _, event: &text_input::Event, window, cx| match event {
                text_input::Event::Submitted => this.graph_rename_finish(true, Some(window), cx),
                text_input::Event::Cancelled => this.graph_rename_finish(false, Some(window), cx),
                // A press elsewhere: focus goes where the press sends it.
                text_input::Event::Blurred => this.graph_rename_finish(true, None, cx),
                _ => {}
            },
        );
        // Focus leaving by the keyboard; whichever of this and `Blurred`
        // comes second finds nothing to commit.
        let luma = cx.entity().downgrade();
        let focus = field.read(cx).focus_handle(cx);
        let blur = window.on_focus_out(&focus, cx, move |_, _, cx| {
            luma.update(cx, |this, cx| this.graph_rename_finish(true, None, cx))
                .ok();
        });
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            canvas.rename_error = None;
            canvas.rename = Some(Rename {
                id: id.to_owned(),
                field,
                _subs: vec![keys, blur],
            });
        });
        window.focus(&focus, cx);
    }

    /// End a rename. `save` renames the node when the typed name differs:
    /// the node and every wire into it, as one graph edit. A name the graph
    /// cannot take leaves the old one, and the card says why. `window` is
    /// present when a key ended it, and the canvas then takes the keyboard
    /// back; on a blur, focus has already gone where the person sent it.
    fn graph_rename_finish(
        &mut self,
        save: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let mut ended = None;
        let mut focus = None;
        self.with_track_editor(cx, |editor| {
            ended = editor.sheet.canvas.rename.take();
            focus = editor.sheet.canvas.focus.clone();
        });
        let Some(rename) = ended else {
            return;
        };
        if let (Some(window), Some(focus)) = (window, focus) {
            window.focus(&focus, cx);
        }
        let (from, to) = (rename.id, rename.field.read(cx).text().trim().to_owned());
        if !save || to == from {
            return;
        }
        let Some(mut graph) = self.track_editor_ref().and_then(shown_graph) else {
            return;
        };
        if let Err(error) = edit::rename(&mut graph, &from, &to) {
            self.with_track_editor(cx, |editor| {
                editor.sheet.canvas.rename_error = Some((from, error));
            });
            return;
        }
        self.graph_live(cx, |graph| {
            // Checked above on the primary clip; the others share its shape.
            let _ = edit::rename(graph, &from, &to);
        });
        let renamed = self
            .track_editor_ref()
            .and_then(shown_graph)
            .is_some_and(|graph| graph.nodes.contains_key(&to));
        if !renamed {
            return;
        }
        // What the sheet keeps by node follows the node to its new name.
        self.with_track_editor(cx, |editor| {
            let sheet = &mut editor.sheet;
            sheet.last = std::mem::take(&mut sheet.last)
                .into_iter()
                .map(|((at, input), value)| {
                    let at = if at == from { to.clone() } else { at };
                    ((at, input), value)
                })
                .collect();
            sheet.open = None;
            let canvas = &mut sheet.canvas;
            if canvas.selected.as_deref() == Some(from.as_str()) {
                canvas.selected = Some(to.clone());
            }
            if canvas.wide.remove(&from) {
                canvas.wide.insert(to.clone());
            }
            canvas.hover = None;
        });
    }

    /// Delete a node: each input it fed goes back to its last value, or to
    /// empty.
    fn graph_delete_node(&mut self, id: &str, cx: &mut Context<Self>) {
        let last = self
            .track_editor_ref()
            .map(|editor| editor.sheet.last.clone());
        let last = last.unwrap_or_default();
        self.graph_live(cx, |graph| {
            edit::delete(graph, id, |graph, at, name| {
                restore(graph, at, name, last.get(&(at.to_owned(), name.to_owned())))
            });
        });
        self.with_track_editor(cx, |editor| {
            if editor.sheet.canvas.selected.as_deref() == Some(id) {
                editor.sheet.canvas.selected = None;
            }
        });
    }
}
