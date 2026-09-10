//! Socket drags and cursor-anchored insertion. The document's edit operation
//! owns compatibility; the canvas only previews its answer.
use super::*;
use luma_patterns as p;
use luma_ui::float::{self, Dismiss, RowState};

#[derive(Clone, PartialEq, Eq)]
struct Socket {
    node: SharedString,
    port: SharedString,
    output: bool,
}

impl Socket {
    fn binding(&self, other: &Self) -> p::GraphEdit {
        let (from, to) = if self.output {
            (self, other)
        } else {
            (other, self)
        };
        p::GraphEdit::Bind {
            node: to.node.to_string(),
            input: to.port.to_string(),
            binding: Some(
                match luma_lib::node_graph::lighting::input_node_key(&from.node) {
                    Some(key) => p::Binding::Input { input: key.into() },
                    None => p::Binding::Connection {
                        node: from.node.to_string(),
                        output: from.port.to_string(),
                    },
                },
            ),
        }
    }
}

impl Scene {
    fn socket(&self, at: Point<f32>, zoom: f32) -> Option<Socket> {
        let Hit::Port { card, port, output } = self.hit(at, zoom) else {
            return None;
        };
        let card = &self.cards[card];
        Some(Socket {
            node: card.node_id.clone(),
            port: if output {
                &card.outputs[port]
            } else {
                &card.inputs[port]
            }
            .id
            .clone(),
            output,
        })
    }

    fn socket_position(&self, socket: &Socket) -> Option<Point<f32>> {
        let card = self.cards.iter().find(|card| card.node_id == socket.node)?;
        let ports = if socket.output {
            &card.outputs
        } else {
            &card.inputs
        };
        let port = ports.iter().find(|port| port.id == socket.port)?;
        Some(card.origin + port.at)
    }
}

#[derive(Clone)]
pub(super) struct WireDrag {
    from: Socket,
    cursor: Point<f32>,
    hovered: Option<Socket>,
    targets: Vec<(Socket, Result<(), String>)>,
}

impl WireDrag {
    fn error(&self) -> Option<&str> {
        let hovered = self.hovered.as_ref()?;
        if hovered == &self.from {
            return None;
        }
        self.targets
            .iter()
            .find(|(socket, _)| socket == hovered)?
            .1
            .as_ref()
            .err()
            .map(String::as_str)
    }

    pub(super) fn paint(
        &self,
        bounds: Bounds<Pixels>,
        scene: &Scene,
        view: Viewport,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(start) = scene.socket_position(&self.from) else {
            return;
        };
        let end = self
            .hovered
            .as_ref()
            .and_then(|socket| scene.socket_position(socket))
            .unwrap_or(self.cursor);
        let (from, to) = if self.from.output {
            (start, end)
        } else {
            (end, start)
        };
        let color = if self.error().is_some() {
            ladder::danger()
        } else {
            ladder::primary()
        };
        paint_wire(
            bounds.origin,
            from,
            to,
            color,
            view,
            WIRE_WIDTH + 1.,
            window,
        );
        for (socket, valid) in &self.targets {
            let hovered = self.hovered.as_ref() == Some(socket);
            if valid.is_err() && !hovered {
                continue;
            }
            let Some(at) = scene.socket_position(socket) else {
                continue;
            };
            let at = view.to_window(bounds.origin, at);
            let radius = px((PORT_RING * view.zoom / 2. + 3.).max(6.));
            window.paint_quad(quad(
                Bounds {
                    origin: at - point(radius, radius),
                    size: size(radius * 2., radius * 2.),
                },
                Corners::all(radius),
                transparent_black(),
                Edges::all(px(if hovered { 2. } else { 1. })),
                if valid.is_ok() {
                    ladder::primary()
                } else {
                    ladder::danger()
                },
                BorderStyle::Solid,
            ));
        }
        if let Some(error) = self.error() {
            let text: SharedString = error.to_string().into();
            let line = paint::shape(&text, 12., FontWeight::NORMAL, ladder::danger(), window);
            let width = line.width + px(16.);
            let cursor = view.to_window(bounds.origin, self.cursor);
            let at = point(
                cursor.x.min(bounds.right() - width).max(bounds.left()),
                (cursor.y + px(18.))
                    .min(bounds.bottom() - px(26.))
                    .max(bounds.top()),
            );
            let box_ = Bounds {
                origin: at,
                size: size(width, px(26.)),
            };
            window.paint_quad(fill(box_, ladder::card()));
            paint::line(
                point(box_.origin.x + px(8.), box_.origin.y + px(5.2)),
                &text,
                12.,
                FontWeight::NORMAL,
                ladder::danger(),
                window,
                cx,
            );
            agent_paint_node(Role::Text, text, box_, window, cx);
        }
    }
}

