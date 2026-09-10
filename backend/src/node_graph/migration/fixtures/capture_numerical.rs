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
    for rgb in [false, true] {
        for (kind, port, params) in [
            ("math", "a", json!({"operation":"add","b":0.27})),
            ("math", "a", json!({"operation":"subtract","b":0.27})),
            ("math", "a", json!({"operation":"multiply","b":2.3})),
            ("math", "a", json!({"operation":"divide","b":0.3})),
            ("math", "a", json!({"operation":"divide","b":0.0})),
            ("math", "a", json!({"operation":"min","b":0.27})),
            ("math", "a", json!({"operation":"max","b":0.27})),
            ("math", "a", json!({"operation":"abs_diff","b":0.27})),
            ("math", "a", json!({"operation":"modulo","b":0.3})),
            ("math", "a", json!({"operation":"modulo","b":0.0})),
            (
                "math",
                "a",
                json!({"operation":"circular_distance","b":0.27}),
            ),
            ("round", "in", json!({"operation":"round"})),
            ("round", "in", json!({"operation":"floor"})),
            ("round", "in", json!({"operation":"ceil"})),
            ("threshold", "in", json!({"threshold":0.0})),
            ("modulo", "in", json!({"divisor":0.3})),
            ("modulo", "in", json!({"divisor":0.0})),
            ("modulo", "in", json!({"divisor":-0.3})),
            (
                "remap",
                "in",
                json!({"in_min":-1.,"in_max":1.,"out_min":0.0,"out_max":1.0}),
            ),
            (
                "remap",
                "in",
                json!({"in_min":1.,"in_max":-1.,"out_min":1.0,"out_max":0.0}),
            ),
            (
                "remap",
                "in",
                json!({"in_min":0.2,"in_max":0.2,"out_min":0.1,"out_max":0.9}),
            ),
            (
                "remap",
                "in",
                json!({"clamp":0.0,"out_min":-1.,"out_max":2.}),
            ),
            (
                "rainbow",
                "in",
                json!({"offset":0.2,"spread":2.3,"saturation":0.45}),
            ),
            ("ramp_between", "start", json!({"end":0.8})),
        ] {
            let mut nodes = vec![
                node(
                    "wave",
                    "sine_wave",
                    json!({"subdivision":0.125,"phase_deg":33.,"amplitude":1.7,"offset":-0.2}),
                ),
                node("subject", kind, params.clone()),
                node("apply", "apply_color", json!({})),
                node("view", "view_signal", json!({})),
            ];
            let mut edges = vec![
                edge("subject", "out", "apply", "signal"),
                edge("subject", "out", "view", "in"),
            ];
            if rgb {
                nodes.extend([
                    node("color", "color", json!({"color":"#cc4488"})),
                    node("tint", "math", json!({"operation":"multiply"})),
                ]);
                edges.extend([
                    edge("color", "out", "tint", "a"),
                    edge("wave", "out", "tint", "b"),
                    edge("tint", "out", "subject", port),
                ]);
            } else {
                edges.push(edge("wave", "out", "subject", port));
            }
            cases.push((
                format!("{kind} {params} rgb={rgb}"),
                Graph {
                    nodes,
                    edges,
                    args: vec![],
                },
            ));
        }
    }
    for sink in ["apply_color", "apply_dimmer", "apply_strobe", "apply_speed"] {
        cases.push((
            sink.into(),
            Graph {
                nodes: vec![
                    node("color", "color", json!({"color":"#33aaff"})),
                    node("apply", sink, json!({})),
                ],
                edges: vec![edge(
                    "color",
                    "out",
                    "apply",
                    if sink == "apply_speed" {
                        "speed"
                    } else {
                        "signal"
                    },
                )],
                args: vec![],
            },
        ));
    }
    for (kind, value) in [
        ("palette", json!({"colors":["#00ff00","#ff0000","#0000ff"]})),
        ("palette", json!({"colors":["#ff5500"]})),
        ("palette", json!({"colors":[]})),
        (
            "gradient",
            json!({"stops":[{"t":0.,"color":"#00ff00"},{"t":0.3,"color":"#ff0000"},{"t":1.,"color":"#0000ff"}]}),
        ),
    ] {
        cases.push((
            format!("{kind} {value}"),
            Graph {
                nodes: vec![
                    node("stops", kind, json!({"value":value.to_string()})),
                    node("ramp", "ramp_between", json!({})),
                    node("sample", "sample_palette", json!({})),
                    node("apply", "apply_color", json!({})),
                ],
                edges: vec![
                    edge("stops", "out", "sample", "stops"),
                    edge("ramp", "out", "sample", "u"),
                    edge("sample", "out", "apply", "signal"),
                ],
                args: vec![],
            },
        ));
    }
    for (kind, params) in [
        ("color", json!({})),
        ("color", json!({"color":{"g":55.0,"a":0.2}})),
        ("color", json!({"color":" {\"r\":120,\"g\":50,\"b\":220}"})),
        ("rainbow", json!({"saturation":-0.2})),
        ("rainbow", json!({"saturation":1.7})),
    ] {
        let mut nodes = vec![
            node("source", kind, params.clone()),
            node("apply", "apply_color", json!({})),
        ];
        let mut edges = vec![edge("source", "out", "apply", "signal")];
        if kind == "rainbow" {
            nodes.push(node("hue", "scalar", json!({"value":0.35})));
            edges.push(edge("hue", "out", "source", "in"));
        }
        cases.push((
            format!("{kind} defaults {params}"),
            Graph {
                nodes,
                edges,
                args: vec![],
            },
        ));
    }
    cases.push((
        "identity time delay".into(),
        Graph {
            nodes: vec![
                node("source", "sine_wave", json!({"offset":0.4})),
                node("delay", "time_delay", json!({"delay":2.})),
                node("apply", "apply_dimmer", json!({})),
            ],
            edges: vec![
                edge("source", "out", "delay", "in"),
                edge("delay", "out", "apply", "signal"),
            ],
            args: vec![],
        },
    ));
    cases
}
#[test]
fn capture_original_numerical_graphs() {
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
        std::env::var("LUMA_CAPTURE_NUMERICAL_MIGRATION").expect("set the capture destination");
    std::fs::write(path, crate::canonical_json::to_string(&json!({"times":times,"grid":grid,"cells":cells,"clip_start":4.,"clip_duration":8.,"cases":captures}))).unwrap();
}
