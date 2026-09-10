use super::*;
use crate::models::node_graph::{Edge, PatternArgDef};

fn node(id: &str, kind: &str, params: serde_json::Value) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: kind.into(),
        params: serde_json::from_value(params).unwrap(),
        position_x: Some(120.),
        position_y: Some(80.),
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
fn mixed_graph() -> Graph {
    let typed = |id: &str, kind: &str| {
        node(
            id,
            &format!("{}{kind}", crate::node_graph::lighting::PREFIX),
            serde_json::json!({}),
        )
    };
    Graph {
        nodes: vec![
            node("args", "pattern_args", serde_json::json!({})),
            node(
                "wave",
                "sine_wave",
                serde_json::json!({"amplitude":0.5,"offset":0.5}),
            ),
            typed("scale", "core/multiply"),
            node(
                "offset",
                "math",
                serde_json::json!({"operation":"add", "b":1.}),
            ),
            typed("position", "write_position"),
            typed("clamp", "core/clamp_coverage"),
            typed("strobe", "write_strobe"),
            typed("combine", "add_lighting"),
            node("brightness", "apply_dimmer", serde_json::json!({})),
            node("view", "view_signal", serde_json::json!({})),
        ],
        edges: vec![
            edge("wave", "out", "scale", "a"),
            edge("args", "gain", "scale", "b"),
            edge("scale", "value", "offset", "a"),
            edge("offset", "out", "position", "pan"),
            edge("args", "gain", "position", "tilt"),
            edge("wave", "out", "clamp", "value"),
            edge("clamp", "mask", "strobe", "value"),
            edge("position", "lighting", "combine", "a"),
            edge("strobe", "lighting", "combine", "b"),
            edge("wave", "out", "brightness", "signal"),
            edge("offset", "out", "view", "in"),
        ],
        args: vec![PatternArgDef {
            id: "gain".into(),
            name: "Motion size".into(),
            arg_type: PatternArgType::Scalar,
            default_value: serde_json::json!({"value":2.}),
        }],
    }
}

#[test]
fn mixed_graphs_compose_signals_in_both_directions_and_preserve_capability_writes() {
    let graph = mixed_graph();
    let original = serde_json::to_string(&graph).unwrap();
    let mut score = crate::node_graph::migration::pattern(&graph, "Mixed motion")
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_string(&graph).unwrap(), original);
    let base = p::standard_library();
    let library = score.library(&base).unwrap();
    score
        .definitions
        .get_mut(ROOT)
        .unwrap()
        .edit(
            &library,
            p::GraphEdit::RenameInput {
                key: "gain".into(),
                name: "Sweep size".into(),
            },
        )
        .unwrap();
    let score: p::Score = serde_json::from_str(&serde_json::to_string(&score).unwrap()).unwrap();
    assert_eq!(score.definitions[ROOT].inputs["gain"].name, "Sweep size");
    let Body::Graph(root) = &score.definitions[ROOT].body else {
        panic!()
    };
    assert_eq!(root.nodes["position"].position, Some([120., 80.]));
    for (id, port) in [("scale", "b"), ("position", "tilt")] {
        assert_eq!(
            root.nodes[id].inputs[port],
            B::Input {
                input: "gain".into()
            }
        );
    }
    let mut terminals = 0;
    for (id, definition) in &score.definitions {
        assert!(
            definition
                .inputs
                .values()
                .all(|p| p.value_type != ValueType::Lighting),
            "{id}"
        );
        let Body::Graph(graph) = &definition.body else {
            panic!()
        };
        terminals += graph
            .nodes
            .values()
            .filter(|n| n.definition == "output")
            .count();
    }
    assert_eq!(terminals, 1);
    let grid = BeatGrid {
        beats: vec![0., 0.5, 1., 1.5, 2., 2.5],
        downbeats: vec![0., 2.],
        bpm: 120.,
        beats_per_bar: 4,
        downbeat_offset: 0.,
    };
    let clock = grid.timeline().unwrap();
    let features = std::sync::Arc::new(ReferenceFeatures {
        clock: clock.clone(),
        timing: std::sync::Arc::new(grid.timing().unwrap()),
        onsets: BTreeMap::new(),
        harmony: vec![],
        audio: BTreeMap::new(),
    });
    let cells = vec![
        p::Cell {
            id: "head-z".into(),
            group: "all".into(),
            world: [0.; 3],
            uvz: [0.; 3],
        },
        p::Cell {
            id: "head-a".into(),
            group: "all".into(),
            world: [1.; 3],
            uvz: [1.; 3],
        },
    ];
    let times = [1.25, 0.125, 0., 0.5, 0.125];
    let library = score.library(&base).unwrap();
    for gain in [2., 3.] {
        let inputs = BTreeMap::from([("gain".into(), Value::Number(gain))]);
        let program = p::PreparedGraph::new(
            &library,
            ROOT,
            &inputs,
            p::Frame {
                cells: &cells,
                features: None,
                beat: 0.,
                clip_start: 0.,
                clip_duration: 4.,
                seed: 0,
            },
        )
        .unwrap()
        .with_features(features.clone())
        .unwrap();
        let batch = program.evaluate_batch(&times).unwrap();
        for (i, beat) in times.into_iter().enumerate() {
            let wave = 0.5 + 0.5 * (std::f64::consts::TAU * beat).sin();
            let expected = wave * gain + 1.;
            let actual = batch["lighting"].lighting().unwrap().sample(i).unwrap();
            let single = program.evaluate_batch(&[beat]).unwrap();
            assert_eq!(
                single["lighting"].lighting().unwrap().sample(0).unwrap(),
                actual
            );
            for value in actual.values() {
                assert!((value.dimmer.unwrap() - wave).abs() < 1e-6);
                assert!((value.strobe.unwrap() - wave).abs() < 1e-6);
                let [pan, tilt] = value.position.unwrap();
                assert!((pan - expected).abs() < 1e-6);
                assert_eq!(tilt, gain);
                assert_eq!(value.color, None);
                assert_eq!(value.speed, None);
            }
            assert!(
                (batch["view/view"].signal().unwrap().values()[[0, i, 0]] - expected).abs() < 1e-6
            );
        }
    }
}

