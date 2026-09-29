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
//! The pan, the wire drag and the pointer handling come from the graph editor
//! this app had before clip graphs (`graph.rs`, removed in 6369a3b5): the
//! wire's stub-and-fillet path, the press/move/release split with move and
//! release on the window, and the drop onto the nearest port.

use luma_ui::glass;

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
/// toolbar its own band.
const CANVAS_PAD: f32 = 20.;
const CANVAS_TOP: f32 = 52.;
/// A port's ring, and the reach a drop still lands on it from.
const PORT: f32 = 10.;
const PORT_GRAB: f32 = 16.;
/// The horizontal run a wire leaves a port on before it turns, and the
/// radius of that turn.
const WIRE_STUB: f32 = 16.;
const WIRE_FILLET: f32 = 10.;
const WIRE_WIDTH: f32 = 1.5;

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
    /// Its top-left, from the canvas origin before the pan.
    pub at: Point<Pixels>,
}

pub(in crate::track_editor) enum Gesture {
    /// Moving the view; `last` is the previous pointer position.
    Pan { last: Point<Pixels> },
    /// Dragging a wire out of `from`. `detach` is the input it was picked up
    /// from, which loses it on the drop.
    Wire {
        from: Source,
        detach: Option<(String, String)>,
        at: Point<Pixels>,
    },
}

/// Where the ports were drawn this frame, in window space.
#[derive(Default)]
pub(in crate::track_editor) struct Geometry {
    canvas: Bounds<Pixels>,
    outputs: Vec<(Source, Point<Pixels>)>,
    inputs: Vec<((String, String), Point<Pixels>)>,
}

/// The canvas's own state, owned by the sheet. Reset when the subject
/// changes.
#[derive(Default)]
pub(in crate::track_editor) struct State {
    /// How far the view moved from rest, where the output sits at the top
    /// right.
    pub pan: Point<Pixels>,
    pub selected: Option<String>,
    pub gesture: Option<Gesture>,
    pub drafts: Vec<Draft>,
    /// Curves whose strip is widened.
    pub wide: BTreeSet<String>,
    /// The add menu, at this window point.
    pub menu: Option<Point<Pixels>>,
    pub geometry: Rc<RefCell<Geometry>>,
}

impl State {
    fn dragging(&self) -> Option<(&Source, Option<&(String, String)>)> {
        match &self.gesture {
            Some(Gesture::Wire { from, detach, .. }) => Some((from, detach.as_ref())),
            _ => None,
        }
    }
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

/// A port's ring. It records its centre as it prepaints, which is where the
/// wires meet it and what a drop is measured against.
fn ring(filled: bool, hot: bool, record: impl Fn(Point<Pixels>) + 'static) -> Div {
    let edge = if hot {
        ladder::primary().into()
    } else {
        glass::hairline(0.45)
    };
    div()
        .size(px(PORT))
        .flex_none()
        .rounded_full()
        .border(px(1.5))
        .border_color(edge)
        .bg(if filled { edge } else { ladder::card().into() })
        .child(canvas(move |bounds, _, _| record(bounds.center()), |_, _, _, _| {}).size_full())
}

/// An input's port, on the card's left edge. A press on a wired one picks
/// its wire up.
pub(super) fn input_port(cx: &Ctx, id: &str, input: &str) -> AnyElement {
    let canvas = &cx.state.sheet.canvas;
    let source = cx.graph.nodes[id]
        .inputs
        .get(input)
        .and_then(Input::source)
        .map(str::to_owned);
    let detached = canvas
        .dragging()
        .is_some_and(|(_, detach)| detach == Some(&(id.to_owned(), input.to_owned())));
    let hot = canvas
        .dragging()
        .is_some_and(|(from, _)| fits(cx.graph, &canvas.drafts, from, id, input));
    let geometry = canvas.geometry.clone();
    let key = (id.to_owned(), input.to_owned());
    let app = cx.app.clone();
    let label = format!(
        "{} {} port",
        edit::label(id),
        input_label(input).to_lowercase()
    );
    ring(source.is_some() && !detached, hot, move |at| {
        geometry.borrow_mut().inputs.push((key.clone(), at));
    })
    .id(SharedString::from(format!("port-{id}-{input}")))
    .occlude()
    .when_some(source, |port, source| {
        let (id, input) = (id.to_owned(), input.to_owned());
        port.cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                cx.stop_propagation();
                let from = Source::Node(source.clone());
                let detach = Some((id.clone(), input.clone()));
                let at = event.position;
                app.update(cx, |this, cx| this.canvas_wire_press(from, detach, at, cx));
            })
    })
    .agent_node(Role::Button, label)
    .into_any_element()
}

