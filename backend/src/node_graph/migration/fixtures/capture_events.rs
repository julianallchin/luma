//! Independent original event/envelope references. Never calls the converter.
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
fn capture_original_event_graphs() {
    let mut times = vec![
        -1., -0.051, -0.05, -0.001, 0., 0.001, 0.049, 0.05, 0.051, 0.1, 0.15, 0.2, 0.3, 0.45, 0.5,
        0.6, 0.75, 0.95, 1., 1.025, 1.049, 1.05, 1.1, 1.2, 1.5, 1.75, 2., 2.5, 3., 4., 6., 10.,
    ];
    times.extend([1., 0.2, 4., -0.05]);
    let shapes = [
        json!({}),
        json!({"attack":0.,"decay":0.,"sustain":1.,"release":0.,"amplitude":1.4,"sustain_level":0.4}),
        json!({"attack":0.7,"decay":0.8,"sustain":0.2,"release":0.8,"attack_curve":-0.7,"decay_curve":0.6,"anticipate":1.,"fit_to_gap":0.,"length_beats":8.}),
        json!({"attack":0.,"decay":0.,"sustain":0.,"release":0.,"anticipate":true}),
        json!({"attack":-1.,"decay":2.,"sustain":0.,"release":0.5,"attack_curve":0.7,"decay_curve":-0.5,"sustain_level":1.3,"amplitude":-0.7}),
    ];
    let onsets = HashMap::from([
        (
            "kick".to_string(),
            vec![
                0., 0.01, 0.024, 0.025, 0.05, 0.9, 1., 1.01, 1.02, 1.04, 1.09, 2., 4.,
            ],
        ),
        ("snare".into(), vec![0.5, 1.5, 2.5, 3.5]),
        ("hat".into(), vec![0.15]),
        ("cymbal".into(), vec![]),
    ]);
    let mut fixtures = Vec::new();
    for variable in [false, true] {
        let grid = BeatGrid {
            beats: if variable {
                vec![-0.7, -0.2, 0.25, 0.8, 1.4, 1.85, 2.5, 3., 3.45, 4.1, 4.6]
            } else {
                (0..13).map(|i| i as f32 * 0.5).collect()
            },
            downbeats: if variable {
                vec![-0.2, 1.85, 4.1]
            } else {
                vec![0., 2., 4., 6.]
            },
            bpm: if variable { 132. } else { 120. },
            downbeat_offset: 0.,
            beats_per_bar: 4,
        };
        for count in [0, 3] {
            let ids =
                vec!["head-z".to_string(), "head-a".into(), "head-m".into()][..count].to_vec();
            let positions = vec![[-0.7, 0., 1.], [0.9, 0., 0.2], [0.2, 0., 0.6]][..count].to_vec();
            let clock = grid.timeline().unwrap();
            let clip_start = clock.beat_at(0.75).unwrap();
            let clip_duration = clock.beat_at(4.75).unwrap() - clip_start;
            let ctx = crate::eval::ResidentContext {
                beat_grid: Some(grid.clone()),
                drum_onsets: onsets.clone(),
                span: (0.75, 4.75),
                positions: positions.clone(),
                ..Default::default()
            };
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
            let mut capture =
                |name: String, mut nodes: Vec<NodeInstance>, mut edges: Vec<Edge>, port: &str| {
                    let port = if nodes
                        .iter()
                        .any(|node| node.id == "effect" && node.type_id == "adsr")
                    {
                        "signal_out"
                    } else {
                        port
                    };
                    nodes.extend([
                        node("view", "view_signal", json!({})),
                        node("writer", "apply_dimmer", json!({})),
                    ]);
                    edges.extend([
                        edge("effect", port, "view", "in"),
                        edge("effect", port, "writer", "signal"),
                    ]);
                    let graph = Graph {
                        nodes,
                        edges,
                        args: vec![],
                    };
                    let old = crate::eval::compile::compile_pattern(
                        &graph.nodes,
                        &graph.edges,
                        &HashMap::new(),
                        ctx.clone(),
                        ids.clone(),
                    )
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
                    let frames =
                        crate::eval::eval(&old, &times, &mut crate::eval::Arena::default());
                    let views =
                        crate::eval::eval_views(&old, &times, &mut crate::eval::Arena::default())
                            .unwrap();
                    cases.push(json!({"name":name,"graph":graph,"frames":frames,"views":views}));
                };
            for subdivision in [0., 0.5, 1., 2., -2.] {
                for downbeat in [0, 1] {
                    for offset in [-0.3, 0.2] {
                        let params = json!({"subdivision":subdivision,"offset":offset,"only_downbeats":downbeat});
                        let suffix =
                            format!("{variable}-{count}-{subdivision}-{downbeat}-{offset}");
                        capture(
                            format!("pulses-{suffix}"),
                            vec![node("effect", "beat_pulses", params.clone())],
                            vec![],
                            "events_out",
                        );
                        for (i, shape) in shapes.iter().enumerate() {
                            let mut merged = params.clone();
                            merged
                                .as_object_mut()
                                .unwrap()
                                .extend(shape.as_object().unwrap().clone());
                            capture(
                                format!("envelope-{i}-{suffix}"),
                                vec![node("effect", "beat_envelope", merged)],
                                vec![],
                                "out",
                            );
                            capture(
                                format!("adsr-beat-{i}-{suffix}"),
                                vec![
                                    node("trigger", "beat_pulses", params.clone()),
                                    node("effect", "adsr", shape.clone()),
                                ],
                                vec![edge("trigger", "events_out", "effect", "events_in")],
                                "out",
                            );
                        }
                    }
                }
            }
            for drum in ["kick", "snare", "hat", "cymbal"] {
                for (i, shape) in shapes.iter().enumerate() {
                    capture(
                        format!("adsr-{drum}-{i}-{variable}-{count}"),
                        vec![
                            node("trigger", "drum_events", json!({})),
                            node("effect", "adsr", shape.clone()),
                        ],
                        vec![edge(
                            "trigger",
                            &format!("{drum}_out"),
                            "effect",
                            "events_in",
                        )],
                        "out",
                    );
                }
            }
            fixtures.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":clip_start,"clip_duration":clip_duration,"onsets":onsets,"cases":cases}));
        }
    }
    std::fs::write(
        std::env::var("LUMA_CAPTURE_EVENT_MIGRATION").expect("capture destination"),
        crate::canonical_json::to_string(&json!(fixtures)),
    )
    .unwrap();
}
