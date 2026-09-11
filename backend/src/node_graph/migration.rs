//! Storage-only conversion of historical standalone patterns. Numerical playback
//! uses the canonical graph after this boundary; old names resolve against v2.
use super::{lighting::decode, Edge, Graph, NodeInstance, PatternArgType};
use luma_patterns::{
    self as p, Binding, Body, Definition, Input, InputNode, Output, Rate, ValueType,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

mod numerical;

pub(crate) mod values;

pub(crate) fn pattern(source: &Graph, name: &str) -> Result<Option<p::Score>, String> {
    if source
        .nodes
        .iter()
        .all(|n| n.type_id == "pattern_args" || n.type_id.starts_with(super::lighting::PREFIX))
    {
        return typed_pattern(source, name).map(Some);
    }
    if numerical::supports(source) {
        return numerical::convert(source, name).map(Some);
    }
    Ok(None)
}

pub(crate) const ROOT: &str = "__score_pattern";

pub(crate) fn argument_value(
    arg_type: &PatternArgType,
    kind: ValueType,
    value: &serde_json::Value,
) -> Result<p::Value, String> {
    if *arg_type == PatternArgType::Scalar {
        decode(kind, value.get("value").unwrap_or(value))
    } else if *arg_type == PatternArgType::Palette {
        let colors = value["colors"].as_array().ok_or("Palette needs colors")?;
        let gradient = serde_json::json!({"stops": colors.iter().enumerate().map(|(i,c)| {
            serde_json::json!({"t": i as f64 / colors.len().saturating_sub(1).max(1) as f64, "color": c})
        }).collect::<Vec<_>>()});
        decode(kind, &gradient)
    } else {
        decode(kind, value)
    }
}

pub(crate) fn typed_pattern(source: &Graph, name: &str) -> Result<p::Score, String> {
    let defaults = source
        .args
        .iter()
        .map(|a| (a.id.clone(), a.default_value.clone()))
        .collect();
    let mut definition = typed_definition(&source.nodes, &source.edges, &defaults)?;
    definition.name = name.into();
    for arg in &source.args {
        if arg.arg_type == PatternArgType::Selection {
            continue;
        }
        if let Some(input) = definition.inputs.get_mut(&arg.id) {
            input.name = arg.name.clone();
        } else {
            // Preserve disconnected author controls as well as wired ones.
            let kind = match arg.arg_type {
                PatternArgType::Seed => ValueType::Seed,
                PatternArgType::AudioSource => ValueType::AudioSource,
                PatternArgType::Drum => ValueType::Drum,
                PatternArgType::Envelope => ValueType::Envelope,
                PatternArgType::Beats => ValueType::Beats,
                PatternArgType::Proportion => ValueType::Proportion,
                PatternArgType::Position => ValueType::Position,
                PatternArgType::Boolean => ValueType::Boolean,
                PatternArgType::Mapping => ValueType::Mapping,
                PatternArgType::Boundary => ValueType::Boundary,
                PatternArgType::Color => ValueType::Color,
                PatternArgType::Scalar => ValueType::Number,
                PatternArgType::Gradient | PatternArgType::Palette => ValueType::Gradient,
                PatternArgType::Selection => unreachable!(),
            };
            definition.inputs.insert(
                arg.id.clone(),
                Input {
                    optional: false,
                    name: arg.name.clone(),
                    description: String::new(),
                    value_type: kind,
                    rate: Rate::Fixed,
                    default: Some(argument_value(&arg.arg_type, kind, &arg.default_value)?),
                    author: None,
                },
            );
        }
    }
    let Body::Graph(graph) = &mut definition.body else {
        unreachable!()
    };
    let origin = source
        .nodes
        .iter()
        .find(|n| n.type_id == "pattern_args")
        .map(|n| [n.position_x.unwrap_or(0.), n.position_y.unwrap_or(0.)])
        .unwrap_or([0., 0.]);
    for (i, (id, input)) in definition.inputs.iter().enumerate() {
        graph.input_nodes.insert(
            id.clone(),
            InputNode {
                name: input.name.clone(),
                position: Some([origin[0], origin[1] + i as f64 * 68.]),
            },
        );
    }
    p::migration::upgrade_v2_definition(ROOT, definition).map_err(|e| e.to_string())
}

/// Infer old exposed types from destinations, preserving defaults as bindings.
/// The standalone playback adapter supplies its resolved defaults/overrides here.
pub(crate) fn typed_definition(
    nodes: &[NodeInstance],
    edges: &[Edge],
    defaults: &HashMap<String, serde_json::Value>,
) -> Result<Definition, String> {
    let library = p::migration::v2_library();
    let mut graph = p::Graph::default();
    let mut inputs = BTreeMap::<String, Input>::new();
    let mut output = None;
    let mut consumed = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for node in nodes {
        if !seen.insert(&node.id) {
            return Err(format!("Duplicate node {}", node.id));
        }
        if node.type_id == "pattern_args" {
            continue;
        }
        let name = node
            .type_id
            .strip_prefix(super::lighting::PREFIX)
            .ok_or_else(|| {
                format!(
                    "Node {} ({}) requires legacy graph conversion",
                    node.id, node.type_id
                )
            })?;
        let def = library
            .definitions
            .get(name)
            .ok_or_else(|| format!("Unknown historical node {name}"))?;
        for key in node.params.keys() {
            if !def.inputs.contains_key(key)
                && !(key == "selection" && p::Selection::from_value(&node.params[key]).is_some())
            {
                return Err(format!("Unknown input {}.{key}", node.id));
            }
        }
        let mut bindings = BTreeMap::new();
        for (id, input) in &def.inputs {
            let feeding: Vec<_> = edges
                .iter()
                .filter(|e| e.to_node == node.id && e.to_port == *id)
                .collect();
            if feeding.len() > 1 {
                return Err(format!("{} has more than one connection to {id}", node.id));
            }
            let binding = if let Some(edge) = feeding.first() {
                consumed.insert(edge.id.as_str());
                let source = nodes
                    .iter()
                    .find(|n| n.id == edge.from_node)
                    .ok_or("Connection source is missing")?;
                if source.type_id == "pattern_args" {
                    let value = defaults
                        .get(&edge.from_port)
                        .ok_or_else(|| format!("Missing exposed input {}", edge.from_port))?;
                    let default = decode(input.value_type, value)?;
                    if let Some(previous) = inputs.get_mut(&edge.from_port) {
                        if previous.value_type != input.value_type {
                            return Err(format!(
                                "Exposed input {} has incompatible destinations",
                                edge.from_port
                            ));
                        }
                        if input.rate == Rate::Fixed {
                            previous.rate = Rate::Fixed;
                        }
                    } else {
                        let mut exposed = input.clone();
                        exposed.optional = false;
                        exposed.name = edge.from_port.clone();
                        exposed.default = Some(default);
                        inputs.insert(edge.from_port.clone(), exposed);
                    }
                    Binding::Input {
                        input: edge.from_port.clone(),
                    }
                } else {
                    Binding::Connection {
                        node: edge.from_node.clone(),
                        output: edge.from_port.clone(),
                    }
                }
            } else if let Some(value) = node.params.get(id) {
                Binding::Value {
                    value: decode(input.value_type, value)?,
                }
            } else {
                continue;
            };
            bindings.insert(id.clone(), binding);
        }
        if let Some(port) = def.lighting_output() {
            if !edges
                .iter()
                .any(|e| e.from_node == node.id && e.from_port == port)
            {
                if output.is_some() {
                    return Err("Historical pattern has more than one terminal output".into());
                }
                output = Some((node.id.clone(), port.to_owned()));
            }
        }
        graph.nodes.insert(
            node.id.clone(),
            p::Node {
                position: node.position_x.zip(node.position_y).map(|(x, y)| [x, y]),
                definition: name.into(),
                inputs: bindings,
            },
        );
    }
    for edge in edges {
        if consumed.contains(edge.id.as_str()) {
            continue;
        }
        let selection = edge.to_port == "selection"
            && nodes
                .iter()
                .any(|n| n.id == edge.from_node && n.type_id == "pattern_args")
            && nodes.iter().any(|n| n.id == edge.to_node)
            && defaults
                .get(&edge.from_port)
                .and_then(p::Selection::from_value)
                .is_some();
        if !selection {
            return Err(format!(
                "Unknown connection {}.{} → {}.{}",
                edge.from_node, edge.from_port, edge.to_node, edge.to_port
            ));
        }
    }
    let (node, port) = output.ok_or("Historical pattern needs a terminal output")?;
    graph.outputs.insert(
        "lighting".into(),
        Binding::Connection { node, output: port },
    );
    Ok(Definition {
        name: "Score Pattern".into(),
        inputs,
        outputs: BTreeMap::from([(
            "lighting".into(),
            Output {
                value_type: ValueType::Lighting,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Graph(graph),
    })
}

/// Kahn's algorithm, ids sorted for deterministic order.
pub(crate) fn ordered_nodes<'a>(
    nodes: &'a [NodeInstance],
    edges: &[Edge],
) -> Result<Vec<&'a NodeInstance>, String> {
    let by_id: HashMap<&str, &NodeInstance> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    if by_id.len() != nodes.len() {
        return Err("Duplicate node id".into());
    }
    let mut edge_ids = BTreeSet::new();
    let mut indeg: HashMap<&str, usize> = nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        if !edge_ids.insert(&e.id) {
            return Err(format!("Duplicate connection {}", e.id));
        }
        if by_id.contains_key(e.from_node.as_str()) && by_id.contains_key(e.to_node.as_str()) {
            *indeg.get_mut(e.to_node.as_str()).unwrap() += 1;
            adj.entry(e.from_node.as_str())
                .or_default()
                .push(e.to_node.as_str());
        } else {
            return Err(format!("Connection {} references a missing node", e.id));
        }
    }
    let mut queue: Vec<&str> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| *k)
        .collect();
    queue.sort_unstable();
    let mut order = Vec::with_capacity(nodes.len());
    let mut qi = 0;
    while qi < queue.len() {
        let id = queue[qi];
        qi += 1;
        order.push(by_id[id]);
        if let Some(succ) = adj.get(id) {
            let mut s = succ.clone();
            s.sort_unstable();
            for m in s {
                let d = indeg.get_mut(m).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(m);
                }
            }
        }
    }
    if order.len() != nodes.len() {
        return Err("cycle in graph".into());
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_color_overrides_keep_explicit_and_embedded_opacity() {
        for (kind, value, expected) in [
            (
                PatternArgType::Gradient,
                serde_json::json!({"stops":[{"t":0.7,"color":[1.,0.,0.],"alpha":0.12345}]}),
                0.12345,
            ),
            (
                PatternArgType::Palette,
                serde_json::json!({"colors":["#ff000033"]}),
                0.2,
            ),
            (
                PatternArgType::Palette,
                serde_json::json!({"colors":[{"r":255.,"g":0.,"b":0.,"a":0.3}]}),
                0.3,
            ),
        ] {
            let p::Value::Gradient(gradient) =
                argument_value(&kind, ValueType::Gradient, &value).unwrap()
            else {
                panic!()
            };
            assert_eq!(gradient.stops.len(), 1);
            for stop in &gradient.stops {
                assert_eq!(stop.color, [1., 0., 0.]);
                assert!((stop.alpha - expected).abs() < 1e-7);
            }
            for at in [-1., 0., 0.5, 1., 2.] {
                assert_eq!(gradient.sample(at), [1., 0., 0.]);
                assert!((gradient.sample_alpha(at) - expected).abs() < 1e-7);
            }
        }
    }

    #[test]
    fn every_standalone_typed_effect_keeps_its_controls_and_explicit_output() {
        let old = p::migration::v2_library();
        let mut count = 0;
        for (id, definition) in &old.definitions {
            if !definition.playable() {
                continue;
            }
            let Ok(graph) = crate::node_graph::lighting::pattern(id) else {
                continue;
            };
            let converted = typed_pattern(&graph, &format!("Authored {id}"))
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            let root = &converted.definitions[ROOT];
            assert_eq!(root.name, format!("Authored {id}"));
            assert!(root.playable());
            let Body::Graph(body) = &root.body else {
                panic!()
            };
            // Controls the current engine no longer has (flicker, reseeding)
            // leave the interface; every remaining input is an authored control.
            assert_eq!(body.input_nodes.len(), root.inputs.len());
            assert!(!root.inputs.is_empty(), "{id}");
            for (key, input) in &root.inputs {
                let arg = graph
                    .args
                    .iter()
                    .find(|a| a.id == *key)
                    .unwrap_or_else(|| panic!("{id}: input {key} is not a typed control"));
                assert_ne!(arg.arg_type, PatternArgType::Selection);
                assert_eq!(input.name, arg.name);
                assert_eq!(body.input_nodes[key].name, arg.name);
            }
            converted.validate(&p::standard_library()).unwrap();
            count += 1;
        }
        assert!(count >= 18, "only checked {count} historical effects");
    }

    #[test]
    fn shared_and_disconnected_controls_survive_with_labels() {
        let mut graph = crate::node_graph::lighting::pattern("chase").unwrap();
        let start = graph.args.iter_mut().find(|a| a.id == "start").unwrap();
        start.name = "Launch position".into();
        let original = graph
            .edges
            .iter()
            .find(|e| e.from_port == "start")
            .unwrap()
            .clone();
        graph.edges.retain(|e| e.to_port != "end");
        graph.edges.push(Edge {
            id: "share".into(),
            to_port: "end".into(),
            ..original
        });
        let converted = typed_pattern(&graph, "My Chase").unwrap();
        let root = &converted.definitions[ROOT];
        assert_eq!(root.inputs["start"].name, "Launch position");
        assert!(
            root.inputs.contains_key("end"),
            "disconnected control was lost"
        );
        // The effect opens into its composition; the chase reads both.
        let Body::Graph(body) = &root.body else {
            panic!()
        };
        for port in ["start", "end"] {
            assert_eq!(
                body.nodes["chase"].inputs[port],
                Binding::Input {
                    input: "start".into()
                }
            );
        }
    }
}
