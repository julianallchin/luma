//! Native values and exposure controls for the selected score-local node.
use super::*;
use luma_patterns as p;
use luma_ui::arg::{
    color::{ColorArg, ColorArgEditor, ColorArgEvent},
    envelope::{EnvelopeChanged, EnvelopeEditor},
    gradient::{Gradient, GradientStop},
    gradient_editor::{GradientChanged, GradientEditor},
    mapping::{MappingChanged, MappingEditor},
    number::{DraftedNumber, NumberEvent},
    signal::{SignalChanged, SignalEditor},
};
use luma_ui::Enabled;

pub(super) struct Controls {
    definition: String,
    node: String,
    cells: Vec<InputControl>,
    name: Option<Entity<luma_ui::text_input::TextInput>>,
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
    Seed(Entity<DraftedNumber<u64>>),
    Number(Entity<DraftedNumber>),
    Signal(Entity<SignalEditor>),
    Color(Entity<ColorArgEditor>),
    Envelope(Entity<EnvelopeEditor>),
    Gradient(Entity<GradientEditor>),
    Mapping(Entity<MappingEditor>),
    Choice,
    Connection,
}
impl Widget {
    fn accepts(&self, value: Option<&p::Value>) -> bool {
        match value {
            Some(p::Value::Seed(_)) => matches!(self, Self::Seed(_)),
            Some(value) if value.scalar_value().is_some() => matches!(self, Self::Number(_)),
            Some(p::Value::Signal(_)) => matches!(self, Self::Signal(_)),
            Some(p::Value::Color(_)) => matches!(self, Self::Color(_)),
            Some(p::Value::Envelope(_)) => matches!(self, Self::Envelope(_)),
            Some(p::Value::Gradient(_)) => matches!(self, Self::Gradient(_)),
            Some(p::Value::Mapping(_)) => matches!(self, Self::Mapping(_)),
            Some(
                p::Value::Boundary(_)
                | p::Value::Boolean(_)
                | p::Value::AudioSource(_)
                | p::Value::Drum(_),
            ) => matches!(self, Self::Choice),
            _ => matches!(self, Self::Connection),
        }
    }
}

