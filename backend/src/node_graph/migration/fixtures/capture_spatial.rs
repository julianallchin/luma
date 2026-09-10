//! Independent capture against the original spatial evaluator. See fixtures/README.md.
use crate::models::node_graph::{BeatGrid, Edge, Graph, NodeInstance};
use luma_patterns as p;
use serde_json::json;
use std::collections::HashMap;
fn node(id: &str, kind: &str, params: serde_json::Value) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: kind.into(),
        params: serde_json::from_value(params).unwrap(),
        position_x: Some(0.),
        position_y: Some(0.),
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
    let attributes: Vec<_> = crate::eval::ops::spatial::ATTRIBUTES
        .iter()
        .map(|a| a.0)
        .chain(
            crate::eval::ops::spatial::LEGACY_ATTRIBUTES
                .iter()
                .map(|a| a.0),
        )
        .collect();
    for attribute in attributes {
        for mirror in [None, Some("x"), Some("y"), Some("z")] {
            let mut nodes = vec![
                node("attribute", "get_attribute", json!({"attribute":attribute})),
                node("apply", "apply_dimmer", json!({})),
                node("view", "view_signal", json!({})),
            ];
            let mut edges = vec![
                edge("attribute", "out", "apply", "signal"),
                edge("attribute", "out", "view", "in"),
            ];
            if let Some(axis) = mirror {
                nodes.push(node("mirror", "mirror", json!({"axis":axis})));
                edges.push(edge("mirror", "out", "attribute", "selection"));
            }
            cases.push((
                format!("{attribute} mirror {mirror:?}"),
                Graph {
                    nodes,
                    edges,
                    args: vec![],
                },
            ));
        }
    }
    for axis in ["x", "y", "z"] {
        cases.push((
            format!("raw mirror {axis}"),
            Graph {
                nodes: vec![
                    node("mirror", "mirror", json!({"axis":axis})),
                    node("apply", "apply_color", json!({})),
                    node("positions", "view_uv", json!({})),
                    node("side", "view_signal", json!({})),
                ],
                edges: vec![
                    edge("mirror", "out", "apply", "signal"),
                    edge("mirror", "out", "positions", "uv"),
                    edge("mirror", "side", "side", "in"),
                ],
                args: vec![],
            },
        ));
    }
    cases
}
#[test]
fn capture_original_spatial_graphs() {
    let times = vec![2.0_f32, 2.25, 3., 4.5, 5.9, 2.25];
    let grid = BeatGrid {
        beats: (0..20).map(|n| n as f32 * 0.5).collect(),
        downbeats: vec![0., 2., 4., 6., 8.],
        bpm: 120.,
        downbeat_offset: 0.,
        beats_per_bar: 4,
    };
    let circle = (0..8)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 8.;
            [a.cos() * 4., a.sin() * 4., a.sin() * 2. + 3.]
        })
        .collect();
    let rigs: Vec<(&str, Vec<[f32; 3]>)> = vec![
        (
            "asymmetric",
            vec![
                [-3., -1., 0.],
                [0., 0., 0.],
                [0.0002, 0.0001, 0.],
                [1., 2., 1.],
                [1.1, 2.1, 1.1],
                [4., -2., 3.],
            ],
        ),
        ("circle", circle),
        ("stack", vec![[1., 1., 0.], [1., 1., 1.], [1., 1., 2.]]),
        (
            "flat rotated",
            vec![
                [-3., -2., 4.],
                [-1., -0.666667, 4.],
                [1., 0.666667, 4.],
                [3., 2., 4.],
            ],
        ),
        ("single", vec![[5., -2., 3.]]),
        ("empty", vec![]),
    ];
    let mut groups = Vec::new();
    for (rig, positions) in rigs {
        // Reverse lexical identity order to catch accidental reassignment of
        // geometry or index values during tensor canonicalization.
        let ids: Vec<_> = (0..positions.len())
            .map(|n| format!("head-{}", positions.len() - n))
            .collect();
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
                uvz: [pos[0] as f64, -f64::from(pos[1]), pos[2] as f64],
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
            captures.push(json!({"name":format!("{rig}: {name}"),"graph":graph,"frames":frames,"views":views}));
        }
        groups.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":4.,"clip_duration":8.,"cases":captures}));
    }
    let path = std::env::var("LUMA_CAPTURE_SPATIAL_MIGRATION").expect("set capture destination");
    std::fs::write(path, crate::canonical_json::to_string(&json!(groups))).unwrap();
}
