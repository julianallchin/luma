//! Native values and exposure controls for the selected score-local node.
use super::*;
use luma_patterns as p;
use luma_ui::arg::{
    color::{ColorArg, ColorArgEditor, ColorArgEvent},
    envelope::{EnvelopeChanged, EnvelopeEditor},
    gradient::{Gradient, GradientStop},
    gradient_editor::{GradientChanged, GradientEditor},
    number::{DraftedNumber, NumberEvent},
};
use luma_ui::Enabled;

pub(super) struct Controls {
    definition: String,
    node: String,
    cells: Vec<InputControl>,
    _subscriptions: Vec<Subscription>,
}
struct InputControl {
    id: String,
    spec: p::Input,
    binding: Option<p::Binding>,
    value: Option<p::Value>,
    widget: Widget,
}
enum Widget {
    Number(Entity<DraftedNumber>),
    Color(Entity<ColorArgEditor>),
    Envelope(Entity<EnvelopeEditor>),
    Gradient(Entity<GradientEditor>),
    Choice,
    Connection,
}

fn resolved_value(
    parent: &p::Definition,
    spec: &p::Input,
    binding: Option<&p::Binding>,
) -> Option<p::Value> {
    match binding {
        Some(p::Binding::Value { value }) => Some(value.clone()),
        Some(p::Binding::Input { input }) => parent.inputs.get(input)?.default.clone(),
        Some(p::Binding::Connection { .. }) => None,
        None => spec.default.clone(),
    }
}
fn value_edit(
    node: &str,
    input: &str,
    binding: &Option<p::Binding>,
    value: p::Value,
) -> p::GraphEdit {
    match binding {
        Some(p::Binding::Input { input }) => p::GraphEdit::Default {
            key: input.clone(),
            value,
        },
        _ => p::GraphEdit::Bind {
            node: node.into(),
            input: input.into(),
            binding: Some(value.into()),
        },
    }
}

