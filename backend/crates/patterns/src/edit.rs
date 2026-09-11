//! Typed graph gestures shared by interactive editors and programmatic authors.
//! A draft may have unfinished wires. Saving/executing still validates the whole
//! score, so an incomplete gesture can never replace the installed show.
use crate::graph::{identity, MAX_GRAPH_NODES};
use crate::{Binding, Body, Definition, Error, Graph, Library, Node, Rate, Result, ValueType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphEdit {
    AddInput {
        key: String,
        name: String,
        position: [f64; 2],
    },
    RenameInput {
        key: String,
        name: String,
    },
    RemoveInput {
        key: String,
    },
    MoveInputs {
        positions: BTreeMap<String, [f64; 2]>,
    },
    Move {
        positions: BTreeMap<String, [f64; 2]>,
    },
    Add {
        id: String,
        definition: String,
    },
    Remove {
        id: String,
    },
    Bind {
        node: String,
        input: String,
        binding: Option<Binding>,
    },
    Output {
        key: String,
        binding: Binding,
    },
    Default {
        key: String,
        value: crate::Value,
    },
    Name {
        name: String,
    },
}

impl Definition {
    /// Mutate atomically; a refused operation leaves even a draft unchanged.
    /// Type/rate mistakes are rejected at the wire, before full-score checking.
    pub fn edit(&mut self, library: &Library, edit: GraphEdit) -> Result<()> {
        let previously_used = self.referenced_inputs();
        let mut next = self.clone();
        let Body::Graph(graph) = &mut next.body else {
            return Err(Error("fundamental operations are immutable".into()));
        };
        // Older documents already have formal parameters. Give those the
        // same editable socket identity as newly created Inputs.
        for (key, spec) in &next.inputs {
            graph
                .input_nodes
                .entry(key.clone())
                .or_insert_with(|| crate::InputNode {
                    name: spec.name.clone(),
                    position: None,
                });
        }
        if graph.nodes.len() > MAX_GRAPH_NODES {
            return Err(Error(format!(
                "a graph supports at most {MAX_GRAPH_NODES} nodes"
            )));
        }
        match edit {
            GraphEdit::AddInput {
                key,
                name,
                position,
            } => {
                identity(&key)?;
                if graph.input_nodes.contains_key(&key) {
                    return Err(Error(format!("Input {key} already exists")));
                }
                if graph.input_nodes.len() >= MAX_GRAPH_NODES {
                    return Err(Error(format!(
                        "a graph supports at most {MAX_GRAPH_NODES} Inputs"
                    )));
                }
                let node = crate::InputNode {
                    name: name.trim().into(),
                    position: Some(position),
                };
                node.validate()?;
                graph.input_nodes.insert(key, node);
            }
            GraphEdit::RenameInput { key, name } => {
                let node = graph
                    .input_nodes
                    .get_mut(&key)
                    .ok_or_else(|| Error(format!("unknown Input {key}")))?;
                node.name = name.trim().into();
                node.validate()?;
                if let Some(spec) = next.inputs.get_mut(&key) {
                    spec.name = node.name.clone();
                }
            }
            GraphEdit::RemoveInput { key } => {
                if graph.input_nodes.remove(&key).is_none() {
                    return Err(Error(format!("unknown Input {key}")));
                }
                next.inputs.remove(&key);
                for node in graph.nodes.values_mut() {
                    node.inputs.retain(
                        |_, binding| !matches!(binding, Binding::Input { input } if input == &key),
                    );
                }
                graph.outputs.retain(
                    |_, binding| !matches!(binding, Binding::Input { input } if input == &key),
                );
            }
            GraphEdit::MoveInputs { positions } => {
                for (key, position) in positions {
                    let node = graph
                        .input_nodes
                        .get_mut(&key)
                        .ok_or_else(|| Error(format!("unknown Input {key}")))?;
                    node.position = Some(position);
                    node.validate()?;
                }
            }
            GraphEdit::Move { positions } => {
                for (id, position) in positions {
                    if position.iter().any(|value| !value.is_finite()) {
                        return Err(Error("node position must be finite".into()));
                    }
                    graph
                        .nodes
                        .get_mut(&id)
                        .ok_or_else(|| Error(format!("unknown node {id}")))?
                        .position = Some(position);
                }
            }
            GraphEdit::Add { id, definition } => {
                identity(&id)?;
                if graph.nodes.contains_key(&id) {
                    return Err(Error(format!("node {id} already exists")));
                }
                if !library.definitions.contains_key(&definition) {
                    return Err(Error(format!("unknown definition {definition}")));
                }
                if graph.nodes.len() >= MAX_GRAPH_NODES {
                    return Err(Error(format!(
                        "a graph supports at most {MAX_GRAPH_NODES} nodes"
                    )));
                }
                let child = &library.definitions[&definition];
                if child.body == Body::Primitive(crate::Primitive::Output) {
                    if graph.nodes.values().any(|node| {
                        library.definitions[&node.definition].body
                            == Body::Primitive(crate::Primitive::Output)
                    }) {
                        return Err(Error("this graph already has an Output".into()));
                    }
                    next.outputs = child.outputs.clone();
                    graph.outputs = BTreeMap::from([(
                        "lighting".into(),
                        Binding::Connection {
                            node: id.clone(),
                            output: "lighting".into(),
                        },
                    )]);
                }
                graph.nodes.insert(
                    id,
                    Node {
                        position: None,
                        definition,
                        inputs: BTreeMap::new(),
                    },
                );
            }
            GraphEdit::Remove { id } => {
                if graph.nodes.remove(&id).is_none() {
                    return Err(Error(format!("unknown node {id}")));
                }
                for node in graph.nodes.values_mut() {
                    node.inputs
                        .retain(|_, b| !matches!(b, Binding::Connection {node, ..} if node == &id));
                }
                graph
                    .outputs
                    .retain(|_, b| !matches!(b, Binding::Connection {node, ..} if node == &id));
            }
            GraphEdit::Bind {
                node,
                input,
                binding,
            } => {
                let target = input_spec(library, graph, &node, &input)?;
                if let Some(Binding::Input { input: key }) = &binding {
                    if !next.inputs.contains_key(key) {
                        let source = graph
                            .input_nodes
                            .get(key)
                            .ok_or_else(|| Error(format!("unknown Input {key}")))?;
                        let mut spec = target.clone();
                        spec.optional = false;
                        spec.name = source.name.clone();
                        match graph.nodes[&node].inputs.get(&input) {
                            Some(Binding::Value { value }) => spec.default = Some(value.clone()),
                            Some(Binding::Input { input }) => {
                                spec.default = next
                                    .inputs
                                    .get(input)
                                    .ok_or_else(|| Error(format!("unknown Input {input}")))?
                                    .default
                                    .clone();
                            }
                            _ => (),
                        }
                        if spec.default.is_none() {
                            spec.default = spec.value_type.signal_default();
                        }
                        if let Some(value) = &spec.default {
                            if let ValueType::Signal(kind) = &mut spec.value_type {
                                let actual = value.value_type().signal_type().ok_or_else(|| {
                                    Error("a signal Input needs a numerical default".into())
                                })?;
                                kind.unit = kind.unit.or(actual.unit);
                                kind.channels = kind.channels.or(actual.channels);
                            } else if matches!(
                                spec.value_type,
                                ValueType::Field | ValueType::Mask | ValueType::ColorField
                            ) {
                                spec.value_type = value.value_type();
                            }
                        }
                        next.inputs.insert(key.clone(), spec);
                    }
                }
                if let Some(binding) = &binding {
                    compatible(
                        library,
                        &next.inputs,
                        graph,
                        binding,
                        target.value_type,
                        target.rate,
                    )?;
                }
                let inputs = &mut graph.nodes.get_mut(&node).unwrap().inputs;
                if let Some(binding) = binding {
                    inputs.insert(input, binding);
                } else {
                    inputs.remove(&input);
                }
            }
            GraphEdit::Output { key, binding } => {
                identity(&key)?;
                let (value_type, _) = binding_type(library, &next.inputs, graph, &binding)?;
                next.outputs.insert(
                    key.clone(),
                    crate::Output {
                        value_type,
                        // A currently constant output can become animated through
                        // later edits. Wire inference computes its current rate.
                        rate: crate::Rate::Frame,
                    },
                );
                graph.outputs.insert(key, binding);
            }
            GraphEdit::Default { key, value } => {
                value.validate()?;
                let input = next
                    .inputs
                    .get_mut(&key)
                    .ok_or_else(|| Error(format!("unknown graph input {key}")))?;
                if !input.value_type.accepts(value.value_type()) {
                    return Err(Error("graph default has the wrong type".into()));
                }
                input.default = Some(value);
            }
            GraphEdit::Name { name } => next.name = name,
        }
        let mut finished = std::collections::BTreeSet::new();
        for node in graph.nodes.keys() {
            crate::graph::check_cycle(
                graph,
                node,
                &mut std::collections::BTreeSet::new(),
                &mut finished,
            )?;
        }
        next.prune_disconnected_inputs(&previously_used);
        *self = next;
        Ok(())
    }
}

impl Definition {
    fn referenced_inputs(&self) -> std::collections::BTreeSet<String> {
        let Body::Graph(graph) = &self.body else {
            return Default::default();
        };
        graph
            .nodes
            .values()
            .flat_map(|node| node.inputs.values())
            .chain(graph.outputs.values())
            .filter_map(|binding| match binding {
                Binding::Input { input } => Some(input.clone()),
                _ => None,
            })
            .collect()
    }
    fn prune_disconnected_inputs(&mut self, previously_used: &std::collections::BTreeSet<String>) {
        let used = self.referenced_inputs();
        let Body::Graph(graph) = &self.body else {
            return;
        };
        self.inputs.retain(|id, _| {
            graph.input_nodes.contains_key(id) || !previously_used.contains(id) || used.contains(id)
        });
    }
}

impl crate::Score {
    /// Reconcile callers when a graph removes an exposed input. Clip overrides
    /// and parent bindings cannot keep controls the callee no longer accepts.
    /// Incomplete wiring is retained as a draft; validate before publication.
    pub fn edit_graph(&mut self, base: &Library, id: &str, edit: GraphEdit) -> Result<()> {
        let mut candidate = self.clone();
        let library = candidate.library(base)?;
        candidate
            .definitions
            .get_mut(id)
            .ok_or_else(|| Error("copy this built-in into the score before editing it".into()))?
            .edit(&library, edit)?;
        loop {
            let library = candidate.library(base)?;
            let mut removed = false;
            for definition in candidate.definitions.values_mut() {
                let previously_used = definition.referenced_inputs();
                if let Body::Graph(graph) = &mut definition.body {
                    for node in graph.nodes.values_mut() {
                        if let Some(child) = library.definitions.get(&node.definition) {
                            node.inputs.retain(|id, _| child.inputs.contains_key(id));
                        }
                    }
                }
                let before = definition.inputs.len();
                definition.prune_disconnected_inputs(&previously_used);
                removed |= before != definition.inputs.len();
            }
            if !removed {
                break;
            }
        }
        let library = candidate.library(base)?;
        for clip in candidate.clips.values_mut() {
            if let Some(definition) = library.definitions.get(&clip.graph) {
                clip.inputs
                    .retain(|id, _| definition.inputs.contains_key(id));
            }
        }
        *self = candidate;
        Ok(())
    }
}

fn input_spec<'a>(
    library: &'a Library,
    graph: &Graph,
    node: &str,
    input: &str,
) -> Result<&'a crate::Input> {
    let node = graph
        .nodes
        .get(node)
        .ok_or_else(|| Error(format!("unknown node {node}")))?;
    library
        .definitions
        .get(&node.definition)
        .and_then(|def| def.inputs.get(input))
        .ok_or_else(|| Error(format!("{}: unknown input {input}", node.definition)))
}
fn binding_type(
    library: &Library,
    inputs: &BTreeMap<String, crate::Input>,
    graph: &Graph,
    binding: &Binding,
) -> Result<(ValueType, Rate)> {
    library.binding_type(inputs, graph, binding)
}

