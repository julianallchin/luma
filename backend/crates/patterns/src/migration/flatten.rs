//! A placed effect is its composition. The version 2 conversion left every
//! score with copies of the library effects it used, called from thin clip
//! wrappers and split into a hue and a brightness on the way out. Each copy
//! dissolves into the clip that calls it, the split collapses back into one
//! Mask color, and a shipped pattern called from a clip opens up the same way.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// The migration's copies carry this in their identity; authors never do.
/// A canvas conversion prefixes the copy with the node it came from.
const COPY: &str = "/signals";
fn stem(id: &str) -> Option<&str> {
    let (prefix, _) = id.split_once(COPY)?;
    prefix.rsplit('/').next()
}

/// Mask color was colour × mask before one Multiply took every signal.
pub(super) fn retire(score: &mut Score) {
    for definition in score.definitions.values_mut() {
        let Body::Graph(graph) = &mut definition.body else {
            continue;
        };
        let mut retired = BTreeSet::new();
        for (id, node) in &mut graph.nodes {
            if node.definition == "mask_color" {
                node.definition = "core/multiply".into();
                if let Some(color) = node.inputs.remove("color") {
                    node.inputs.insert("a".into(), color);
                }
                if let Some(mask) = node.inputs.remove("mask") {
                    node.inputs.insert("b".into(), mask);
                }
                retired.insert(id.clone());
            }
        }
        for binding in graph
            .nodes
            .values_mut()
            .flat_map(|node| node.inputs.values_mut())
            .chain(graph.outputs.values_mut())
        {
            if let Binding::Connection { node, output } = binding {
                if output == "color" && retired.contains(node) {
                    *output = "value".into();
                }
            }
        }
    }
}

pub fn flatten(score: &mut Score) -> Result<()> {
    let library = standard_library();
    adopt_shipped_helpers(&mut score.definitions, &library);
    // Copies of the version 2 library; a copy of an authored helper stays
    // the node the author made.
    let historical = super::v2_library();
    let copies: BTreeSet<String> = score
        .definitions
        .keys()
        .filter(|id| stem(id).is_some_and(|stem| historical.definitions.contains_key(stem)))
        .cloned()
        .collect();
    let referenced_before = referenced(&score.definitions, &score.clips);
    let clips: Vec<String> = score
        .definitions
        .iter()
        .filter(|(_, definition)| definition.playable())
        .map(|(id, _)| id.clone())
        .collect();
    for id in clips {
        let mut definition = score.definitions[&id].clone();
        let Body::Graph(graph) = &mut definition.body else {
            continue;
        };
        open_up(graph, &score.definitions, &library, &copies)?;
        collapse_splits(graph);
        score.definitions.insert(id, definition);
    }
    // A copy nothing calls any more was only ever the migration's.
    loop {
        let now = referenced(&score.definitions, &score.clips);
        let before = score.definitions.len();
        score.definitions.retain(|id, _| {
            !(copies.contains(id) && referenced_before.contains(id) && !now.contains(id))
        });
        if score.definitions.len() == before {
            return Ok(());
        }
    }
}

fn referenced(
    definitions: &BTreeMap<String, Definition>,
    clips: &BTreeMap<String, Clip>,
) -> BTreeSet<String> {
    definitions
        .values()
        .filter_map(|definition| match &definition.body {
            Body::Graph(graph) => Some(graph.nodes.values()),
            Body::Primitive(_) => None,
        })
        .flatten()
        .map(|node| node.definition.clone())
        .chain(clips.values().map(|clip| clip.graph.clone()))
        .collect()
}

/// A copy that computes exactly what a shipped graph computes is that graph.
fn adopt_shipped_helpers(definitions: &mut BTreeMap<String, Definition>, library: &Library) {
    loop {
        let adopted: BTreeMap<String, String> = definitions
            .iter()
            .filter_map(|(id, copy)| {
                let stem = stem(id)?;
                let shipped = library.definitions.get(stem)?;
                same_shape(copy, shipped).then(|| (id.clone(), stem.to_owned()))
            })
            .collect();
        if adopted.is_empty() {
            return;
        }
        for definition in definitions.values_mut() {
            let Body::Graph(graph) = &mut definition.body else {
                continue;
            };
            for node in graph.nodes.values_mut() {
                if let Some(shipped) = adopted.get(&node.definition) {
                    node.definition = shipped.clone();
                }
            }
        }
        definitions.retain(|id, _| !adopted.contains_key(id));
    }
}