pub(super) fn sync(editor: &mut Editor, window: &mut Window, cx: &mut Context<Luma>) {
    let Some(definition) = editor.edited_definition() else {
        return;
    };
    let target = editor.target();
    if editor.inspecting_builtin() {
        return;
    }
    let Some(node_id) = editor.selected.first().map(ToString::to_string) else {
        return;
    };
    let Source::Score(source) = &mut editor.source else {
        return;
    };
    let Some(parent) = source.library.definitions.get(&definition) else {
        return;
    };
    let p::Body::Graph(graph) = &parent.body else {
        return;
    };
    let Some(node) = graph.nodes.get(&node_id) else {
        return;
    };
    let Some(child) = source.library.definitions.get(&node.definition) else {
        return;
    };
    let stale = source.controls.as_ref().is_none_or(|controls| {
        controls.definition != definition
            || controls.node != node_id
            || controls.cells.len() != child.inputs.len()
            || controls.cells.iter().any(|cell| {
                child
                    .inputs
                    .get(&cell.id)
                    .is_none_or(|input| input.value_type != cell.spec.value_type)
                    || matches!(cell.binding, Some(p::Binding::Connection { .. }))
                        != matches!(
                            node.inputs.get(&cell.id),
                            Some(p::Binding::Connection { .. })
                        )
            })
    });
    if !stale {
        for cell in &mut source.controls.as_mut().unwrap().cells {
            cell.spec = child.inputs[&cell.id].clone();
            cell.binding = node.inputs.get(&cell.id).cloned();
            let value = resolved_value(parent, &cell.spec, cell.binding.as_ref());
            if value != cell.value {
                match (&cell.widget, &value) {
                    (
                        Widget::Number(entity),
                        Some(
                            p::Value::Number(v)
                            | p::Value::Beats(v)
                            | p::Value::Proportion(v)
                            | p::Value::Position(v),
                        ),
                    ) => entity.update(cx, |field, cx| field.set_value(*v, cx)),
                    (Widget::Color(entity), Some(p::Value::Color(rgb))) => {
                        entity.update(cx, |field, cx| {
                            field.set_value(ColorArg::decode(rgb.map(|v| v as f32), 1.), cx)
                        })
                    }
                    (Widget::Gradient(entity), Some(p::Value::Gradient(gradient))) => {
                        entity.update(cx, |field, cx| {
                            field.set_value(display_gradient(gradient), cx)
                        });
                    }
                    (Widget::Envelope(entity), Some(p::Value::Envelope(envelope))) => {
                        entity.update(cx, |field, cx| field.set_value(envelope.clone(), cx))
                    }
                    _ => (),
                }
                cell.value = value;
            }
        }
        return;
    }
    let mut subscriptions = Vec::new();
    let mut cells = Vec::new();
    for (id, spec) in &child.inputs {
        let binding = node.inputs.get(id).cloned();
        let value = resolved_value(parent, spec, binding.as_ref());
        let node_id = node_id.clone();
        let input = id.clone();
        let target = target.clone();
        let widget = match &value {
            Some(
                p::Value::Number(v)
                | p::Value::Beats(v)
                | p::Value::Proportion(v)
                | p::Value::Position(v),
            ) => {
                let kind = spec.value_type;
                let field = cx.new(|cx| {
                    DraftedNumber::new(
                        spec.name.clone(),
                        *v,
                        if kind == p::ValueType::Proportion || kind == p::ValueType::Beats {
                            0.
                        } else {
                            -1e9
                        },
                        if kind == p::ValueType::Proportion {
                            1.
                        } else {
                            1e9
                        },
                        260.,
                        window,
                        cx,
                    )
                });
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &NumberEvent, cx| {
                        let NumberEvent::Committed(value) = *event;
                        let value = match kind {
                            p::ValueType::Beats => p::Value::Beats(value),
                            p::ValueType::Proportion => p::Value::Proportion(value),
                            p::ValueType::Position => p::Value::Position(value),
                            _ => p::Value::Number(value),
                        };
                        this.set_graph_input_value(&target, &node_id, &input, value, cx);
                    },
                ));
                Widget::Number(field)
            }
            Some(p::Value::Color(rgb)) => {
                let field = cx.new(|cx| {
                    ColorArgEditor::new(
                        spec.name.clone(),
                        ColorArg::decode(rgb.map(|v| v as f32), 1.),
                        cx,
                    )
                    .rgb_only()
                });
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &ColorArgEvent, cx| {
                        let ColorArgEvent::Changed(value) = *event;
                        this.set_graph_input_value(
                            &target,
                            &node_id,
                            &input,
                            p::Value::Color(value.rgb.map(f64::from)),
                            cx,
                        );
                    },
                ));
                Widget::Color(field)
            }
            Some(p::Value::Gradient(gradient)) => {
                let field = cx.new(|cx| GradientEditor::new(display_gradient(gradient), cx));
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &GradientChanged, cx| {
                        let stops = event
                            .0
                            .stops()
                            .iter()
                            .map(|stop| p::ColorStop {
                                t: f64::from(stop.t),
                                color: [stop.color.r, stop.color.g, stop.color.b].map(f64::from),
                            })
                            .collect();
                        this.set_graph_input_value(
                            &target,
                            &node_id,
                            &input,
                            p::Value::Gradient(p::Gradient { stops }),
                            cx,
                        );
                    },
                ));
                Widget::Gradient(field)
            }
            Some(p::Value::Envelope(envelope)) => {
                let field = cx.new(|_| EnvelopeEditor::new(envelope.clone()));
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &EnvelopeChanged, cx| {
                        this.set_graph_input_value(
                            &target,
                            &node_id,
                            &input,
                            p::Value::Envelope(event.0.clone()),
                            cx,
                        );
                    },
                ));
                Widget::Envelope(field)
            }
            Some(
                p::Value::Mapping(_)
                | p::Value::Boundary(_)
                | p::Value::Boolean(_)
                | p::Value::AudioSource(_)
                | p::Value::Drum(_),
            ) => Widget::Choice,
            _ => Widget::Connection,
        };
        cells.push(InputControl {
            id: id.clone(),
            spec: spec.clone(),
            binding,
            value,
            widget,
        });
    }
    source.controls = Some(Controls {
        definition,
        node: node_id,
        cells,
        _subscriptions: subscriptions,
    });
}

impl Luma {
    fn set_graph_input_value(
        &mut self,
        target: &Target,
        node: &str,
        input: &str,
        value: p::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let Some(definition) = editor.edited_definition() else {
            return;
        };
        let Source::Score(source) = &editor.source else {
            return;
        };
        let Some(p::Definition {
            body: p::Body::Graph(graph),
            ..
        }) = source.library.definitions.get(&definition)
        else {
            return;
        };
        let binding = graph
            .nodes
            .get(node)
            .and_then(|node| node.inputs.get(input))
            .cloned();
        self.apply_score_graph_edit(target, value_edit(node, input, &binding, value), cx);
    }
}

