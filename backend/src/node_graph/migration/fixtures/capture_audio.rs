//! Independent original frequency/stem references; see fixtures/README.md.
use crate::models::node_graph::{BeatGrid, Edge, Graph, NodeInstance};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};

fn node(id: &str, kind: &str, params: serde_json::Value) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: kind.into(),
        params: serde_json::from_value(params).unwrap(),
        position_x: None,
        position_y: None,
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
#[test]
fn capture_original_audio_graphs() {
    let times = vec![0_f32, 0.001, 0.02, 0.08, 0.2, 0.25, 0.4, 0.08];
    let grid = BeatGrid {
        beats: (0..10).map(|i| i as f32 * 0.5).collect(),
        downbeats: vec![0., 2., 4.],
        bpm: 120.,
        downbeat_offset: 0.,
        beats_per_bar: 4,
    };
    let ranges = [
        json!([]),
        json!([[300, 700]]),
        json!("[[0,50],[700,2200]]"),
        json!([[300, 1000], [500, 1700]]),
        json!([[-40, 0], [22000, 25000]]),
        json!([[700, 300]]),
        json!("invalid"),
    ];
    let mut fixtures = Vec::new();
    for sample_rate in [8000, 44100, 44101] {
        let ranges = if sample_rate == 44101 {
            vec![
                json!([[11046.783203125, 11046.783203125]]),
                json!([[11046.7841796875, 11046.7841796875]]),
            ]
        } else {
            ranges.to_vec()
        };
        let mut audio = serde_json::Map::new();
        let mut resident = HashMap::new();
        for (index, name) in ["mix", "bass", "drums", "vocals", "other"]
            .into_iter()
            .enumerate()
        {
            let samples: Vec<f32> = (0..sample_rate / 4)
                .map(|i| {
                    let t = i as f32 / sample_rate as f32;
                    let frequency = if sample_rate == 44101 {
                        11046.783 + index as f32 * 20.
                    } else {
                        [440., 80., 1300., 770., 2500.][index]
                    };
                    let a = (t * frequency * std::f32::consts::TAU).sin();
                    let b = (t * frequency * 2.3 * std::f32::consts::TAU).sin();
                    (a * 0.7 + b * 0.2) * (0.3 + t * 2.)
                })
                .collect();
            audio.insert(
                name.into(),
                json!({"samples":samples,"sample_rate":sample_rate}),
            );
            resident.insert(
                name.to_string(),
                crate::eval::ResidentAudio {
                    samples: Arc::new(samples),
                    sample_rate,
                },
            );
        }
        for count in [0, 3] {
            let ids =
                vec!["head-z".to_string(), "head-a".into(), "head-m".into()][..count].to_vec();
            let positions = vec![[-0.7, 0., 1.], [0.9, 0., 0.2], [0.2, 0., 0.6]][..count].to_vec();
            let ctx = crate::eval::ResidentContext {
                audio: Some(resident["mix"].clone()),
                stems: resident
                    .iter()
                    .filter(|(name, _)| name.as_str() != "mix")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                beat_grid: Some(grid.clone()),
                span: (0., 0.5),
                positions: positions.clone(),
                ..Default::default()
            };
            let cells: Vec<_> = ids
                .iter()
                .zip(&positions)
                .map(|(id, pos)| luma_patterns::Cell {
                    id: id.clone(),
                    group: "all".into(),
                    world: pos.map(f64::from),
                    uvz: pos.map(f64::from),
                })
                .collect();
            let mut cases = Vec::new();
            for source in ["mix", "bass", "drums", "vocals", "other"] {
                for (index, ranges) in ranges.iter().enumerate() {
                    let graph = Graph {
                        nodes: vec![
                            node("audio", "audio_input", json!({})),
                            node("stems", "stem_splitter", json!({})),
                            node(
                                "band",
                                "frequency_amplitude",
                                json!({"selected_frequency_ranges":ranges}),
                            ),
                            node("view", "view_signal", json!({})),
                            node("writer", "apply_dimmer", json!({})),
                        ],
                        edges: vec![
                            edge("audio", "out", "stems", "audio_in"),
                            if source == "mix" {
                                edge("audio", "out", "band", "audio_in")
                            } else {
                                edge("stems", &format!("{source}_out"), "band", "audio_in")
                            },
                            edge("band", "amplitude_out", "view", "in"),
                            edge("band", "amplitude_out", "writer", "signal"),
                        ],
                        args: vec![],
                    };
                    let old = crate::eval::compile::compile_pattern(
                        &graph.nodes,
                        &graph.edges,
                        &HashMap::new(),
                        ctx.clone(),
                        ids.clone(),
                    )
                    .unwrap();
                    let frames =
                        crate::eval::eval(&old, &times, &mut crate::eval::Arena::default());
                    let views =
                        crate::eval::eval_views(&old, &times, &mut crate::eval::Arena::default())
                            .unwrap();
                    cases.push(json!({"name":format!("{source}-{index}-{sample_rate}-{count}"),"graph":graph,"frames":frames,"views":views}));
                }
            }
            fixtures.push(json!({"times":times,"grid":grid,"cells":cells,"clip_start":0.,"clip_duration":1.,"audio":audio,"cases":cases}));
        }
    }
    let path = std::env::var("LUMA_CAPTURE_AUDIO_MIGRATION").expect("capture destination");
    std::fs::write(path, crate::canonical_json::to_string(&json!(fixtures))).unwrap();
}