#[test]
fn mixed_graphs_reject_wrong_ports_and_duplicate_sources() {
    let mut graph = mixed_graph();
    graph.edges.push(edge("wave", "out", "position", "pan"));
    assert!(crate::node_graph::migration::pattern(&graph, "Invalid")
        .unwrap_err()
        .contains("multiple sources"));
    let mut graph = mixed_graph();
    graph
        .edges
        .iter_mut()
        .find(|e| e.to_node == "scale")
        .unwrap()
        .from_port = "missing".into();
    assert!(crate::node_graph::migration::pattern(&graph, "Invalid")
        .unwrap_err()
        .contains("no numerical source"));
    for (target, port) in [("position", "typo"), ("missing_node", "pan")] {
        let mut graph = mixed_graph();
        graph.edges.push(edge("wave", "out", target, port));
        assert!(crate::node_graph::migration::pattern(&graph, "Invalid").is_err());
    }
    let mut graph = mixed_graph();
    graph.nodes.push(graph.nodes[0].clone());
    assert!(crate::node_graph::migration::pattern(&graph, "Invalid")
        .unwrap_err()
        .contains("Duplicate node"));
    let mut graph = mixed_graph();
    graph
        .nodes
        .push(node("extra_strobe", "apply_strobe", serde_json::json!({})));
    graph
        .edges
        .push(edge("wave", "out", "extra_strobe", "signal"));
    assert!(crate::node_graph::migration::pattern(&graph, "Invalid")
        .unwrap_err()
        .contains("multiple strobe outputs"));
    let mut graph = mixed_graph();
    let score = crate::node_graph::migration::pattern(&graph, "Collision")
        .unwrap()
        .unwrap();
    let Body::Graph(root) = &score.definitions[ROOT].body else {
        panic!()
    };
    let collision = root.nodes["scale"]
        .definition
        .strip_prefix("node/")
        .unwrap();
    graph
        .nodes
        .push(node(collision, "scalar", serde_json::json!({})));
    graph.edges.push(edge("scale", "value", collision, "value"));
    assert!(crate::node_graph::migration::pattern(&graph, "Collision")
        .unwrap_err()
        .contains("Duplicate migrated definition"));
}