/// Saved graphs may still spell the field samplers by their old names; both
/// run the ordinary sampler's kernel.
fn modern(graph: &Graph) -> Graph {
    let mut graph = graph.clone();
    let mut envelopes = BTreeSet::new();
    for (id, node) in &mut graph.nodes {
        match node.definition.as_str() {
            "sample_field_envelope" => {
                node.definition = "envelope".into();
                if let Some(phase) = node.inputs.remove("phase") {
                    node.inputs.insert("progress".into(), phase);
                }
                envelopes.insert(id.clone());
            }
            "sample_field_gradient" => node.definition = "sample_gradient".into(),
            _ => {}
        }
    }
    for binding in graph
        .nodes
        .values_mut()
        .flat_map(|node| node.inputs.values_mut())
        .chain(graph.outputs.values_mut())
    {
        if let Binding::Connection { node, output } = binding {
            if output == "mask" && envelopes.contains(node) {
                *output = "value".into();
            }
        }
    }
    graph
}

fn same_shape(a: &Definition, b: &Definition) -> bool {
    let interface = |definition: &Definition| {
        (
            definition
                .inputs
                .iter()
                .map(|(key, input)| (key.clone(), input.value_type, input.default.clone()))
                .collect::<Vec<_>>(),
            definition.outputs.clone(),
        )
    };
    if interface(a) != interface(b) {
        return false;
    }
    match (&a.body, &b.body) {
        (Body::Graph(x), Body::Graph(y)) => {
            let x = modern(x);
            x.outputs == y.outputs
                && x.nodes.len() == y.nodes.len()
                && x.nodes.iter().all(|(id, node)| {
                    y.nodes.get(id).is_some_and(|other| {
                        node.definition == other.definition && node.inputs == other.inputs
                    })
                })
        }
        (x, y) => x == y,
    }
}

/// Inline the effect copies and shipped patterns a clip calls, and through
/// an effect copy the copies it called that carry events or merely rename;
/// a mask helper stays a node.
fn open_up(
    graph: &mut Graph,
    definitions: &BTreeMap<String, Definition>,
    library: &Library,
    copies: &BTreeSet<String>,
) -> Result<()> {
    let historical = super::v2_library();
    let effect = |id: &str| {
        copies.contains(id)
            && stem(id).is_some_and(|stem| {
                historical.definitions.get(stem).is_some_and(|original| {
                    matches!(original.body, Body::Graph(_))
                        && original.playable()
                        && original
                            .inputs
                            .values()
                            .all(|input| input.value_type != ValueType::Lighting)
                })
            })
    };
    let events = |id: &str| {
        copies.contains(id)
            && definitions.get(id).is_some_and(|copy| match &copy.body {
                Body::Graph(body) => {
                    body.nodes.len() <= 1
                        || body.nodes.values().any(|node| {
                            matches!(node.definition.as_str(), "beat_trigger" | "drum_trigger")
                        })
                }
                Body::Primitive(_) => false,
            })
    };
    let pattern = |id: &str| {
        library
            .definitions
            .get(id)
            .is_some_and(|shipped| shipped.placeable() && !shipped.playable())
    };
    let mut pending: Vec<String> = graph
        .nodes
        .iter()
        .filter(|(_, node)| effect(&node.definition) || pattern(&node.definition))
        .map(|(id, _)| id.clone())
        .collect();
    let mut opened = 0;
    while let Some(node) = pending.pop() {
        let Some(call) = graph.nodes.get(&node) else {
            continue;
        };
        let callee = definitions
            .get(&call.definition)
            .or_else(|| library.definitions.get(&call.definition))
            .ok_or_else(|| Error(format!("unknown definition {}", call.definition)))?;
        if !matches!(callee.body, Body::Graph(_)) {
            continue;
        }
        opened += 1;
        if opened > crate::graph::MAX_GRAPH_NODES {
            return Err(Error("flattening does not settle".into()));
        }
        let before: BTreeSet<String> = graph.nodes.keys().cloned().collect();
        graph.inline(&node, callee)?;
        pending.extend(
            graph
                .nodes
                .iter()
                .filter(|(id, node)| {
                    !before.contains(*id) && (effect(&node.definition) || events(&node.definition))
                })
                .map(|(id, _)| id.clone()),
        );
    }
    Ok(())
}