fn resolved_value(spec: &p::Input, binding: Option<&p::Binding>) -> Option<p::Value> {
    match binding {
        Some(p::Binding::Value { value }) => Some(value.clone()),
        Some(p::Binding::Input { .. } | p::Binding::Connection { .. }) => None,
        None => spec.default.clone(),
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
    let source = &mut editor.source;
    let Some(parent) = source.library.definitions.get(&definition) else {
        return;
    };
    let p::Body::Graph(graph) = &parent.body else {
        return;
    };
    let input_key = luma_lib::node_graph::lighting::input_node_key(&node_id);
    let (specs, bindings, input_name) = if let Some(key) = input_key {
        let name = parent
            .inputs
            .get(key)
            .map(|spec| spec.name.clone())
            .or_else(|| graph.input_nodes.get(key).map(|node| node.name.clone()));
        let specs = parent
            .inputs
            .get(key)
            .map(|spec| {
                let mut spec = spec.clone();
                spec.name = "Value".into();
                (key.to_string(), spec)
            })
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        (specs, std::collections::BTreeMap::new(), name)
    } else {
        let Some(node) = graph.nodes.get(&node_id) else {
            return;
        };
        let Some(child) = source.library.definitions.get(&node.definition) else {
            return;
        };
        (child.inputs.clone(), node.inputs.clone(), None)
    };
    let stale = source.controls.as_ref().is_none_or(|controls| {
        controls.definition != definition
            || controls.node != node_id
            || controls.cells.len() != specs.len()
            || controls.cells.iter().any(|cell| {
                specs.get(&cell.id).is_none_or(|input| {
                    input.value_type != cell.spec.value_type
                        || !cell
                            .widget
                            .accepts(resolved_value(input, bindings.get(&cell.id)).as_ref())
                }) || matches!(cell.binding, Some(p::Binding::Connection { .. }))
                    != matches!(bindings.get(&cell.id), Some(p::Binding::Connection { .. }))
            })
    });
    if !stale {
        if let (Some(field), Some(name)) = (&source.controls.as_ref().unwrap().name, &input_name) {
            if !field.focus_handle(cx).is_focused(window) && field.read(cx).text() != name {
                field.update(cx, |field, cx| field.set_text(name.clone(), cx));
            }
        }
        for cell in &mut source.controls.as_mut().unwrap().cells {
            cell.spec = specs[&cell.id].clone();
            cell.binding = bindings.get(&cell.id).cloned();
            let value = resolved_value(&cell.spec, cell.binding.as_ref());
            if value != cell.value {
                match (&cell.widget, &value) {
                    (Widget::Seed(entity), Some(p::Value::Seed(seed))) => {
                        entity.update(cx, |field, cx| field.set_value(*seed, cx));
                    }
                    (Widget::Signal(entity), Some(p::Value::Signal(signal))) => {
                        entity.update(cx, |field, cx| field.set_value(signal.clone(), window, cx));
                    }
                    (Widget::Number(entity), Some(value)) if value.scalar_value().is_some() => {
                        entity.update(cx, |field, cx| {
                            field.set_value(value.scalar_value().unwrap(), cx)
                        })
                    }
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
                    (Widget::Mapping(entity), Some(p::Value::Mapping(mapping))) => {
                        entity.update(cx, |field, cx| field.set_value(mapping.clone(), cx))
                    }
                    _ => (),
                }
                cell.value = value;
            }
        }
        return;
    }
    let mut subscriptions = Vec::new();
    let name = input_name.map(|name| {
        let field = cx.new(|cx| {
            let mut field = luma_ui::text_input::TextInput::search("Input name", cx);
            field.set_text(name, cx);
            field
        });
        subscriptions.push(cx.subscribe(&field, |_: &mut Luma, _, _, cx| cx.notify()));
        let app = cx.entity().downgrade();
        let name_field = field.clone();
        let target = target.clone();
        let key = input_key.unwrap().to_string();
        subscriptions.push(
            window.on_focus_out(&field.focus_handle(cx), cx, move |_, _, cx| {
                let name = name_field.read(cx).text().to_string();
                app.update(cx, |this, cx| {
                    this.set_graph_input_name(&target, &key, name, cx)
                })
                .ok();
            }),
        );
        field
    });
    let mut cells = Vec::new();
    for (id, spec) in &specs {
        let binding = bindings.get(id).cloned();
        let value = resolved_value(spec, binding.as_ref());
        let node_id = node_id.clone();
        let input = id.clone();
        let target = target.clone();
        let widget = match &value {
            Some(value) if value.scalar_value().is_some() => {
                let v = value.scalar_value().unwrap();
                let kind = spec.value_type;
                let unit = kind.signal_type().and_then(|s| s.unit);
                let field = cx.new(|cx| {
                    DraftedNumber::new(
                        spec.name.clone(),
                        v,
                        if matches!(unit, Some(p::Unit::Proportion | p::Unit::Beats)) {
                            0.
                        } else {
                            -1e9
                        },
                        if unit == Some(p::Unit::Proportion) {
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
                        if let Ok(value) =
                            luma_lib::node_graph::lighting::decode(kind, &serde_json::json!(value))
                        {
                            this.set_graph_input_value(&target, &node_id, &input, value, cx);
                        }
                    },
                ));
                Widget::Number(field)
            }
            Some(p::Value::Seed(seed)) => {
                let field = cx.new(|cx| {
                    DraftedNumber::new(spec.name.clone(), *seed, 0, u64::MAX, 260., window, cx)
                });
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &NumberEvent<u64>, cx| {
                        let NumberEvent::Committed(value) = *event;
                        this.set_graph_input_value(
                            &target,
                            &node_id,
                            &input,
                            p::Value::Seed(value),
                            cx,
                        );
                    },
                ));
                Widget::Seed(field)
            }
            Some(p::Value::Signal(signal)) => {
                let field = cx.new(|cx| {
                    SignalEditor::new(spec.name.clone(), signal.clone(), 260., window, cx)
                });
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &SignalChanged, cx| {
                        this.set_graph_input_value(
                            &target,
                            &node_id,
                            &input,
                            p::Value::Signal(event.0.clone()),
                            cx,
                        );
                    },
                ));
                Widget::Signal(field)
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
                let field =
                    cx.new(|cx| GradientEditor::new(display_gradient(gradient), window, cx));
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &GradientChanged, cx| {
                        let stops = event
                            .0
                            .stops()
                            .iter()
                            .map(|stop| p::ColorStop {
                                alpha: f64::from(stop.color.a),
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
            Some(p::Value::Mapping(mapping)) => {
                let field = cx.new(|cx| {
                    MappingEditor::new(spec.name.clone(), mapping.clone(), 260., window, cx)
                });
                subscriptions.push(cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &MappingChanged, cx| {
                        this.set_graph_input_value(
                            &target,
                            &node_id,
                            &input,
                            p::Value::Mapping(event.0.clone()),
                            cx,
                        );
                    },
                ));
                Widget::Mapping(field)
            }
            Some(
                p::Value::Boundary(_)
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
        name,
        _subscriptions: subscriptions,
    });
}

impl Luma {
    fn set_graph_input_name(
        &mut self,
        target: &Target,
        key: &str,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let Some(id) = editor.edited_definition() else {
            return;
        };
        let source = &editor.source;
        let Some(parent) = source.library.definitions.get(&id) else {
            return;
        };
        let p::Body::Graph(graph) = &parent.body else {
            return;
        };
        let current = parent
            .inputs
            .get(key)
            .map(|spec| &spec.name)
            .or_else(|| graph.input_nodes.get(key).map(|node| &node.name));
        if current.is_none_or(|current| current == name.trim()) {
            return;
        }
        self.apply_score_graph_edit(
            target,
            p::GraphEdit::RenameInput {
                key: key.into(),
                name,
            },
            cx,
        );
    }

    pub(crate) fn graph_input_name_key(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.active_graph_target() else {
            return;
        };
        let Some(TabBody::Graph(editor)) = self.workspace.body(&target) else {
            return;
        };
        let source = &editor.source;
        let Some(controls) = &source.controls else {
            return;
        };
        let Some(key) =
            luma_lib::node_graph::lighting::input_node_key(&controls.node).map(str::to_string)
        else {
            return;
        };
        let Some(field) = controls.name.clone() else {
            return;
        };
        if commit {
            self.set_graph_input_name(&target, &key, field.read(cx).text().to_string(), cx);
        } else {
            let parent = &source.library.definitions[&controls.definition];
            let p::Body::Graph(graph) = &parent.body else {
                return;
            };
            let name = parent
                .inputs
                .get(&key)
                .map(|spec| &spec.name)
                .or_else(|| graph.input_nodes.get(&key).map(|node| &node.name));
            if let Some(name) = name {
                field.update(cx, |field, cx| field.set_text(name.clone(), cx));
            }
        }
        self.focus.focus(window, cx);
    }
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
        if let Some(key) = luma_lib::node_graph::lighting::input_node_key(node) {
            self.apply_score_graph_edit(
                target,
                p::GraphEdit::Default {
                    key: key.into(),
                    value,
                },
                cx,
            );
            return;
        }
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let Some(definition) = editor.edited_definition() else {
            return;
        };
        let source = &editor.source;
        let Some(p::Definition {
            body: p::Body::Graph(graph),
            ..
        }) = source.library.definitions.get(&definition)
        else {
            return;
        };
        let Some(child) = graph.nodes.get(node) else {
            return;
        };
        // A widget can lose focus after its port is connected. Such a late
        // edit must neither replace the wire nor change its source's default.
        if matches!(
            child.inputs.get(input),
            Some(p::Binding::Input { .. } | p::Binding::Connection { .. })
        ) {
            return;
        }
        self.apply_score_graph_edit(
            target,
            p::GraphEdit::Bind {
                node: node.into(),
                input: input.into(),
                binding: Some(value.into()),
            },
            cx,
        );
    }
}

pub(super) fn add_button(editor: &Editor, app: &Entity<Luma>) -> Option<AnyElement> {
    if editor.inspecting_builtin() {
        return None;
    }
    let app = app.clone();
    let target = editor.target();
    Some(
        luma_ui::button("Add node", Enabled::Yes)
            .id("graph-add-node")
            .on_click(move |_, window, cx| {
                app.update(cx, |this, cx| {
                    this.open_graph_catalog(&target, window.mouse_position(), window, cx);
                });
            })
            .agent_node(Role::Button, "Add node")
            .into_any_element(),
    )
}

pub(super) fn panel(editor: &Editor, app: &Entity<Luma>) -> Option<AnyElement> {
    let source = &editor.source;
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
    if let Some((node, port)) = &source.selected_output {
        content = content.child(luma_ui::silkscreen(format!("{node}.{port}")));
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
    if let Some(name) = &controls.name {
        content = content.child(
            div()
                .key_context(crate::keymap::context::GRAPH_INPUT_NAME)
                .child(luma_ui::silkscreen("Name"))
                .child(
                    div()
                        .h(px(32.))
                        .child(name.clone())
                        .agent_node(Role::Input, "Input name"),
                ),
        );
        if controls.cells.is_empty() {
            content = content.child(luma_ui::silkscreen(
                "Connect to a parameter to choose its type and default",
            ));
        }
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
            Widget::Signal(field) => row.child(field.clone()),
            Widget::Seed(field) => row.child(field.clone()),
            Widget::Color(field) => row.child(field.clone()),
            Widget::Envelope(field) => row.child(field.clone()),
            Widget::Gradient(field) => row.child(field.clone()),
            Widget::Mapping(field) => row.child(field.clone()),
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
                                let source = &mut editor.source;
                                source.choice_open =
                                    if source.choice_open.as_ref() == Some(&toggle_menu) {
                                        None
                                    } else {
                                        Some(toggle_menu.clone())
                                    };
                            })
                        });
                    },
                    move |selected, _, cx| {
                        if let Ok(value) = luma_lib::node_graph::lighting::decode(
                            kind,
                            &serde_json::json!(options[selected].id),
                        ) {
                            app.update(cx, |this, cx| {
                                this.edit_graph_tab(&target, cx, |editor| {
                                    let source = &mut editor.source;
                                    source.choice_open = None;
                                });
                                this.set_graph_input_value(&target, &node, &input, value, cx);
                            });
                        }
                    },
                ))
            }
            Widget::Connection if matches!(cell.binding, Some(p::Binding::Input { .. })) => row,
            Widget::Connection => row.child(luma_ui::silkscreen(match &cell.binding {
                Some(p::Binding::Connection { node, output }) => format!("From {node}.{output}"),
                _ if matches!(cell.value, Some(p::Value::Events(p::Events::Automatic))) => {
                    "Uses Repeat · connect a trigger to replace it".into()
                }
                _ => format!("Connect a {} output", cell.spec.value_type),
            })),
        };
        if cell.spec.optional && cell.binding.is_none() {
            row = row.child(luma_ui::silkscreen("Not written"));
            if let Some(value) = &cell.value {
                let app = app.clone();
                let target = target.clone();
                let edit = p::GraphEdit::Bind {
                    node: controls.node.clone(),
                    input: cell.id.clone(),
                    binding: Some(value.clone().into()),
                };
                row = row.child(
                    luma_ui::button("Use value", Enabled::Yes)
                        .id(SharedString::from(format!(
                            "write-{}-{}",
                            controls.node, cell.id
                        )))
                        .on_click(move |_, _, cx| {
                            app.update(cx, |this, cx| {
                                this.apply_score_graph_edit(&target, edit.clone(), cx)
                            });
                        })
                        .agent_node(Role::Button, format!("Write {}", cell.spec.name)),
                );
            }
        }
        if controls.name.is_none() {
            if let Some(p::Binding::Input { input }) = &cell.binding {
                let app = app.clone();
                let target = target.clone();
                let id = luma_lib::node_graph::lighting::input_node_id(input);
                let label = source.library.definitions[&controls.definition].inputs[input]
                    .name
                    .clone();
                row = row.child(
                    luma_ui::button(&format!("Input · {label}"), Enabled::Yes)
                        .id(SharedString::from(format!(
                            "edit-input-{}-{}",
                            controls.node, cell.id
                        )))
                        .on_click(move |_, window, cx| {
                            app.update(cx, |this, cx| {
                                this.focus.focus(window, cx);
                                this.edit_graph_tab(&target, cx, |editor| {
                                    editor.selected = vec![id.clone().into()];
                                    editor.selected_edge = None;
                                });
                            });
                        })
                        .agent_node(Role::Button, format!("Edit Input {label}")),
                );
            } else if cell.binding.is_some() {
                let app = app.clone();
                let target = target.clone();
                let edit = p::GraphEdit::Bind {
                    node: controls.node.clone(),
                    input: cell.id.clone(),
                    binding: None,
                };
                let label = if matches!(cell.binding, Some(p::Binding::Connection { .. })) {
                    "Disconnect"
                } else if cell.spec.optional {
                    "Clear"
                } else {
                    "Reset"
                };
                row = row.child(
                    luma_ui::button(label, Enabled::Yes)
                        .id(SharedString::from(format!(
                            "reset-{}-{}",
                            controls.node, cell.id
                        )))
                        .on_click(move |_, _, cx| {
                            app.update(cx, |this, cx| {
                                this.apply_score_graph_edit(&target, edit.clone(), cx)
                            });
                        })
                        .agent_node(Role::Button, format!("{}: {label}", cell.spec.name)),
                );
            }
        }
        content = content.child(row);
    }
    Some(
        content
            .agent_node(Role::Card, "Node inspector")
            .into_any_element(),
    )
}

fn display_gradient(gradient: &p::Gradient) -> Gradient {
    Gradient::new(gradient.stops.iter().map(|stop| GradientStop {
        t: stop.t as f32,
        color: Rgba {
            r: stop.color[0] as f32,
            g: stop.color[1] as f32,
            b: stop.color[2] as f32,
            a: stop.alpha as f32,
        },
    }))
}
