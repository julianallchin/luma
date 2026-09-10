//! Reference-only generator. Expected values come from the original category
//! evaluator, never from the replacement sampler or migration.
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
fn capture_original_reductions() {
    let mut fixtures = Vec::new();
    for heads in [0, 3] {
        for variable in [false, true] {
            let grid = BeatGrid {
                beats: if variable {
                    vec![0., 0.4, 1., 1.6, 2.1, 2.9, 3.5, 4.2, 5.]
                } else {
                    (0..11).map(|n| n as f32 * 0.5).collect()
                },
                downbeats: vec![0., 2.1, 5.],
                bpm: 120.,
                downbeat_offset: 0.,
                beats_per_bar: 4,
            };
            let clock = grid.timeline().unwrap();
            for (start, end) in [(0., 1.), (0.75, 3.25)] {
                let positions =
                    vec![[0., 0., 0.], [-0.5, 0.2, 0.7], [0.8, -0.2, 1.]][..heads].to_vec();
                let ids: Vec<_> = ["head-c", "head-a", "head-b"][..heads]
                    .iter()
                    .map(|id| id.to_string())
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
                let times: Vec<f32> = [0., 0.13, 0.41, 0.78, 1., 0.41, -0.1, 1.1]
                    .into_iter()
                    .map(|p| start + (end - start) * p)
                    .collect();
                let mut cases = Vec::new();
                for (source, params) in [
                    ("scalar", json!({"value":0.3})),
                    ("ramp_between", json!({"start":-0.4,"end":1.2})),
                    (
                        "sine_wave",
                        json!({"phase_deg":13.,"subdivision":1.3,"amplitude":0.7,"offset":0.3}),
                    ),
                    ("circle", json!({"width":1.2,"height":0.4,"speed":0.7})),
                    (
                        "noise",
                        json!({"scale":1.3,"octaves":3,"amplitude":0.8,"offset":0.2}),
                    ),
                ] {
                    for sequence in [
                        vec!["normalize"],
                        vec!["invert"],
                        vec!["normalize", "normalize"],
                        vec!["normalize", "invert"],
                    ] {
                        let mut nodes = vec![node("source", source, params.clone())];
                        let mut edges = Vec::new();
                        let mut from = "source".to_string();
                        let mut port = if source == "circle" { "uv" } else { "out" };
                        for (i, kind) in sequence.iter().enumerate() {
                            let id = format!("reduce{i}");
                            nodes.push(node(&id, kind, json!({})));
                            edges.push(edge(&from, port, &id, "in"));
                            from = id;
                            port = "out";
                        }
                        nodes.extend([
                            node("view", "view_signal", json!({})),
                            node("writer", "apply_dimmer", json!({})),
                        ]);
                        edges.extend([
                            edge(&from, port, "view", "in"),
                            edge(&from, port, "writer", "signal"),
                        ]);
                        let graph = Graph {
                            nodes,
                            edges,
                            args: vec![],
                        };
                        let ctx = crate::eval::ResidentContext {
                            positions: positions.clone(),
                            beat_grid: Some(grid.clone()),
                            span: (start, end),
                            ..Default::default()
                        };
                        let plan = crate::eval::compile::compile_pattern(
                            &graph.nodes,
                            &graph.edges,
                            &HashMap::new(),
                            ctx,
                            ids.clone(),
                        )
                        .unwrap();
                        let frames = crate::eval::try_eval(
                            &plan,
                            &times,
                            &mut crate::eval::Arena::default(),
                        )
                        .unwrap();
                        let views = crate::eval::eval_views(
                            &plan,
                            &times,
                            &mut crate::eval::Arena::default(),
                        )
                        .unwrap();
                        cases.push(json!({"name":format!("{source}/{}",sequence.join("/")),"graph":graph,"frames":frames,"views":views}));
                    }
                }
                fixtures.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":clock.beat_at(f64::from(start)).unwrap(),"clip_duration":clock.beat_at(f64::from(end)).unwrap()-clock.beat_at(f64::from(start)).unwrap(),"cases":cases}));
            }
        }
    }
    let path =
        std::env::var("LUMA_CAPTURE_REDUCTIONS").expect("set an explicit reference output path");
    std::fs::write(path, crate::canonical_json::to_string(&json!(fixtures))).unwrap();
}