/// A node's output port, on the card's right edge. A press drags a new wire
/// out of it.
fn output_port(cx: &Ctx, from: Source, label: String) -> AnyElement {
    let canvas = &cx.state.sheet.canvas;
    let wired = match &from {
        Source::Node(id) => !edit::references(cx.graph, id).is_empty(),
        Source::Draft(_) => false,
    };
    let hot = canvas
        .dragging()
        .is_some_and(|(dragged, _)| *dragged == from);
    let geometry = canvas.geometry.clone();
    let recorded = from.clone();
    let app = cx.app.clone();
    let key = match &from {
        Source::Node(id) => format!("out-{id}"),
        Source::Draft(index) => format!("out-draft-{index}"),
    };
    ring(wired, hot, move |at| {
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
        .w(px(width))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
        .p(px(NODE_PAD))
        .rounded(px(luma_ui::radius::CARD))
        .bg(ladder::card())
        .border_1()
        .border_color(if selected {
            ladder::primary().into()
        } else {
            glass::hairline(0.1)
        })
        .block_mouse_except_scroll()
}

/// A card's title row: the label, then its buttons, and on a node that feeds
/// something its output port past the right edge.
fn title(label: &str, buttons: Vec<AnyElement>, port: Option<AnyElement>) -> Div {
    div()
        .relative()
        .w_full()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ladder::foreground())
                .child(label.to_owned()),
        )
        .child(div().flex_1())
        .children(buttons)
        .children(port.map(|port| {
            div()
                .absolute()
                .right(px(-NODE_PAD - PORT / 2. - 1.))
                .top(px((CONTROL_HEIGHT - PORT) / 2.))
                .child(port)
        }))
}