fn compatible(
    library: &Library,
    inputs: &BTreeMap<String, crate::Input>,
    graph: &Graph,
    binding: &Binding,
    expected: ValueType,
    rate: Rate,
) -> Result<()> {
    let (actual, actual_rate) = binding_type(library, inputs, graph, binding)?;
    if !expected.accepts(actual) {
        return Err(Error(format!("expected {expected}, got {actual}")));
    }
    if rate == Rate::Fixed && actual_rate == Rate::Frame {
        return Err(Error(
            "frame-varying wire connected to a fixed input".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{standard_library, Value};
    #[test]
    fn exposed_defaults_and_refused_wires_preserve_the_authored_graph() {
        let library = standard_library();
        let mut definition = library.definitions["chase"].instance("chase");
        let Body::Graph(body) = &definition.body else {
            unreachable!()
        };
        let node = body.nodes.keys().next().unwrap().clone();
        definition
            .edit(
                &library,
                GraphEdit::Bind {
                    node: node.clone(),
                    input: "width".into(),
                    binding: Some(Value::Proportion(0.3).into()),
                },
            )
            .unwrap();
        definition
            .edit(
                &library,
                GraphEdit::AddInput {
                    key: "pill_size".into(),
                    name: "Pill size".into(),
                    position: [0., 0.],
                },
            )
            .unwrap();
        definition
            .edit(
                &library,
                GraphEdit::Bind {
                    node: node.clone(),
                    input: "width".into(),
                    binding: Some(Binding::Input {
                        input: "pill_size".into(),
                    }),
                },
            )
            .unwrap();
        assert_eq!(
            definition.inputs["pill_size"].default,
            Some(Value::Proportion(0.3))
        );
        let before = definition.clone();
        assert!(definition
            .edit(
                &library,
                GraphEdit::Bind {
                    node,
                    input: "width".into(),
                    binding: Some(Value::Beats(2.0).into())
                }
            )
            .is_err());
        assert_eq!(definition, before);
    }
    #[test]
    fn removing_a_node_keeps_a_repairable_draft_but_cannot_execute() {
        let mut library = standard_library();
        let mut definition = library.definitions["chase"].instance("chase");
        let Body::Graph(body) = &definition.body else {
            unreachable!()
        };
        let id = body.nodes.keys().next().unwrap().clone();
        definition.edit(&library, GraphEdit::Remove { id }).unwrap();
        library.definitions.insert("draft".into(), definition);
        assert!(library
            .validate("draft")
            .unwrap_err()
            .0
            .contains("missing output"));
    }
    #[test]
    fn removing_an_exposure_updates_nested_callers_and_clip_overrides() {
        let base = standard_library();
        let mut score = crate::Score::default();
        score
            .insert_effect(&base, "beat_chase", "inner", 0., 4.)
            .unwrap();
        let wrapper = score.definitions["inner"].instance("inner");
        score.definitions.insert("outer".into(), wrapper);
        let mut clip = score.clips["inner"].clone();
        clip.graph = "outer".into();
        clip.inputs.insert("width".into(), Value::Proportion(0.7));
        score.clips.insert("outer-clip".into(), clip);
        score
            .edit_graph(
                &base,
                "inner",
                GraphEdit::RemoveInput {
                    key: "width".into(),
                },
            )
            .unwrap();
        score.validate(&base).unwrap();
        assert!(!score.definitions["inner"].inputs.contains_key("width"));
        assert!(!score.definitions["outer"].inputs.contains_key("width"));
        assert!(!score.clips["outer-clip"].inputs.contains_key("width"));
        assert!(score.clips["outer-clip"].graph == "outer");
    }
    #[test]
    fn closing_a_wire_cycle_is_refused_without_destroying_the_draft() {
        let library = standard_library();
        let mut definition = library.definitions["chase"].instance("chase");
        for id in ["a", "b"] {
            definition
                .edit(
                    &library,
                    GraphEdit::Add {
                        id: id.into(),
                        definition: "core/add".into(),
                    },
                )
                .unwrap();
        }
        definition
            .edit(
                &library,
                GraphEdit::Bind {
                    node: "a".into(),
                    input: "a".into(),
                    binding: Some(Binding::Connection {
                        node: "b".into(),
                        output: "value".into(),
                    }),
                },
            )
            .unwrap();
        let before = definition.clone();
        let error = definition
            .edit(
                &library,
                GraphEdit::Bind {
                    node: "b".into(),
                    input: "a".into(),
                    binding: Some(Binding::Connection {
                        node: "a".into(),
                        output: "value".into(),
                    }),
                },
            )
            .unwrap_err();
        assert!(error.0.contains("cycle"));
        assert_eq!(definition, before);
    }
    #[test]
    fn rename_keeps_unrelated_interface_declarations() {
        let library = standard_library();
        let mut definition = library.definitions["chase"].instance("chase");
        definition
            .inputs
            .insert("reserved".into(), definition.inputs["width"].clone());
        let before = definition.inputs.clone();
        definition
            .edit(&library, GraphEdit::Name { name: "".into() })
            .unwrap();
        assert_eq!(definition.inputs, before);
    }
}
