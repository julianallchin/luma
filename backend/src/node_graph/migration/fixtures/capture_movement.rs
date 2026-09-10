//! Reference generator for the original numerical evaluator. See README.md.
use crate::models::node_graph::{BeatGrid, Edge, Graph, NodeInstance};
use luma_patterns as p;
use serde_json::json;
use std::collections::HashMap;

fn node(id: &str, kind: &str, params: serde_json::Value) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: kind.into(),
        params: serde_json::from_value(params).unwrap(),
        position_x: Some(100.),
        position_y: Some(80.),
    }
}
fn edge(from: &str, fp: &str, to: &str, tp: &str) -> Edge {
    Edge {
        id: format!("{from}.{fp}-{to}.{tp}"),
        from_node: from.into(),
        from_port: fp.into(),
        to_node: to.into(),
        to_port: tp.into(),
    }
}
fn cases() -> Vec<(String, Graph)> {
    let mut cases = Vec::new();
    for (kind, params) in [
        ("circle", json!({})),
        ("circle", json!({"radius":0.75,"speed":0.17})),
        ("figure_8", json!({})),
        ("figure_8", json!({"width":1.3,"height":0.4,"speed":-0.23})),
        ("sweep", json!({})),
        ("sweep", json!({"angle":37.,"range":0.8,"speed":0.14})),
        ("sweep", json!({"angle":135.,"range":0.6,"speed":0.3})),
    ] {
        for mode in [
            "direct",
            "scale",
            "rgb_add",
            "rgb_modulo",
            "rgb_reverse_add",
        ] {
            let mut nodes = vec![
                node("motion", kind, params.clone()),
                node("apply", "apply_movement", json!({})),
                node("view", "view_uv", json!({})),
            ];
            let mut edges = Vec::new();
            let (source, port) = if mode == "direct" {
                ("motion", "uv")
            } else {
                let op = match mode {
                    "scale" => "multiply",
                    "rgb_modulo" => "modulo",
                    _ => "add",
                };
                nodes.push(node("math", "math", json!({"operation":op,"b":1.7})));
                if mode == "rgb_reverse_add" {
                    nodes.push(node("color", "color", json!({"color":"#9955cc"})));
                    edges.push(edge("color", "out", "math", "a"));
                    edges.push(edge("motion", "uv", "math", "b"));
                } else {
                    edges.push(edge("motion", "uv", "math", "a"));
                    if mode.starts_with("rgb") {
                        nodes.push(node("color", "color", json!({"color":"#9955cc"})));
                        edges.push(edge("color", "out", "math", "b"));
                    }
                }
                ("math", "out")
            };
            edges.extend([
                edge(source, port, "apply", "uv"),
                edge(source, port, "view", "uv"),
            ]);
            // Also verify two- and three-channel movement as a color signal.
            nodes.push(node("tint", "apply_color", json!({})));
            edges.push(edge(source, port, "tint", "signal"));
            cases.push((
                format!("{kind} {params} {mode}"),
                Graph {
                    nodes,
                    edges,
                    args: vec![],
                },
            ));
        }
    }
    for (kind, params) in [
        ("scalar", json!({"value":0.75})),
        ("color", json!({"color":"#3366cc"})),
    ] {
        cases.push((
            format!("{kind} as movement"),
            Graph {
                nodes: vec![
                    node("source", kind, params),
                    node("apply", "apply_movement", json!({})),
                ],
                edges: vec![edge("source", "out", "apply", "uv")],
                args: vec![],
            },
        ));
    }
    cases
}
#[test]
fn capture_original_movement_graphs() {
    let times = vec![2.0_f32, 2.25, 3., 4.5, 5.9, 2.25];
    let ids = vec!["head-a".to_string(), "head-b".to_string()];
    let grid = BeatGrid {
        beats: (0..20).map(|n| n as f32 * 0.5).collect(),
        downbeats: vec![0., 2., 4., 6., 8.],
        bpm: 120.,
        downbeat_offset: 0.0,
        beats_per_bar: 4,
    };
    let ctx = crate::eval::ResidentContext {
        beat_grid: Some(grid.clone()),
        span: (2., 6.),
        positions: vec![[0., 0., 0.], [0., 0., 1.]],
        ..Default::default()
    };
    let cells: Vec<_> = ids
        .iter()
        .zip(&ctx.positions)
        .map(|(id, pos)| p::Cell {
            id: id.clone(),
            group: "all".into(),
            world: pos.map(f64::from),
            uvz: pos.map(f64::from),
        })
        .collect();
    let mut captures = Vec::new();
    for (name, graph) in cases() {
        let old = crate::eval::compile::compile_pattern(
            &graph.nodes,
            &graph.edges,
            &HashMap::new(),
            ctx.clone(),
            ids.clone(),
        )
        .unwrap();
        let frames = crate::eval::eval(&old, &times, &mut crate::eval::Arena::default());
        let views =
            crate::eval::eval_views(&old, &times, &mut crate::eval::Arena::default()).unwrap();
        captures.push(json!({"name":name,"graph":graph,"frames":frames,"views":views}));
    }
    let path =
        std::env::var("LUMA_CAPTURE_MOVEMENT_MIGRATION").expect("set the capture destination");
    std::fs::write(path, crate::canonical_json::to_string(&json!({"times":times,"grid":grid,"cells":cells,"clip_start":4.,"clip_duration":8.,"cases":captures}))).unwrap();
}