/// A colour split into hue and peak brightness, read back together by a
/// Multiply or an Apply, is the colour.
fn collapse_splits(graph: &mut Graph) {
    let wire = |node: &str, output: &str| Binding::Connection {
        node: node.into(),
        output: output.into(),
    };
    let number = |value: f64| Binding::from(Value::Number(value));
    let mut splits = Vec::new();
    for (hue, choose) in &graph.nodes {
        if choose.definition != "core/choose"
            || choose.inputs.get("no") != Some(&Binding::from(Value::Color([0.0; 3])))
        {
            continue;
        }
        let (
            Some(Binding::Connection {
                node: lit,
                output: lit_out,
            }),
            Some(Binding::Connection {
                node: ratio,
                output: ratio_out,
            }),
        ) = (choose.inputs.get("condition"), choose.inputs.get("yes"))
        else {
            continue;
        };
        let (Some(lit_node), Some(ratio_node)) = (graph.nodes.get(lit), graph.nodes.get(ratio))
        else {
            continue;
        };
        let Some(Binding::Connection {
            node: peak,
            output: peak_out,
        }) = ratio_node.inputs.get("b")
        else {
            continue;
        };
        let Some(peak_node) = graph.nodes.get(peak) else {
            continue;
        };
        let source = ratio_node.inputs.get("a");
        if lit_out == "mask"
            && ratio_out == "value"
            && peak_out == "value"
            && lit_node.definition == "core/greater"
            && ratio_node.definition == "core/divide"
            && peak_node.definition == "core/channel_maximum"
            && lit_node.inputs.get("a") == Some(&wire(peak, "value"))
            && lit_node.inputs.get("b") == Some(&number(1e-5))
            && lit_node.inputs.get("tolerance") == Some(&number(0.0))
            && source.is_some()
            && peak_node.inputs.get("value") == source
        {
            splits.push((
                hue.clone(),
                lit.clone(),
                ratio.clone(),
                peak.clone(),
                source.unwrap().clone(),
            ));
        }
    }
    for (hue, lit, ratio, peak, source) in splits {
        let (hue_wire, peak_wire) = (wire(&hue, "value"), wire(&peak, "value"));
        let pairs: Vec<String> = graph
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.definition == "core/multiply"
                    && node.inputs.get("a") == Some(&hue_wire)
                    && node.inputs.get("b") == Some(&peak_wire)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for pair in pairs {
            graph.nodes.remove(&pair);
            let pair_wire = wire(&pair, "value");
            for binding in graph
                .nodes
                .values_mut()
                .flat_map(|node| node.inputs.values_mut())
                .chain(graph.outputs.values_mut())
            {
                if *binding == pair_wire {
                    *binding = source.clone();
                }
            }
        }
        for apply in graph
            .nodes
            .values_mut()
            .filter(|node| node.definition == "output")
        {
            if apply.inputs.get("color") == Some(&hue_wire)
                && apply.inputs.get("dimmer") == Some(&peak_wire)
            {
                apply.inputs.remove("dimmer");
                apply.inputs.insert("color".into(), source.clone());
            }
        }
        for id in [hue, lit, ratio, peak] {
            let read = graph
                .nodes
                .iter()
                .filter(|(other, _)| **other != id)
                .flat_map(|(_, node)| node.inputs.values())
                .chain(graph.outputs.values())
                .any(|binding| matches!(binding, Binding::Connection { node, .. } if *node == id));
            if !read {
                graph.nodes.remove(&id);
            }
        }
    }
}