pub(super) fn add_button(editor: &Editor, app: &Entity<Luma>) -> Option<AnyElement> {
    if !matches!(editor.source, Source::Score(_)) || editor.inspecting_builtin() {
        return None;
    }
    let app = app.clone();
    let target = editor.target();
    Some(
        luma_ui::button("Add node", Enabled::Yes)
            .id("graph-add-node")
            .on_click(move |_, window, cx| {
                app.update(cx, |this, cx| {
                    let mut search = None;
                    this.edit_graph_tab(&target, cx, |editor| {
                        if let Source::Score(source) = &mut editor.source {
                            source.catalog_open = !source.catalog_open;
                            source.catalog_query.clear();
                            search = Some(source.search.clone());
                        }
                    });
                    if let Some(search) = search {
                        search.update(cx, |field, cx| field.set_text("", cx));
                        search.focus_handle(cx).focus(window, cx);
                    }
                });
            })
            .agent_node(Role::Button, "Add node")
            .into_any_element(),
    )
}

pub(super) fn panel(editor: &Editor, app: &Entity<Luma>) -> Option<AnyElement> {
    let Source::Score(source) = &editor.source else {
        return None;
    };
    if editor.inspecting_builtin() {
        return None;
    }
    let target = editor.target();
    let mut content = div()
        .id("graph-controls")
        .w(px(300.))
        .flex_none()
        .px(px(14.))
        .py(px(10.))
        .overflow_y_scroll()
        .border_l_1()
        .border_color(ladder::trim())
        .flex()
        .flex_col()
        .gap(px(12.));
    if source.catalog_open {
        content = content.child(
            div()
                .h(px(32.))
                .flex_none()
                .child(source.search.clone())
                .agent_node(Role::Input, "Search nodes…"),
        );
        let query = source.catalog_query.to_lowercase();
        for (id, definition) in &source.library.definitions {
            if !definition.name.to_lowercase().contains(&query)
                && !id.to_lowercase().contains(&query)
            {
                continue;
            }
            let app = app.clone();
            let target = target.clone();
            let id = id.clone();
            let label = source.library.display_name(&id);
            content = content.child(
                luma_ui::button(&label, Enabled::Yes)
                    .flex_none()
                    .id(SharedString::from(format!("graph-add-{id}")))
                    .on_click(move |_, _, cx| {
                        app.update(cx, |this, cx| {
                            this.edit_graph_tab(&target, cx, |editor| {
                                if let Source::Score(source) = &mut editor.source {
                                    source.catalog_open = false;
                                }
                            });
                            this.add_score_graph_node(&target, &id, cx);
                        });
                    })
                    .agent_node(Role::Button, format!("Add {label}")),
            );
        }
        return Some(content.into_any_element());
    }
    let Some(controls) = source.controls.as_ref() else {
        return Some(
            content
                .child(luma_ui::silkscreen("Select a node".to_string()))
                .into_any_element(),
        );
    };
    if editor.selected.first().map(|id| id.as_ref()) != Some(controls.node.as_str())
        || editor.edited_definition().as_deref() != Some(&controls.definition)
    {
        return Some(
            content
                .child(luma_ui::silkscreen("Select a node".to_string()))
                .into_any_element(),
        );
    }
    if let Some((node, port)) = &source.pending_port {
        content = content.child(luma_ui::silkscreen(format!(
            "{node}.{port} → click an input"
        )));
        let app = app.clone();
        let target = target.clone();
        let binding = p::Binding::Connection {
            node: node.clone(),
            output: port.clone(),
        };
        let key = source.library.definitions[&controls.definition]
            .lighting_output()
            .unwrap_or("lighting")
            .to_string();
        content = content.child(
            luma_ui::button("Use as graph output", Enabled::Yes)
                .id("graph-use-output")
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.apply_score_graph_edit(
                            &target,
                            p::GraphEdit::Output {
                                key: key.clone(),
                                binding: binding.clone(),
                            },
                            cx,
                        )
                    })
                })
                .agent_node(Role::Button, "Use as graph output"),
        );
    }
    let called = source
        .library
        .definitions
        .get(&controls.definition)
        .and_then(|parent| {
            let p::Body::Graph(graph) = &parent.body else {
                return None;
            };
            graph
                .nodes
                .get(&controls.node)
                .and_then(|node| source.library.definitions.get(&node.definition))
        });
    if called.is_some_and(|definition| matches!(definition.body, p::Body::Graph(_))) {
        let app = app.clone();
        let target = target.clone();
        let node = controls.node.clone();
        content = content.child(
            luma_ui::button("Edit a copy", Enabled::from(source.draft.is_none()))
                .id("graph-customize-node")
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.customize_score_graph_node(&target, &node, cx)
                    })
                })
                .agent_node(Role::Button, "Edit a copy"),
        );
    }
    for cell in &controls.cells {
        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(luma_ui::silkscreen(cell.spec.name.clone()));
        row = match &cell.widget {
            Widget::Number(field) => row.child(field.clone()),
            Widget::Color(field) => row.child(field.clone()),
            Widget::Envelope(field) => row.child(field.clone()),
            Widget::Gradient(field) => row.child(field.clone()),
            Widget::Choice => {
                let choices = luma_lib::node_graph::lighting::choices(cell.spec.value_type);
                let labels: Vec<&str> =
                    choices.iter().map(|option| option.label.as_str()).collect();
                let wire = cell
                    .value
                    .as_ref()
                    .map(luma_lib::node_graph::lighting::wire_value)
                    .unwrap_or_default();
                let key = wire
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| wire.to_string());
                let selected = choices
                    .iter()
                    .find(|option| option.id == key)
                    .map(|option| option.label.as_str())
                    .unwrap_or("Choose…");
                let toggle = app.clone();
                let toggle_target = target.clone();
                let app = app.clone();
                let target = target.clone();
                let node = controls.node.clone();
                let input = cell.id.clone();
                let menu = format!("{}.{}", node, input);
                let toggle_menu = menu.clone();
                let previous = cell.value.clone();
                let kind = cell.spec.value_type;
                let options = choices.clone();
                row.child(luma_ui::arg::select::luma_arg_select(
                    &cell.spec.name,
                    selected,
                    &labels,
                    source.choice_open.as_ref() == Some(&menu),
                    move |_, cx| {
                        toggle.update(cx, |this, cx| {
                            this.edit_graph_tab(&toggle_target, cx, |editor| {
                                if let Source::Score(source) = &mut editor.source {
                                    source.choice_open =
                                        if source.choice_open.as_ref() == Some(&toggle_menu) {
                                            None
                                        } else {
                                            Some(toggle_menu.clone())
                                        };
                                }
                            })
                        });
                    },
                    move |selected, _, cx| {
                        if let Ok(mut value) = luma_lib::node_graph::lighting::decode(
                            kind,
                            &serde_json::json!(options[selected].id),
                        ) {
                            if let (Some(p::Value::Mapping(previous)), p::Value::Mapping(mapping)) =
                                (&previous, &mut value)
                            {
                                mapping.reverse = previous.reverse;
                                mapping.per_group = previous.per_group;
                            }
                            app.update(cx, |this, cx| {
                                this.edit_graph_tab(&target, cx, |editor| {
                                    if let Source::Score(source) = &mut editor.source {
                                        source.choice_open = None;
                                    }
                                });
                                this.set_graph_input_value(&target, &node, &input, value, cx);
                            });
                        }
                    },
                ))
            }
            Widget::Connection => row.child(luma_ui::silkscreen(match &cell.binding {
                Some(p::Binding::Connection { node, output }) => format!("From {node}.{output}"),
                _ => format!("Connect a {:?} output", cell.spec.value_type),
            })),
        };
        let app = app.clone();
        let target = target.clone();
        let node = controls.node.clone();
        let input = cell.id.clone();
        let exposed = matches!(cell.binding, Some(p::Binding::Input { .. }));
        let edit = if exposed {
            p::GraphEdit::Bind {
                node: node.clone(),
                input: input.clone(),
                binding: cell.value.clone().map(Into::into),
            }
        } else if matches!(cell.binding, Some(p::Binding::Connection { .. })) {
            p::GraphEdit::Bind {
                node: node.clone(),
                input: input.clone(),
                binding: None,
            }
        } else {
            let parent = &source.library.definitions[&controls.definition];
            let mut key = input.clone();
            let mut index = 2;
            while parent.inputs.contains_key(&key) {
                key = format!("{input}_{index}");
                index += 1;
            }
            p::GraphEdit::Expose {
                node: node.clone(),
                input: input.clone(),
                key,
                name: None,
            }
        };
        let label = if exposed {
            "On clip · make local"
        } else if matches!(cell.binding, Some(p::Binding::Connection { .. })) {
            "Disconnect"
        } else {
            "Expose on clip"
        };
        row = row.child(
            luma_ui::button(label, Enabled::Yes)
                .id(SharedString::from(format!(
                    "expose-{}-{input}",
                    controls.node
                )))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.apply_score_graph_edit(&target, edit.clone(), cx)
                    })
                })
                .agent_node(Role::Button, format!("{}: {label}", cell.spec.name)),
        );
        content = content.child(row);
    }
    Some(content.into_any_element())
}

fn display_gradient(gradient: &p::Gradient) -> Gradient {
    Gradient::new(gradient.stops.iter().map(|stop| GradientStop {
        t: stop.t as f32,
        color: Rgba {
            r: stop.color[0] as f32,
            g: stop.color[1] as f32,
            b: stop.color[2] as f32,
            a: 1.0,
        },
    }))
}
