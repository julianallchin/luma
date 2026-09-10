//! Independent original random-selection references. Expected values use only
//! the unchanged category evaluator, never the replacement or converter.
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
fn capture_original_selection_graphs() {
    let times = vec![
        -0.1, 0., 0.04, 0.1, 0.49, 0.5, 0.74, 0.75, 0.9, 1., 1.01, 1.1, 1.49, 1.5, 2.3, 3.49, 3.5,
        5., 6., 2.3,
    ];
    let grid = BeatGrid {
        beats: vec![0., 0.4, 1., 1.6, 2.1, 2.9, 3.5, 4.2, 5.],
        downbeats: vec![0., 2.1, 5.],
        bpm: 110.,
        downbeat_offset: 0.,
        beats_per_bar: 4,
    };
    let onsets = HashMap::from([
        (
            "kick".to_string(),
            vec![
                0., 0.001, 0.026, 0.05, 0.9, 1., 1.01, 1.02, 1.04, 1.09, 2., 4.,
            ],
        ),
        ("snare".into(), vec![0.5, 1.5, 2.5, 3.5]),
        ("hat".into(), vec![0.15]),
        ("cymbal".into(), vec![]),
    ]);
    let clock = grid.timeline().unwrap();
    let mut fixtures = Vec::new();
    for heads in [0, 1, 5] {
        for clip_start_seconds in [0., 0.75] {
            let ids = vec![
                "head-z".to_string(),
                "head-a".into(),
                "head-y".into(),
                "head-b".into(),
                "head-m".into(),
            ][..heads]
                .to_vec();
            let positions = vec![
                [-0.7, 0., 1.],
                [0.9, 0., 0.2],
                [0.2, 0., 0.6],
                [-0.4, 0., 0.5],
                [0.7, 0., 0.8],
            ][..heads]
                .to_vec();
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
            let clip_start = clock.beat_at(clip_start_seconds).unwrap();
            let clip_duration = clock.beat_at(7.).unwrap() - clip_start;
            let ctx = crate::eval::ResidentContext {
                beat_grid: Some(grid.clone()),
                drum_onsets: onsets.clone(),
                positions,
                span: (clip_start_seconds as f32, 7.),
                ..Default::default()
            };
            let mut cases = Vec::new();
            for source in ["beats", "kick", "snare", "none"] {
                for count in [-1., 0., 0.5, 1., 2., 4., 12.] {
                    for avoid in [false, true] {
                        for identity in ["random", "sweep_copy"] {
                            let mut nodes = vec![
                                node(
                                    identity,
                                    "random_select_mask",
                                    json!({"count":count,"avoid_repeat":avoid}),
                                ),
                                node("view", "view_signal", json!({})),
                                node("writer", "apply_dimmer", json!({})),
                            ];
                            let mut edges = vec![
                                edge(identity, "out", "view", "in"),
                                edge(identity, "out", "writer", "signal"),
                            ];
                            match source {
                                "none" => (),
                                "beats" => {
                                    nodes.push(node(
                                        "trigger",
                                        "beat_pulses",
                                        json!({"subdivision":2.,"offset":0.1}),
                                    ));
                                    edges.push(edge(
                                        "trigger",
                                        "events_out",
                                        identity,
                                        "events_in",
                                    ));
                                }
                                drum => {
                                    nodes.push(node("trigger", "drum_events", json!({})));
                                    edges.push(edge(
                                        "trigger",
                                        &format!("{drum}_out"),
                                        identity,
                                        "events_in",
                                    ));
                                }
                            }
                            let graph = Graph {
                                nodes,
                                edges,
                                args: vec![],
                            };
                            let name = format!(
                                "{heads}-{clip_start_seconds}-{source}-{count}-{avoid}-{identity}"
                            );
                            let old = crate::eval::compile::compile_pattern(
                                &graph.nodes,
                                &graph.edges,
                                &HashMap::new(),
                                ctx.clone(),
                                ids.clone(),
                            )
                            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
                            let frames =
                                crate::eval::eval(&old, &times, &mut crate::eval::Arena::default());
                            let views = crate::eval::eval_views(
                                &old,
                                &times,
                                &mut crate::eval::Arena::default(),
                            )
                            .unwrap();
                            cases.push(
                                json!({"name":name,"graph":graph,"frames":frames,"views":views}),
                            );
                        }
                    }
                }
            }
            fixtures.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":clip_start,"clip_duration":clip_duration,"onsets":onsets,"cases":cases}));
        }
    }
    std::fs::write(
        std::env::var("LUMA_CAPTURE_SELECTION_MIGRATION").expect("capture destination"),
        crate::canonical_json::to_string(&json!(fixtures)),
    )
    .unwrap();
}