impl Editor {
    fn connection_check(&self, from: &Socket, to: &Socket) -> Result<(), String> {
        if from.output == to.output {
            return Err("Connect an output to an input".into());
        }
        let source = &self.source;

        let id = self.edited_definition().ok_or("No graph is open")?;
        source
            .library
            .definitions
            .get(&id)
            .ok_or("Graph is missing")?
            .clone()
            .edit(&source.library, from.binding(to))
            .map_err(|e| e.to_string())
    }
}

impl Luma {
    pub(super) fn graph_wire_press(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return false;
        };
        let cursor = editor.view.get().to_graph(editor.origin.get(), at);
        let scene = editor.scene.borrow();
        let Some(from) = scene.socket(cursor, editor.view.get().zoom) else {
            return false;
        };
        editor.selected = vec![from.node.clone()];
        editor.selected_edge = None;
        if editor.inspecting_builtin() {
            cx.notify();
            return true;
        }
        let mut targets = Vec::new();
        for card in &scene.cards {
            for (ports, output) in [(&card.inputs, false), (&card.outputs, true)] {
                for port in ports {
                    let socket = Socket {
                        node: card.node_id.clone(),
                        port: port.id.clone(),
                        output,
                    };
                    if socket != from {
                        let result = editor.connection_check(&from, &socket);
                        targets.push((socket, result));
                    }
                }
            }
        }
        {
            let source = &mut editor.source;
            source.selected_output = (from.output
                && luma_lib::node_graph::lighting::input_node_key(&from.node).is_none())
            .then(|| (from.node.to_string(), from.port.to_string()));
        }
        // Keep the error row's height while a pointer gesture is in flight.
        // Removing it here shifts every destination socket under the cursor.
        editor.fit = false;
        editor.gesture = Some(Gesture::Wire(WireDrag {
            from,
            cursor,
            hovered: None,
            targets,
        }));
        cx.notify();
        true
    }

    pub(super) fn graph_wire_drag(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return false;
        };
        let Some(Gesture::Wire(wire)) = &mut editor.gesture else {
            return false;
        };
        wire.cursor = editor.view.get().to_graph(editor.origin.get(), at);
        wire.hovered = editor
            .scene
            .borrow()
            .socket(wire.cursor, editor.view.get().zoom);
        cx.notify();
        true
    }

    pub(super) fn graph_wire_release(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.graph_wire_drag(target, at, cx) {
            return false;
        }
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return true;
        };
        let Some(Gesture::Wire(wire)) = editor.gesture.take() else {
            return true;
        };
        let Some(to) = wire.hovered.as_ref().filter(|to| *to != &wire.from) else {
            return true;
        };
        if let Err(error) = editor.connection_check(&wire.from, to) {
            editor.error = Some(error);
            return true;
        }
        self.apply_score_graph_edit(target, wire.from.binding(to), cx);
        true
    }
}

pub(super) struct NodeMenu {
    at: Point<Pixels>,
    pub position: [f64; 2],
    pub active: usize,
}

fn matches(source: &score::ScoreGraph) -> Vec<(String, String)> {
    let query = source.catalog_query.to_lowercase();
    let mut rows: Vec<_> = source
        .library
        .definitions
        .iter()
        // Saved graphs may still name the old field-specific samplers. Both
        // execute the same tensor kernels as the ordinary samplers; they are
        // compatibility spellings, not additional authoring choices.
        .filter(|(id, _)| {
            !matches!(
                id.as_str(),
                "sample_field_envelope" | "sample_field_gradient"
            )
        })
        .filter(|(_, definition)| {
            // A complete clip is selected on the timeline. In the canvas,
            // composition uses its numerical graph and one explicit Output.
            (!definition.playable() || definition.body == p::Body::Primitive(p::Primitive::Output))
                && !matches!(
                    definition.body,
                    p::Body::Primitive(
                        p::Primitive::ChaseEvents
                            | p::Primitive::PulseEvents
                            | p::Primitive::DissolveEvents
                    )
                )
        })
        .filter(|(id, definition)| {
            id.to_lowercase().contains(&query) || definition.name.to_lowercase().contains(&query)
        })
        .map(|(id, _)| (id.clone(), source.library.display_name(id)))
        .collect();
    if "input".contains(&query) {
        rows.push(("$input".into(), "Input".into()));
    }
    rows.sort_by(|a, b| {
        (a.0.starts_with("core/"), a.1.to_lowercase(), &a.0).cmp(&(
            b.0.starts_with("core/"),
            b.1.to_lowercase(),
            &b.0,
        ))
    });
    rows
}

