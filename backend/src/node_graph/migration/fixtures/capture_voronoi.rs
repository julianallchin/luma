//! Frozen references produced only by the original category evaluator.
use crate::models::node_graph::{BeatGrid, Edge, Graph, NodeInstance};
use serde_json::json;
use std::collections::HashMap;
fn node(id: &str, kind: &str, params: serde_json::Value) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: kind.into(),
        params: serde_json::from_value(params).unwrap(),
        position_x: None,
        position_y: None,
    }
}
fn edge(from: &str, port: &str, to: &str, input: &str) -> Edge {
    Edge {
        id: format!("{from}.{port}-{to}.{input}"),
        from_node: from.into(),
        from_port: port.into(),
        to_node: to.into(),
        to_port: input.into(),
    }
}
#[test]
fn capture_original_voronoi() {
    let grid = BeatGrid {
        beats: vec![0., 0.4, 1., 1.6, 2.1, 2.9, 3.5, 4.2, 5.],
        downbeats: vec![0., 2.1, 5.],
        bpm: 110.,
        downbeat_offset: 0.,
        beats_per_bar: 4,
    };
    let clock = grid.timeline().unwrap();
    let times = vec![-1., 0., 0.17, 0.75, 1.4, 3.7, 8., 31., -1., 3.7];
    let mut fixtures = Vec::new();
    for positions in [
        vec![],
        vec![[0.; 3]],
        vec![[-1., 0., 0.], [0., 0., 0.], [1., 0., 0.]],
        vec![
            [-1., 0., 0.],
            [0., -0.2, 1.],
            [1., 0.5, 2.],
            [-0.7, 0.9, 1.4],
            [0.3, 0.1, 0.4],
        ],
    ] {
        let ids: Vec<_> = (0..positions.len())
            .map(|i| format!("head-{}", positions.len() - i))
            .collect();
        let cells: Vec<_> = ids
            .iter()
            .zip(&positions)
            .map(|(id, p)| luma_patterns::Cell {
                id: id.clone(),
                group: "all".into(),
                world: p.map(f64::from),
                uvz: p.map(f64::from),
            })
            .collect();
        let mut cases = Vec::new();
        for (palette_name, palette) in [
            ("empty", json!({"colors":[]})),
            ("one", json!({"colors":["#ff8811"]})),
            ("three", json!({"colors":["#ff2255","#33ff88","#3355ff"]})),
            (
                "uneven",
                json!({"stops":[{"t":0.,"color":"#ff0000"},{"t":0.2,"color":"#00ff88"},{"t":1.,"color":"#1133ff"}]}),
            ),
            (
                "alpha",
                json!({"colors":["#ff000033","#00ff00aa","#0000ffff"]}),
            ),
        ] {
            let mut variants = vec![("default".to_string(), json!({}))];
            for (key, values) in [
                ("num_points", vec![-1., 1., 3., 6.5, 64., 100.]),
                ("softness", vec![-1., 0., 0.05, 1.]),
                ("vibrance", vec![0., 1., 1.5]),
                ("wander_speed", vec![0., -0.8, 1.3]),
                ("seed_offset", vec![1., 1234.]),
            ] {
                for value in values {
                    variants.push((format!("{key}-{value}"), json!({key:value})));
                }
            }
            for (variant, params) in variants {
                let graph = Graph {
                    nodes: vec![
                        node(
                            "palette",
                            "palette",
                            json!({"value":palette.to_string()}),
                        ),
                        node("effect", "soft_voronoi", params),
                        node("view", "view_signal", json!({})),
                        node("writer", "apply_color", json!({})),
                    ],
                    edges: vec![
                        edge("palette", "out", "effect", "stops"),
                        edge("effect", "out", "view", "in"),
                        edge("effect", "out", "writer", "signal"),
                    ],
                    args: vec![],
                };
                let plan = crate::eval::compile::compile_pattern(
                    &graph.nodes,
                    &graph.edges,
                    &HashMap::new(),
                    crate::eval::ResidentContext {
                        positions: positions.clone(),
                        beat_grid: Some(grid.clone()),
                        span: (0.75, 8.),
                        ..Default::default()
                    },
                    ids.clone(),
                )
                .unwrap();
                let frames =
                    crate::eval::try_eval(&plan, &times, &mut crate::eval::Arena::default())
                        .unwrap();
                let views =
                    crate::eval::eval_views(&plan, &times, &mut crate::eval::Arena::default())
                        .unwrap();
                cases.push(json!({"name":format!("{palette_name}/{variant}"),"graph":graph,"frames":frames,"views":views}));
            }
        }
        fixtures.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":clock.beat_at(0.75).unwrap(),"clip_duration":clock.beat_at(8.).unwrap()-clock.beat_at(0.75).unwrap(),"cases":cases}));
    }
    std::fs::write(
        std::env::var("LUMA_CAPTURE_VORONOI").unwrap(),
        crate::canonical_json::to_string(&json!(fixtures)),
    )
    .unwrap();
}
