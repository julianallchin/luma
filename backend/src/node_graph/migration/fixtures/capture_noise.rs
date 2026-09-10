//! Independent original noise/wander evaluator references; see fixtures/README.md.
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
    for seed in ["noise", "organic-path", "a-different-seed"] {
        for params in [
            json!({}),
            json!({"scale":1.7,"octaves":4.,"amplitude":0.8,"offset":0.35}),
            json!({"scale":-0.7,"octaves":8.,"amplitude":1.2,"offset":-0.1}),
        ] {
            for mode in [
                "none",
                "x",
                "y",
                "xy",
                "time",
                "xt",
                "yt",
                "xyt",
                "field_time",
                "y_field_time",
            ] {
                let mut nodes = vec![
                    node(seed, "noise", params.clone()),
                    node("apply", "apply_color", json!({})),
                    node("view", "view_signal", json!({})),
                ];
                let mut edges = vec![
                    edge(seed, "out", "apply", "signal"),
                    edge(seed, "out", "view", "in"),
                ];
                for (key, attribute) in [("x", "pos_x"), ("y", "pos_y")] {
                    if mode.contains(key) && mode != "field_time" {
                        nodes.push(node(key, "get_attribute", json!({"attribute":attribute})));
                        edges.push(edge(key, "out", seed, key));
                    }
                }
                if mode.contains('t') {
                    nodes.push(if mode.contains("field_time") {
                        node("time", "get_attribute", json!({"attribute":"pos_z"}))
                    } else {
                        node(
                            "time",
                            "sine_wave",
                            json!({"subdivision":0.17,"amplitude":0.8,"offset":0.25}),
                        )
                    });
                    edges.push(edge("time", "out", seed, "time"));
                }
                cases.push((
                    format!("noise {seed} {params} {mode}"),
                    Graph {
                        nodes,
                        edges,
                        args: vec![],
                    },
                ));
            }
        }
        for params in [
            json!({}),
            json!({"radius":0.9,"speed":0.31,"smoothness":3.2}),
            json!({"radius":2.5,"speed":-0.7,"smoothness":8.}),
            json!({"radius":0.3,"speed":0.,"smoothness":0.1}),
        ] {
            cases.push((
                format!("wander {seed} {params}"),
                Graph {
                    nodes: vec![
                        node(seed, "wander", params),
                        node("apply", "apply_movement", json!({})),
                        node("tint", "apply_color", json!({})),
                        node("view", "view_uv", json!({})),
                    ],
                    edges: vec![
                        edge(seed, "uv", "apply", "uv"),
                        edge(seed, "uv", "tint", "signal"),
                        edge(seed, "uv", "view", "uv"),
                    ],
                    args: vec![],
                },
            ));
        }
    }
    cases
}
#[test]
fn capture_original_noise_graphs() {
    let times = vec![2.0_f32, 2.25, 3., 4.5, 5.9, 2.25];
    let grid = BeatGrid {
        beats: (0..20).map(|n| n as f32 * 0.5).collect(),
        downbeats: vec![0., 2., 4., 6., 8.],
        bpm: 120.,
        downbeat_offset: 0.,
        beats_per_bar: 4,
    };
    let mut references = Vec::new();
    for count in [0, 1, 3] {
        let positions =
            vec![[-0.73, 0.12, 1.2], [1.43, -0.55, -0.31], [0.41, 0.83, 0.62]][..count].to_vec();
        let ids = vec![
            "head-z".to_string(),
            "head-a".to_string(),
            "head-m".to_string(),
        ][..count]
            .to_vec();
        let ctx = crate::eval::ResidentContext {
            beat_grid: Some(grid.clone()),
            span: (2., 6.),
            positions: positions.clone(),
            ..Default::default()
        };
        let cells: Vec<_> = ids
            .iter()
            .zip(&positions)
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
        references.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":4.,"clip_duration":8.,"cases":captures}));
    }
    let path = std::env::var("LUMA_CAPTURE_NOISE_MIGRATION").expect("set the capture destination");
    std::fs::write(path, crate::canonical_json::to_string(&json!(references))).unwrap();
}
