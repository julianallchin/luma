//! Capture shared palette semantics from the original category evaluator.
use crate::models::node_graph::{
    BeatGrid, Edge, Graph, NodeInstance, PatternArgDef, PatternArgType,
};
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
fn capture_original_shared_palettes() {
    let grid = BeatGrid {
        beats: (0..13).map(|i| i as f32 * 0.5).collect(),
        downbeats: vec![0., 2., 4., 6.],
        bpm: 120.,
        beats_per_bar: 4,
        downbeat_offset: 0.,
    };
    let harmony = vec![
        (0., 1., Some(0)),
        (1., 2., Some(4)),
        (2., 3., Some(7)),
        (3., 4., Some(11)),
    ];
    let times = vec![0., 0.25, 1.25, 2.25, 3.25, 4.25, 0.25];
    let positions = vec![[-1., 0., 0.], [0., 0., 1.], [1., 1., 2.]];
    let ids: Vec<String> = ["z", "a", "m"].into_iter().map(str::to_owned).collect();
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
    let context = crate::eval::ResidentContext {
        beat_grid: Some(grid.clone()),
        span: (0., 6.),
        positions,
        chord_sections: harmony.clone(),
        ..Default::default()
    };
    let mut cases = Vec::new();
    for (name, kind, palette) in [
        (
            "empty-palette",
            PatternArgType::Palette,
            json!({"colors":[]}),
        ),
        (
            "empty-gradient",
            PatternArgType::Gradient,
            json!({"stops":[]}),
        ),
        (
            "one-palette",
            PatternArgType::Palette,
            json!({"colors":["#ff000033"]}),
        ),
        (
            "one-gradient",
            PatternArgType::Gradient,
            json!({"stops":[{"t":0.7,"color":"#00ff0080"}]}),
        ),
        (
            "two-colors",
            PatternArgType::Palette,
            json!({"colors":["#ff000033","#0000ffcc"]}),
        ),
    ] {
        for share_second in [false, true] {
            let mut graph = Graph {
                nodes: vec![
                    node("args", "pattern_args", json!({})),
                    node("harmony", "harmony_analysis", json!({})),
                    node(
                        "a",
                        "chroma_palette",
                        json!({"fallback_palette":{"colors":["#0000ff"]}}),
                    ),
                    node(
                        "b",
                        "chroma_palette",
                        json!({"fallback_palette":{"colors":["#00ff00"]}}),
                    ),
                    node("position", "scalar", json!({"value":0.3})),
                    node("sample", "sample_palette", json!({})),
                    node("regions", "soft_voronoi", json!({"num_points":3})),
                    node("out", "apply_color", json!({})),
                ],
                edges: vec![
                    edge("harmony", "signal", "a", "chroma"),
                    edge("harmony", "signal", "b", "chroma"),
                    edge("position", "out", "sample", "u"),
                    edge("a", "out", "out", "signal"),
                ],
                args: vec![PatternArgDef {
                    id: "colors".into(),
                    name: "Shared colors".into(),
                    arg_type: kind.clone(),
                    default_value: palette.clone(),
                }],
            };
            for target in ["a", "sample", "regions"] {
                graph.edges.push(edge("args", "colors", target, "stops"));
            }
            if share_second {
                graph.edges.push(edge("args", "colors", "b", "stops"));
            }
            for source in ["a", "b", "sample", "regions"] {
                let view = format!("view_{source}");
                graph.nodes.push(node(&view, "view_signal", json!({})));
                graph.edges.push(edge(source, "out", &view, "in"));
            }
            let plan = crate::eval::compile::compile_pattern(
                &graph.nodes,
                &graph.edges,
                &HashMap::from([("colors".into(), palette.clone())]),
                context.clone(),
                ids.clone(),
            )
            .unwrap();
            assert!(plan
                .ops
                .iter()
                .all(|op| !matches!(op.kind, crate::eval::OpKind::Graph(_))));
            let frames =
                crate::eval::try_eval(&plan, &times, &mut crate::eval::Arena::default()).unwrap();
            let views =
                crate::eval::eval_views(&plan, &times, &mut crate::eval::Arena::default()).unwrap();
            cases.push(json!({"name":format!("{name}/share={share_second}"),"graph":graph,"frames":frames,"views":views}));
        }
    }
    std::fs::write(std::env::var("LUMA_CAPTURE_SHARED_PALETTES").expect("capture destination"),crate::canonical_json::to_string(&json!({"grid":grid,"times":times,"harmony":harmony,"cells":cells,"clip_start":0.,"clip_duration":12.,"cases":cases}))).unwrap();
}