/// Where an input's port sits in its row's header line: on the card's left
/// edge.
pub(super) fn port_slot(port: AnyElement) -> Div {
    div()
        .absolute()
        .left(px(-NODE_PAD - PORT / 2. - 1.))
        .top(px((CONTROL_HEIGHT - PORT) / 2.))
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
    let app = cx.app.clone();
    let at = id.to_owned();
    plate(if wide { NODE_W_WIDE } else { NODE_W }, selected)
        .id(SharedString::from(format!("node-{id}")))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            let at = at.clone();
            app.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    let canvas = &mut editor.sheet.canvas;
                    if canvas.selected.as_deref() != Some(at.as_str()) {
                        canvas.selected = Some(at);
                    }
                })
            });
        })
        .child(title(&label, buttons, port))
        .children(settings(cx, id, node))
        .children(rows)
        .children(cx.controls.noise.get(id).cloned())
        .agent_node(Role::Card, label)
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
    let pan = cx.state.sheet.canvas.pan;
    div()
        .absolute()
        .left(draft.at.x + pan.x)
        .top(draft.at.y + pan.y)
        .child(
            plate(NODE_W, false)
                .child(title(&label, vec![discard], Some(port)))
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
    // Right to left in `columns`, so the sources come first on screen.
    let content = columns.iter().rev().fold(
        div()
            .absolute()
            .top(px(CANVAS_TOP) + state.pan.y)
            .right(px(CANVAS_PAD) - state.pan.x)
            .flex()
            .flex_row()
            .items_start()
            .gap(px(COLUMN_GAP)),
        |row, column| {
            row.child(
                column
                    .iter()
                    .fold(div().flex().flex_col().gap(px(NODE_GAP)), |col, id| {
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
    div()
        .id("clip-graph-canvas")
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_hidden()
        .bg(ladder::background())
        .child(wires(cx))
        .child(content)
        .children(drafts)
        .child(toolbar(cx, wide))
        .children(add_menu(cx))
        .agent_node(Role::Card, "Graph canvas")
        .into_any_element()
}

/// The buttons over the canvas's top-right corner: add a node, bring the
/// view back to rest, and widen the panel.
fn toolbar(cx: &Ctx, wide: Option<bool>) -> Div {
    let add = cx.app.clone();
    let fit = cx.app.clone();
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
        )
        .child(
            luma_ui::button("Fit", Enabled::Yes)
                .id("graph-fit")
                .on_click(move |_, _, cx| {
                    fit.update(cx, |this, cx| {
                        this.with_track_editor(cx, |editor| {
                            editor.sheet.canvas.pan = Point::default()
                        })
                    });
                })
                .agent_node(Role::Button, "Fit graph"),
        );
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

/// The wires, under the cards, from the port centres the cards recorded
/// this frame; the wire being dragged; and the canvas's pointer handling.
fn wires(cx: &Ctx) -> AnyElement {
    let state = &cx.state.sheet.canvas;
    let geometry = state.geometry.clone();
    let painted = state.geometry.clone();
    let detach = state.dragging().and_then(|(_, detach)| detach.cloned());
    let links: Vec<(Source, (String, String))> = cx
        .graph
        .nodes
        .iter()
        .flat_map(|(id, node)| {
            node.wires().map(move |(input, from)| {
                (
                    Source::Node(from.to_owned()),
                    (id.clone(), input.to_owned()),
                )
            })
        })
        .filter(|(_, to)| detach.as_ref() != Some(to))
        .collect();
    let dragged = match &state.gesture {
        Some(Gesture::Wire { from, at, .. }) => Some((from.clone(), *at)),
        _ => None,
    };
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
            let output = |from: &Source| {
                geometry
                    .outputs
                    .iter()
                    .find(|(source, _)| source == from)
                    .map(|(_, at)| *at)
            };
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for (from, to) in &links {
                    let start = output(from);
                    let end = geometry
                        .inputs
                        .iter()
                        .find(|(key, _)| key == to)
                        .map(|(_, at)| *at);
                    if let (Some(start), Some(end)) = (start, end) {
                        paint_wire(start, end, glass::hairline(0.4), WIRE_WIDTH, window);
                        agent_paint_node(
                            Role::Text,
                            format!(
                                "{} → {} {}",
                                match from {
                                    Source::Node(id) => edit::label(id),
                                    Source::Draft(_) => String::new(),
                                },
                                edit::label(&to.0),
                                input_label(&to.1).to_lowercase()
                            ),
                            Bounds::centered_at(
                                point((start.x + end.x) / 2., (start.y + end.y) / 2.),
                                size(px(4.), px(4.)),
                            ),
                            window,
                            cx,
                        );
                    }
                }
                if let Some((from, at)) = &dragged {
                    if let Some(start) = output(from) {
                        paint_wire(
                            start,
                            *at,
                            ladder::primary().into(),
                            WIRE_WIDTH + 0.5,
                            window,
                        );
                    }
                }
            });
            drop(geometry);
            listen(&app, &hitbox, window);
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// This frame's pointer handlers. Press and wheel belong to the canvas's
/// own ground; move and release are the window's, so a drag that leaves the
/// canvas keeps tracking and ends wherever the button comes up.
fn listen(app: &Entity<Luma>, hitbox: &Hitbox, window: &mut Window) {
    let pressed = app.clone();
    let inside = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !inside.is_hovered(window) {
            return;
        }
        let at = event.position;
        match event.button {
            MouseButton::Left => pressed.update(cx, |this, cx| this.canvas_pan_press(at, cx)),
            MouseButton::Right => pressed.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.canvas.menu = Some(at))
            }),
            _ => {}
        }
    });
    let moved = app.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            let at = event.position;
            moved.update(cx, |this, cx| this.canvas_move(at, cx));
        }
    });
    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
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
        let delta = event.delta.pixel_delta(window.line_height());
        wheeled.update(cx, |this, cx| this.canvas_pan_by(delta, cx));
    });
}