impl Luma {
    pub(super) fn open_graph_catalog(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return;
        };
        if editor.inspecting_builtin() {
            return;
        }
        let bounds = Bounds {
            origin: editor.origin.get(),
            size: editor.canvas_size.get(),
        };
        let at = if bounds.contains(&at) {
            at
        } else {
            bounds.center()
        };
        let point = editor.view.get().to_graph(bounds.origin, at);
        let source = &mut editor.source;
        source.catalog = Some(NodeMenu {
            at,
            position: [f64::from(point.x), f64::from(point.y)],
            active: 0,
        });
        source.catalog_query.clear();
        source.catalog_scroll.scroll_to_item(0);
        source.search.update(cx, |field, cx| field.set_text("", cx));
        editor.gesture = None;
        editor.fit = false;
        // The popup's input must be mounted before the native input handler
        // takes focus. A click-to-type harness can conceal this first-open gap.
        let app = cx.entity().downgrade();
        let target = target.clone();
        window.on_next_frame(move |window, cx| {
            app.update(cx, |this, cx| {
                if this.workspace_hidden
                    || this.workspace.active() != Some(&target)
                    || this.overlay.as_open().is_some()
                {
                    return;
                }
                if let Some(TabBody::Graph(editor)) = this.workspace.body(&target) {
                    {
                        let source = &editor.source;
                        if source.catalog.is_some() {
                            source.search.focus_handle(cx).focus(window, cx);
                        }
                    }
                }
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn graph_add_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.active_graph_target() {
            self.open_graph_catalog(&target, window.mouse_position(), window, cx);
        }
    }

    pub(crate) fn graph_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        self.edit_graph_tab(&target, cx, |editor| {
            if matches!(editor.gesture, Some(Gesture::Wire(_))) {
                editor.gesture = None;
            }
            {
                let source = &mut editor.source;
                source.catalog = None;
            }
        });
        self.focus.focus(window, cx);
    }

    pub(crate) fn graph_catalog_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        self.edit_graph_tab(&target, cx, |editor| {
            let source = &mut editor.source;
            let count = matches(source).len();
            let Some(menu) = &mut source.catalog else {
                return;
            };
            if count > 0 {
                menu.active = if forward {
                    (menu.active + 1) % count
                } else {
                    (menu.active + count - 1) % count
                };
                source.catalog_scroll.scroll_to_item(menu.active);
            }
        });
    }

    pub(crate) fn graph_catalog_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        let Some(TabBody::Graph(editor)) = self.workspace.body(&target) else {
            return;
        };
        let source = &editor.source;
        let Some(menu) = &source.catalog else {
            return;
        };
        let Some((id, _)) = matches(source).get(menu.active).cloned() else {
            return;
        };
        self.add_score_graph_node(&target, &id, cx);
        self.focus.focus(window, cx);
    }
}

pub(super) fn catalog(editor: &Editor, app: &Entity<Luma>) -> Option<AnyElement> {
    let source = &editor.source;
    let menu = source.catalog.as_ref()?;
    let target = editor.target();
    let rows = matches(source);
    let mut list = div()
        .id("graph-node-list")
        .max_h(px(300.))
        .overflow_y_scroll()
        .track_scroll(&source.catalog_scroll);
    for (index, (id, label)) in rows.iter().enumerate() {
        let app = app.clone();
        let target = target.clone();
        let id = id.clone();
        list = list.child(
            float::menu_row(
                RowState::of(false, index == menu.active),
                format!("add-{id}"),
            )
            .id(SharedString::from(format!("graph-add-{id}")))
            .child(label.clone())
            .on_click(move |_, window, cx| {
                app.update(cx, |this, cx| {
                    this.add_score_graph_node(&target, &id, cx);
                    this.focus.focus(window, cx);
                });
            })
            .agent_node(Role::Button, format!("Add {label}")),
        );
    }
    if rows.is_empty() {
        list = list.child(float::empty_row("No matching nodes"));
    }
    let content = float::popover_card()
        .w(px(280.))
        .key_context(crate::keymap::context::GRAPH_NODE_SEARCH)
        .child(
            div()
                .h(px(34.))
                .flex_none()
                .child(source.search.clone())
                .agent_node(Role::Input, "Search nodes…"),
        )
        .child(list)
        .agent_node(Role::Card, "Add node menu");
    let app = app.clone();
    Some(float::anchored_at(
        "graph-node-menu",
        menu.at,
        Dismiss::on_press_out(move |window, cx| {
            app.update(cx, |this, cx| this.graph_cancel(window, cx));
        }),
        content.into_any_element(),
    ))
}
