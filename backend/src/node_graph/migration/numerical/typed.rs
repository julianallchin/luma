//! Import a historical typed call into a numerical graph. Capability bundles
//! exist only while resolving the old wires; the saved result contains signals.
use super::*;
use p::migration::{Capability, Expanded};

pub(super) struct Converted {
    pub node: Node,
    pub definitions: BTreeMap<String, Definition>,
    pub outputs: BTreeMap<String, Expanded<B>>,
    pub bundle_edges: BTreeSet<String>,
}

pub(super) fn lower(
    node: &NodeInstance,
    graph: &Graph,
    source: &mut impl FnMut(&str) -> Result<Option<B>, String>,
    bundles: &BTreeMap<(String, String), BTreeMap<Capability, B>>,
) -> Result<Converted, String> {
    let name = node
        .type_id
        .strip_prefix(crate::node_graph::lighting::PREFIX)
        .unwrap();
    let library = p::migration::v2_library();
    let definition = library
        .definitions
        .get(name)
        .ok_or_else(|| format!("Unknown historical node {name}"))?;
    for key in node.params.keys() {
        if !definition.inputs.contains_key(key)
            && !(key == "selection" && p::Selection::from_value(&node.params[key]).is_some())
        {
            return Err(format!("Unknown input {}.{key}", node.id));
        }
    }
    let mut arguments = BTreeMap::new();
    let mut shapes = BTreeMap::new();
    let mut bundle_edges = BTreeSet::new();
    for (key, input) in &definition.inputs {
        if input.value_type == ValueType::Lighting {
            let edges = graph
                .edges
                .iter()
                .filter(|e| e.to_node == node.id && e.to_port == *key)
                .collect::<Vec<_>>();
            if edges.len() != 1 {
                return Err(format!("{}.{key} needs one capability source", node.id));
            }
            let edge = edges[0];
            bundle_edges.insert(edge.id.clone());
            let caps = bundles
                .get(&(edge.from_node.clone(), edge.from_port.clone()))
                .ok_or_else(|| format!("{}.{key} has no capability source", node.id))?;
            shapes.insert(key.clone(), caps.keys().copied().collect());
            arguments.insert(key.clone(), Expanded::Bundle(caps.clone()));
        } else if let Some(binding) = source(key)? {
            arguments.insert(key.clone(), Expanded::Single(binding));
        } else if let Some(value) = node.params.get(key) {
            let value = crate::node_graph::lighting::decode(input.value_type, value)?;
            arguments.insert(key.clone(), Expanded::Single(value.into()));
        }
    }
    let upgraded = p::migration::upgrade_v2_node(name, shapes).map_err(|e| e.to_string())?;
    let remap: BTreeMap<_, _> = upgraded
        .definitions
        .keys()
        .map(|id| {
            (
                id.clone(),
                format!("node/{}:{}/{id}", node.id.len(), node.id),
            )
        })
        .collect();
    let mut bindings = BTreeMap::new();
    for (key, ports) in upgraded.inputs {
        let Some(value) = arguments.get(&key) else {
            continue;
        };
        match (ports, value) {
            (Expanded::Single(port), Expanded::Single(binding)) => {
                bindings.insert(port, binding.clone());
            }
            (Expanded::Bundle(ports), Expanded::Bundle(caps)) => {
                for (cap, port) in ports {
                    bindings.insert(port, caps[&cap].clone());
                }
            }
            _ => {
                return Err(format!(
                    "{}.{key} changed value type during migration",
                    node.id
                ))
            }
        }
    }
    let outputs = upgraded
        .outputs
        .into_iter()
        .map(|(key, ports)| {
            (
                key,
                match ports {
                    Expanded::Single(port) => Expanded::Single(wire(&node.id, &port)),
                    Expanded::Bundle(ports) => Expanded::Bundle(
                        ports
                            .into_iter()
                            .map(|(cap, port)| (cap, wire(&node.id, &port)))
                            .collect(),
                    ),
                },
            )
        })
        .collect();
    let definitions = upgraded
        .definitions
        .into_iter()
        .map(|(id, mut definition)| {
            if let Body::Graph(graph) = &mut definition.body {
                for node in graph.nodes.values_mut() {
                    if let Some(name) = remap.get(&node.definition) {
                        node.definition = name.clone();
                    }
                }
            }
            (remap[&id].clone(), definition)
        })
        .collect();
    Ok(Converted {
        node: Node {
            definition: remap
                .get(&upgraded.definition)
                .cloned()
                .unwrap_or(upgraded.definition),
            inputs: bindings,
            position: node.position_x.zip(node.position_y).map(|(x, y)| [x, y]),
        },
        definitions,
        outputs,
        bundle_edges,
    })
}
