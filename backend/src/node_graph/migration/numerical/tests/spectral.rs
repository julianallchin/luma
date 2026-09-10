use super::*;

#[derive(Deserialize)]
struct SpectralReference {
    rgb: [f64; 3],
    weights: [f64; 12],
    expected: [f64; 3],
}

fn node() -> NodeInstance {
    NodeInstance {
        id: "shift".into(),
        type_id: "spectral_shift".into(),
        params: std::collections::HashMap::from([("strength".into(), serde_json::json!(0.))]),
        position_x: None,
        position_y: None,
    }
}

fn cells() -> Vec<p::Cell> {
    ["z", "a"]
        .into_iter()
        .map(|id| p::Cell {
            id: id.into(),
            group: "all".into(),
            world: [0.; 3],
            uvz: [0.; 3],
        })
        .collect()
}

fn frame(cells: &[p::Cell]) -> p::Frame<'_> {
    p::Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    }
}

#[test]
fn spectral_shift_matches_original_hsl_rotation_with_compact_shared_operations() {
    let cases: Vec<SpectralReference> =
        serde_json::from_str(include_str!("../../fixtures/spectral-v1.json")).unwrap();
    assert_eq!(cases.len(), 153);
    let definition = harmony::lower(&node()).unwrap();
    let Body::Graph(body) = &definition.body else {
        panic!()
    };
    assert!(body.nodes.len() <= 12);
    assert_eq!(
        body.nodes
            .values()
            .filter(|n| n.definition == "rotate_hue")
            .count(),
        1
    );
    assert_eq!(
        body.nodes
            .values()
            .filter(|n| n.definition == "core/channel_argmax")
            .count(),
        1
    );
    // Strength never affected the historical operation.
    assert!(!definition.inputs.contains_key("strength"));
    let mut library = p::standard_library();
    library.definitions.insert("shift".into(), definition);
    let colors = signal(
        [1, cases.len(), 3],
        cases.iter().flat_map(|c| c.rgb).collect(),
        None,
    );
    let weights = signal(
        [1, cases.len(), 12],
        cases.iter().flat_map(|c| c.weights).collect(),
        None,
    );
    let cells = cells();
    let prepared = p::PreparedGraph::new(
        &library,
        "shift",
        &BTreeMap::from([
            ("in".into(), Value::Signal(colors)),
            ("chroma".into(), Value::Signal(weights)),
        ]),
        frame(&cells),
    )
    .unwrap();
    let result = prepared.evaluate_batch(&vec![0.; cases.len()]).unwrap();
    let output = result["out"].signal().unwrap();
    assert_eq!(output.values().dim(), (1, cases.len(), 3));
    for (time, case) in cases.iter().enumerate() {
        for (channel, expected) in case.expected.iter().enumerate() {
            assert!(
                (output.values()[[0, time, channel]] - expected).abs() < 3e-5,
                "case {time}: {:?}",
                case.rgb
            );
        }
    }
}

#[test]
fn spectral_shift_preserves_the_first_selected_head_and_accepts_rgba_inputs() {
    let mut library = p::standard_library();
    library
        .definitions
        .insert("shift".into(), harmony::lower(&node()).unwrap());
    let domain = Some(vec!["a".into(), "z".into()]);
    let colors = signal(
        [2, 1, 4],
        vec![0., 0., 1., 1., 1., 0., 0., 0.5],
        domain.clone(),
    );
    let mut weights = vec![0.; 24];
    weights[0] = 1.;
    weights[16] = 1.;
    let weights = signal([2, 1, 12], weights, domain);
    let cells = cells();
    let result = p::PreparedGraph::new(
        &library,
        "shift",
        &BTreeMap::from([
            ("in".into(), Value::Signal(colors)),
            ("chroma".into(), Value::Signal(weights)),
        ]),
        frame(&cells),
    )
    .unwrap()
    .evaluate_batch(&[2., 0., 2.])
    .unwrap();
    let output = result["out"].signal().unwrap();
    assert_eq!(output.values().dim().0, 1);
    assert_eq!(output.fixtures(), None);
    for rgb in output.values().as_slice().unwrap().chunks_exact(3) {
        for (actual, expected) in rgb.iter().zip([0., 1., 0.]) {
            assert!((actual - expected).abs() < 1e-12);
        }
    }
}

#[test]
fn connected_harmony_and_spectral_shift_migrate_together() {
    let mut graph: Graph = serde_json::from_value(serde_json::json!({
        "nodes":[
            {"id":"audio","typeId":"audio_input","params":{}},
            {"id":"harmony","typeId":"harmony_analysis","params":{}},
            {"id":"color","typeId":"color","params":{}},
            {"id":"shift","typeId":"spectral_shift","params":{"strength":0.}},
            {"id":"out","typeId":"apply_color","params":{}}
        ],
        "edges":[
            {"id":"audio","fromNode":"audio","fromPort":"out","toNode":"harmony","toPort":"audio_in"},
            {"id":"color","fromNode":"color","fromPort":"out","toNode":"shift","toPort":"in"},
            {"id":"chroma","fromNode":"harmony","fromPort":"signal","toNode":"shift","toPort":"chroma"},
            {"id":"out","fromNode":"shift","fromPort":"out","toNode":"out","toPort":"signal"}
        ], "args":[]
    })).unwrap();
    let score = crate::node_graph::migration::pattern(&graph, "Harmonic hue")
        .unwrap()
        .unwrap();
    score
        .library(&p::standard_library())
        .unwrap()
        .validate(ROOT)
        .unwrap();
    let grid = BeatGrid {
        beats: vec![0., 0.5, 1., 1.5, 2.],
        downbeats: vec![0., 2.],
        bpm: 120.,
        beats_per_bar: 4,
        downbeat_offset: 0.,
    };
    let cells = cells();
    let prepared = p::PreparedGraph::new(
        &score.library(&p::standard_library()).unwrap(),
        ROOT,
        &BTreeMap::new(),
        frame(&cells),
    )
    .unwrap()
    .with_features(std::sync::Arc::new(ReferenceFeatures {
        clock: grid.timeline().unwrap(),
        timing: std::sync::Arc::new(grid.timing().unwrap()),
        harmony: vec![(0., 0.5, Some(4)), (0.5, 2., Some(6))],
        onsets: BTreeMap::new(),
        audio: BTreeMap::new(),
    }))
    .unwrap();
    for (beat, expected) in [
        (1.5, [0., 1., 1.]),
        (0.25, [0., 1., 0.]),
        (1.5, [0., 1., 1.]),
    ] {
        let result = prepared.evaluate_batch(&[beat]).unwrap();
        for value in result["lighting"]
            .lighting()
            .unwrap()
            .sample(0)
            .unwrap()
            .values()
        {
            assert_eq!(value.dimmer, Some(1.));
            for (actual, expected) in value.rgb().into_iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-12);
            }
        }
    }
    graph.edges[0].from_node = "color".into();
    assert!(
        crate::node_graph::migration::pattern(&graph, "Invalid source")
            .unwrap_err()
            .contains("audio source")
    );
}

// Exercise the persisted tensor encoding without adding a second ndarray dependency.
fn signal(dim: [usize; 3], data: Vec<f64>, fixtures: Option<Vec<String>>) -> p::Signal {
    serde_json::from_value(serde_json::json!({
        "values":{"v":1,"dim":dim,"data":data},
        "unit":"number", "channels":{"components":dim[2]}, "fixtures":fixtures
    }))
    .unwrap()
}