/// A wire from an output port to an input port: out along a short stub,
/// across, and in along another, the two turns rounded.
fn paint_wire(
    from: Point<Pixels>,
    to: Point<Pixels>,
    color: Hsla,
    width: f32,
    window: &mut Window,
) {
    let stub = px(WIRE_STUB);
    let corners = [
        from,
        point(from.x + stub, from.y),
        point(to.x - stub, to.y),
        to,
    ];
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(corners[0]);
    for index in 1..corners.len() - 1 {
        let (previous, corner, next) = (corners[index - 1], corners[index], corners[index + 1]);
        let radius = WIRE_FILLET
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
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn distance(from: Point<Pixels>, to: Point<Pixels>) -> f32 {
    f32::from(to.x - from.x).hypot(f32::from(to.y - from.y))
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

// -- gestures ---------------------------------------------------------------------

impl Luma {
    fn canvas_pan_press(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            canvas.selected = None;
            canvas.gesture = Some(Gesture::Pan { last: at });
        });
    }

    fn canvas_wire_press(
        &mut self,
        from: Source,
        detach: Option<(String, String)>,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.with_track_editor(cx, |editor| {
            editor.sheet.canvas.gesture = Some(Gesture::Wire { from, detach, at });
        });
    }

    /// A pointer move anywhere in the window: only a running gesture cares,
    /// so an idle pointer redraws nothing.
    fn canvas_move(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let moving = self
            .track_editor_ref()
            .is_some_and(|editor| editor.sheet.canvas.gesture.is_some());
        if !moving {
            return;
        }
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            match &mut canvas.gesture {
                Some(Gesture::Pan { last }) => {
                    canvas.pan = canvas.pan + (at - *last);
                    *last = at;
                }
                Some(Gesture::Wire { at: to, .. }) => *to = at,
                None => {}
            }
        });
    }

    fn canvas_pan_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            let canvas = &mut editor.sheet.canvas;
            canvas.pan = canvas.pan + delta;
        });
    }

    /// The button came up: a wire lands on the input port under it, or, when
    /// it was picked up from an input and dropped on nothing, that input
    /// goes back to its last value.
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
            Gesture::Wire { from, detach, .. } => {
                let target = canvas
                    .geometry
                    .borrow()
                    .inputs
                    .iter()
                    .map(|(key, centre)| (key.clone(), distance(*centre, at)))
                    .filter(|(_, reach)| *reach <= PORT_GRAB)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(key, _)| key);
                let drafts = canvas.drafts.clone();
                Some((from.clone(), detach.clone(), target, drafts))
            }
        };
        self.with_track_editor(cx, |editor| editor.sheet.canvas.gesture = None);
        let Some((from, detach, target, drafts)) = wire else {
            return;
        };
        let graph = self
            .track_editor_ref()
            .and_then(primary_clip)
            .and_then(|clip| Some(clip.core.as_ref()?.graph.clone()));
        let Some(graph) = graph else {
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
    /// a draft with what it needs to check. A wire picked up from another
    /// input leaves it in the same edit.
    fn canvas_connect(
        &mut self,
        from: &Source,
        drafts: &[Draft],
        id: &str,
        input: &str,
        detach: Option<(String, String)>,
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
                .filter(|value| value.source().is_none())
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
            .and_then(|core| {
                core.graph
                    .nodes
                    .get(id)?
                    .inputs
                    .get(input)?
                    .source()
                    .map(str::to_owned)
            })
            .is_some();
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
            let origin = canvas.geometry.borrow().canvas.origin;
            let local = at - origin - canvas.pan;
            canvas.menu = None;
            canvas.drafts.push(Draft { kind, at: local });
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
