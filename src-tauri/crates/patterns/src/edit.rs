//! Typed graph gestures shared by interactive editors and programmatic authors.
//! A draft may have unfinished wires. Saving/executing still validates the whole
//! score, so an incomplete gesture can never replace the installed show.
use crate::{Binding, Body, Definition, Error, Graph, Library, Node, Rate, Result, ValueType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphEdit {
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
    Expose {
        node: String,
        input: String,
        key: String,
        name: Option<String>,
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
        match edit {
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
            GraphEdit::Expose {
                node,
                input,
                key,
                name,
            } => {
                identity(&key)?;
                if next.inputs.contains_key(&key) {
                    return Err(Error(format!("graph input {key} already exists")));
                }
                let mut spec = input_spec(library, graph, &node, &input)?.clone();
                match graph.nodes[&node].inputs.get(&input) {
                    Some(Binding::Value { value }) => spec.default = Some(value.clone()),
                    Some(_) => {
                        return Err(Error("disconnect this input before exposing it".into()))
                    }
                    None => (),
                }
                if let Some(name) = name {
                    spec.name = name;
                }
                next.inputs.insert(key.clone(), spec);
                graph
                    .nodes
                    .get_mut(&node)
                    .unwrap()
                    .inputs
                    .insert(input, Binding::Input { input: key });
            }
            GraphEdit::Output { key, binding } => {
                identity(&key)?;
                let (value_type, rate) = binding_type(library, &next.inputs, graph, &binding)?;
                next.outputs
                    .insert(key.clone(), crate::Output { value_type, rate });
                graph.outputs.insert(key, binding);
            }
            GraphEdit::Default { key, value } => {
                value.validate()?;
                let input = next
                    .inputs
                    .get_mut(&key)
                    .ok_or_else(|| Error(format!("unknown graph input {key}")))?;
                if input.value_type != value.value_type() {
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
        self.inputs
            .retain(|id, _| !previously_used.contains(id) || used.contains(id));
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

fn identity(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(Error("identity must contain 1–256 printable bytes".into()));
    }
    Ok(())
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
    match binding {
        Binding::Value { value } => {
            value.validate()?;
            Ok((value.value_type(), Rate::Fixed))
        }
        Binding::Input { input } => inputs
            .get(input)
            .map(|i| (i.value_type, i.rate))
            .ok_or_else(|| Error(format!("unknown graph input {input}"))),
        Binding::Connection { node, output } => {
            let node = graph
                .nodes
                .get(node)
                .ok_or_else(|| Error(format!("unknown node {node}")))?;
            library
                .definitions
                .get(&node.definition)
                .and_then(|def| def.outputs.get(output))
                .map(|o| (o.value_type, o.rate))
                .ok_or_else(|| Error(format!("{}: unknown output {output}", node.definition)))
        }
    }
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
    if actual != expected {
        return Err(Error(format!("expected {expected:?}, got {actual:?}")));
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
                GraphEdit::Expose {
                    node: node.clone(),
                    input: "width".into(),
                    key: "pill_size".into(),
                    name: Some("Pill size".into()),
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
            .insert_effect(&base, "chase", "inner", 0., 4.)
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
                GraphEdit::Bind {
                    node: "effect".into(),
                    input: "width".into(),
                    binding: Some(Value::Proportion(0.3).into()),
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
                        definition: "add_lighting".into(),
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
                        output: "lighting".into(),
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
                        output: "lighting".into(),
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
