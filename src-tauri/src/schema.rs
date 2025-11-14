use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum PortType {
    Intensity,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum ParamType {
    Number,
    Text,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PortDef {
    pub id: String,
    pub name: String,
    pub port_type: PortType,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ParamDef {
    pub id: String,
    pub name: String,
    pub param_type: ParamType,
    pub default_number: Option<f32>,
    pub default_text: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NodeTypeDef {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub inputs: Vec<PortDef>,
    pub outputs: Vec<PortDef>,
    pub params: Vec<ParamDef>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NodeInstance {
    pub id: String,
    pub type_id: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: HashMap<String, Value>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Graph {
    pub nodes: Vec<NodeInstance>,
    pub edges: Vec<Edge>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RunResult {
    pub views: HashMap<String, Vec<f32>>,
}

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "sample_pattern".into(),
            name: "Sample Pattern".into(),
            description: Some("Generates a repeating kick-style intensity envelope.".into()),
            category: Some("Sources".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "threshold".into(),
            name: "Threshold".into(),
            description: Some("Outputs 1.0 when input exceeds threshold, otherwise 0.0.".into()),
            category: Some("Modifiers".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            params: vec![ParamDef {
                id: "threshold".into(),
                name: "Threshold".into(),
                param_type: ParamType::Number,
                default_number: Some(0.5),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "view_channel".into(),
            name: "View Channel".into(),
            description: Some("Displays the incoming intensity channel.".into()),
            category: Some("Utilities".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_zone_dimmer".into(),
            name: "Apply Zone Dimmer".into(),
            description: Some("Marks the intensity channel for output to a zone dimmer.".into()),
            category: Some("Outputs".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            outputs: vec![],
            params: vec![ParamDef {
                id: "zone".into(),
                name: "Zone".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("Main".into()),
            }],
        },
    ]
}

#[tauri::command]
pub async fn run_graph(graph: Graph) -> Result<RunResult, String> {
    println!("Received graph with {} nodes to run.", graph.nodes.len());

    if graph.nodes.is_empty() {
        return Ok(RunResult {
            views: HashMap::new(),
        });
    }

    const PREVIEW_LENGTH: usize = 256;

    let nodes_by_id: HashMap<&str, &NodeInstance> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut dependency_graph: DiGraph<&str, ()> = DiGraph::new();
    let mut node_indices = HashMap::new();

    for node in &graph.nodes {
        let idx = dependency_graph.add_node(node.id.as_str());
        node_indices.insert(node.id.as_str(), idx);
    }

    for edge in &graph.edges {
        let Some(&from_idx) = node_indices.get(edge.from_node.as_str()) else {
            return Err(format!("Unknown from_node '{}' in edge", edge.from_node));
        };
        let Some(&to_idx) = node_indices.get(edge.to_node.as_str()) else {
            return Err(format!("Unknown to_node '{}' in edge", edge.to_node));
        };
        dependency_graph.add_edge(from_idx, to_idx, ());
    }

    let sorted = toposort(&dependency_graph, None)
        .map_err(|_| "Graph has a cycle. Execution aborted.".to_string())?;

    let mut incoming_edges: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        incoming_edges
            .entry(edge.to_node.as_str())
            .or_default()
            .push(edge);
    }

    let mut output_buffers: HashMap<(String, String), Vec<f32>> = HashMap::new();
    let mut view_results: HashMap<String, Vec<f32>> = HashMap::new();

    for node_idx in sorted {
        let node_id = dependency_graph[node_idx];
        let node = nodes_by_id
            .get(node_id)
            .copied()
            .ok_or_else(|| format!("Node '{}' not found during execution", node_id))?;

        match node.type_id.as_str() {
            "sample_pattern" => {
                let mut buffer = vec![0.0f32; PREVIEW_LENGTH];

                for start in (0..PREVIEW_LENGTH).step_by(64) {
                    buffer[start] = 1.0;
                    if start + 1 < PREVIEW_LENGTH {
                        buffer[start + 1] = 0.5;
                    }
                    if start + 2 < PREVIEW_LENGTH {
                        buffer[start + 2] = 0.2;
                    }
                }

                output_buffers.insert((node.id.clone(), "out".into()), buffer);
            }
            "threshold" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.first())
                    .ok_or_else(|| format!("Threshold node '{}' missing input", node.id))?;

                let input_buffer = output_buffers
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                    .ok_or_else(|| format!(
                        "Threshold node '{}' input buffer not found",
                        node.id
                    ))?;

                let threshold = node
                    .params
                    .get("threshold")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.5) as f32;

                let mut output = Vec::with_capacity(PREVIEW_LENGTH);
                for &sample in input_buffer.iter().take(PREVIEW_LENGTH) {
                    output.push(if sample >= threshold { 1.0 } else { 0.0 });
                }

                output_buffers.insert((node.id.clone(), "out".into()), output);
            }
            "view_channel" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.first())
                    .ok_or_else(|| format!("View node '{}' missing input", node.id))?;

                let input_buffer = output_buffers
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                    .ok_or_else(|| format!(
                        "View node '{}' input buffer not found",
                        node.id
                    ))?;

                view_results.insert(node.id.clone(), input_buffer.clone());
            }
            "apply_zone_dimmer" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.first())
                    .ok_or_else(|| format!("Zone dimmer node '{}' missing input", node.id))?;

                let _ = output_buffers.get(&(input_edge.from_node.clone(), input_edge.from_port.clone())).ok_or_else(|| format!(
                    "Zone dimmer node '{}' input buffer not found",
                    node.id
                ))?;

                if let Some(zone) = node
                    .params
                    .get("zone")
                    .and_then(|value| value.as_str())
                {
                    println!("Zone '{}' dimmer updated from node '{}'", zone, node.id);
                } else {
                    println!("Zone dimmer node '{}' executed", node.id);
                }
            }
            other => {
                println!("Encountered unknown node type '{}'", other);
            }
        }
    }

    Ok(RunResult { views: view_results })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(graph: Graph) -> RunResult {
        tauri::async_runtime::block_on(run_graph(graph)).expect("graph execution should succeed")
    }

    #[test]
    fn sample_pattern_flows_to_view() {
        let sample_node = NodeInstance {
            id: "n1".into(),
            type_id: "sample_pattern".into(),
            params: HashMap::new(),
        };

        let view_node = NodeInstance {
            id: "n2".into(),
            type_id: "view_channel".into(),
            params: HashMap::new(),
        };

        let edge = Edge {
            id: "e1".into(),
            from_node: "n1".into(),
            from_port: "out".into(),
            to_node: "n2".into(),
            to_port: "in".into(),
        };

        let result = run(Graph {
            nodes: vec![sample_node, view_node],
            edges: vec![edge],
        });

        assert!(result.views.contains_key("n2"));
        let samples = &result.views["n2"];
        assert_eq!(samples[0], 1.0);
        assert_eq!(samples[1], 0.5);
        assert_eq!(samples[2], 0.2);
    }

    #[test]
    fn threshold_applies_binary_output() {
        let sample_node = NodeInstance {
            id: "n1".into(),
            type_id: "sample_pattern".into(),
            params: HashMap::new(),
        };

        let threshold_node = NodeInstance {
            id: "n2".into(),
            type_id: "threshold".into(),
            params: HashMap::from([(String::from("threshold"), json!(0.6))]),
        };

        let view_node = NodeInstance {
            id: "n3".into(),
            type_id: "view_channel".into(),
            params: HashMap::new(),
        };

        let edges = vec![
            Edge {
                id: "e1".into(),
                from_node: "n1".into(),
                from_port: "out".into(),
                to_node: "n2".into(),
                to_port: "in".into(),
            },
            Edge {
                id: "e2".into(),
                from_node: "n2".into(),
                from_port: "out".into(),
                to_node: "n3".into(),
                to_port: "in".into(),
            },
        ];

        let result = run(Graph {
            nodes: vec![sample_node, threshold_node, view_node],
            edges,
        });

        let samples = &result.views["n3"];
        assert_eq!(samples[0], 1.0);
        assert_eq!(samples[1], 0.0);
    }
}

