//! Independent original-evaluator references; see fixtures/README.md.
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
    for (kind, port, params) in [
        (
            "sine_wave",
            "out",
            json!({"subdivision":0.31,"offset":0.3,"amplitude":1.4}),
        ),
        ("color", "out", json!({"color":"#407fcc"})),
        ("get_attribute", "out", json!({"attribute":"rel_x"})),
        ("harmony_analysis", "signal", json!({})),
    ] {
        for width in [-0.2, 0., 0.7, 2.] {
            for curve in [-1.3, -0.0009, 0., 0.0009, 0.8] {
                cases.push((
                    format!("falloff {kind} width={width} curve={curve}"),
                    Graph {
                        nodes: vec![
                            node("source", kind, params.clone()),
                            node("falloff", "falloff", json!({"width":width,"curve":curve})),
                            node("apply", "apply_color", json!({})),
                            node("view", "view_signal", json!({})),
                        ],
                        edges: vec![
                            edge("source", port, "falloff", "in"),
                            edge("falloff", "out", "apply", "signal"),
                            edge("falloff", "out", "view", "in"),
                        ],
                        args: vec![],
                    },
                ));
            }
        }
    }
    for mode in [
        "direct",
        "mix",
        "negative",
        "below_floor",
        "short_input",
        "field_mix",
    ] {
        for palette in [
            json!(null),
            json!({"colors":[]}),
            json!({"colors":["#885522"]}),
            json!({"colors":["#ee2211","#113388","#33cc99"]}),
            json!({"stops":[{"t":0.,"color":"#202040"},{"t":0.3,"color":"#ffccee"},{"t":1.,"color":"#112266"}]}),
        ] {
            let mut nodes = vec![
                node("harmony", "harmony_analysis", json!({})),
                node(
                    "palette",
                    "chroma_palette",
                    if palette.is_null() {
                        json!({})
                    } else {
                        json!({"fallback_palette":palette.to_string()})
                    },
                ),
                node("apply", "apply_color", json!({})),
                node("view", "view_signal", json!({})),
                node("pitches", "view_signal", json!({})),
            ];
            let mut edges = vec![
                edge("palette", "out", "apply", "signal"),
                edge("palette", "out", "view", "in"),
                edge("harmony", "signal", "pitches", "in"),
            ];
            let (src, port) = match mode {
                "direct" => ("harmony", "signal"),
                "short_input" => {
                    nodes.push(node("short", "color", json!({"color":"#ff0080"})));
                    ("short", "out")
                }
                "field_mix" => {
                    nodes.push(node("field", "get_attribute", json!({"attribute":"rel_x"})));
                    nodes.push(node("weights", "math", json!({"operation":"add"})));
                    edges.push(edge("harmony", "signal", "weights", "a"));
                    edges.push(edge("field", "out", "weights", "b"));
                    ("weights", "out")
                }
                _ => {
                    let (op, b) = match mode {
                        "mix" => ("add", 0.08),
                        "negative" => ("subtract", 0.2),
                        _ => ("multiply", 0.0001),
                    };
                    nodes.push(node("weights", "math", json!({"operation":op,"b":b})));
                    edges.push(edge("harmony", "signal", "weights", "a"));
                    ("weights", "out")
                }
            };
            edges.push(edge(src, port, "palette", "chroma"));
            cases.push((
                format!("chroma {mode} {palette}"),
                Graph {
                    nodes,
                    edges,
                    args: vec![],
                },
            ));
        }
    }
    cases
}
#[test]
fn capture_original_harmony_graphs() {
    let times = vec![
        2.0_f32, 2.125, 2.25, 2.5, 2.75, 3., 3.25, 3.5, 4., 4.25, 4.5, 5., 5.5, 5.9, 2.25,
    ];
    let ids = vec![
        "head-z".to_string(),
        "head-a".to_string(),
        "head-m".to_string(),
    ];
    let grid = BeatGrid {
        beats: (0..20).map(|n| n as f32 * 0.5).collect(),
        downbeats: vec![0., 2., 4., 6., 8.],
        bpm: 120.,
        downbeat_offset: 0.,
        beats_per_bar: 4,
    };
    let harmony = vec![
        (2., 2.25, Some(0)),
        (2.25, 2.5, Some(1)),
        (2.5, 2.75, Some(2)),
        (2.75, 3., Some(3)),
        (3., 3.25, Some(4)),
        (3.25, 3.5, Some(5)),
        (3.5, 4., Some(6)),
        (4., 4.25, Some(7)),
        (4.25, 4.5, Some(8)),
        (4.5, 5., Some(9)),
        (5., 5.5, Some(10)),
        (5.5, 5.8, Some(11)),
        (2.1, 2.2, None),
    ];
    let ctx = crate::eval::ResidentContext {
        beat_grid: Some(grid.clone()),
        span: (2., 6.),
        positions: vec![[-1., 0., 0.], [0., 0., 1.], [1., 1., 2.]],
        chord_sections: harmony.clone(),
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
        std::env::var("LUMA_CAPTURE_HARMONY_MIGRATION").expect("set the capture destination");
    std::fs::write(path,crate::canonical_json::to_string(&json!({"times":times,"grid":grid,"cells":cells,"harmony":harmony,"clip_start":4.,"clip_duration":8.,"cases":captures}))).unwrap();
}
