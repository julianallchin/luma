use luma_patterns::*;
use ndarray::{Array3, Axis};
use std::collections::BTreeMap;
fn vector(values: Vec<f64>) -> Value {
    let channels = Channels::components(values.len()).unwrap();
    Value::Signal(
        Signal::new(
            Array3::from_shape_vec((1, 1, values.len()), values).unwrap(),
            Unit::Number,
            channels,
            None,
        )
        .unwrap(),
    )
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 8.,
        seed: 0,
    }
}
fn node(definition: &str, inputs: impl IntoIterator<Item = (&'static str, Binding)>) -> Node {
    Node {
        definition: definition.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        position: None,
    }
}
fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}

#[test]
fn moving_sites_remain_bounded_and_geometry_is_independent_of_seek_order() {
    let cells: Vec<_> = (0..4)
        .map(|n| Cell {
            id: format!("head-{n}"),
            group: "all".into(),
            world: [n as f64 / 3., 0.5, 0.5],
            uvz: [0.; 3],
        })
        .collect();
    let mut library = standard_library();
    let mut graph = Graph::default();
    graph.nodes.insert("clock".into(), node("clip_time", []));
    graph.nodes.insert(
        "seconds".into(),
        node(
            "core/multiply",
            [
                ("a", wire("clock", "beat")),
                ("b", Value::Seconds(1.).into()),
            ],
        ),
    );
    graph.nodes.insert(
        "sites".into(),
        node(
            "wander_points",
            [
                ("time", wire("seconds", "value")),
                ("count", Value::Number(3.).into()),
            ],
        ),
    );
    graph
        .nodes
        .insert("geometry".into(), node("fixture_geometry", []));
    graph.nodes.insert(
        "weights".into(),
        node(
            "proximity_weights",
            [
                ("position", wire("geometry", "position")),
                ("points", wire("sites", "points")),
            ],
        ),
    );
    for (port, node) in [("points", "sites"), ("weights", "weights")] {
        graph.outputs.insert(port.into(), wire(node, port));
    }
    library.definitions.insert(
        "test".into(),
        Definition {
            name: "Sites".into(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([
                (
                    "points".into(),
                    library.definitions["wander_points"].outputs["points"].clone(),
                ),
                (
                    "weights".into(),
                    library.definitions["proximity_weights"].outputs["weights"].clone(),
                ),
            ]),
            body: Body::Graph(graph),
        },
    );
    let prepare = |cells: &[Cell]| {
        PreparedGraph::new(&library, "test", &BTreeMap::new(), frame(cells)).unwrap()
    };
    let program = prepare(&cells);
    let times = [31., -2., 0., 1., 31.];
    let batch = program.evaluate_batch(&times).unwrap();
    assert_eq!(batch["points"].signal().unwrap().values().dim(), (1, 5, 9));
    assert!(batch["points"]
        .signal()
        .unwrap()
        .values()
        .iter()
        .all(|v| (0. ..=1.).contains(v)));
    let weights = batch["weights"].signal().unwrap();
    assert_eq!(weights.values().dim(), (4, 5, 3));
    for sum in weights.values().sum_axis(Axis(2)) {
        assert!((sum - 1.).abs() < 1e-12);
    }
    for (t, time) in times.iter().enumerate() {
        let single = program.evaluate_batch(&[*time]).unwrap();
        for port in ["points", "weights"] {
            assert_eq!(
                batch[port]
                    .signal()
                    .unwrap()
                    .values()
                    .index_axis(Axis(1), t),
                single[port]
                    .signal()
                    .unwrap()
                    .values()
                    .index_axis(Axis(1), 0)
            );
        }
    }
    let mut reversed = cells.clone();
    reversed.reverse();
    let reversed = prepare(&reversed).evaluate_batch(&times).unwrap();
    assert_eq!(weights, reversed["weights"].signal().unwrap());
    let empty = prepare(&[]).evaluate_batch(&times).unwrap();
    assert_eq!(empty["weights"].signal().unwrap().values().dim(), (0, 5, 3));
    assert_eq!(
        program.evaluate_batch(&[]).unwrap()["weights"]
            .signal()
            .unwrap()
            .values()
            .dim(),
        (4, 0, 3)
    );
}

#[test]
fn proximity_accepts_custom_sites_and_position_samples_without_fixtures() {
    let library = standard_library();
    for (temperature, expected) in [(0., 1.), (0.25, 1. / (1. + (-2_f64).exp()))] {
        let inputs = BTreeMap::from([
            ("position".into(), vector(vec![0.25, 0., 0.])),
            ("points".into(), vector(vec![0., 0., 0., 1., 0., 0.])),
            ("temperature".into(), Value::Number(temperature)),
        ]);
        let program =
            PreparedGraph::new(&library, "proximity_weights", &inputs, frame(&[])).unwrap();
        let values = program.evaluate_batch(&[0.]).unwrap();
        let weights = values["weights"].signal().unwrap();
        assert!(weights.fixtures().is_none());
        assert!((weights.values()[[0, 0, 0]] - expected).abs() < 1e-12);
        assert!((weights.values()[[0, 0, 1]] - (1. - expected)).abs() < 1e-12);
    }
    let inputs = BTreeMap::from([
        ("position".into(), vector(vec![0.; 3])),
        ("points".into(), vector(vec![0.; 6])),
        ("temperature".into(), Value::Number(0.)),
    ]);
    let program = PreparedGraph::new(&library, "proximity_weights", &inputs, frame(&[])).unwrap();
    assert_eq!(
        program.evaluate_batch(&[0.]).unwrap()["weights"]
            .signal()
            .unwrap()
            .values()
            .as_slice()
            .unwrap(),
        [0.5, 0.5]
    );
    for count in [0., 0.5, 4097.] {
        assert!(PreparedGraph::new(
            &library,
            "wander_points",
            &BTreeMap::from([("count".into(), Value::Number(count))]),
            frame(&[])
        )
        .is_err());
    }
}

#[test]
fn palettes_keep_opacity_and_perceptual_mix_exposes_it_as_a_separate_signal() {
    let gradient: Gradient = serde_json::from_str(
        r#"{"stops":[{"t":0,"color":[1,0,0],"alpha":0.2},{"t":1,"color":[0,0,1],"alpha":0.8}]}"#,
    )
    .unwrap();
    gradient.validate().unwrap();
    assert!((gradient.sample_alpha(0.5) - 0.5).abs() < 1e-12);
    let saved = serde_json::to_string(&gradient).unwrap();
    assert_eq!(serde_json::from_str::<Gradient>(&saved).unwrap(), gradient);
    let opaque: Gradient =
        serde_json::from_str(r#"{"stops":[{"t":0,"color":[0,0,0]},{"t":1,"color":[1,1,1]}]}"#)
            .unwrap();
    assert_eq!(opaque.sample_alpha(0.25), 1.);
    assert!(!serde_json::to_string(&opaque).unwrap().contains("alpha"));
    for perceptual in [false, true] {
        let program = PreparedGraph::new(
            &standard_library(),
            "mix_palette",
            &BTreeMap::from([
                ("gradient".into(), Value::Gradient(gradient.clone())),
                ("weights".into(), vector(vec![0.25, 0.75])),
                ("perceptual".into(), Value::Boolean(perceptual)),
                ("vibrance".into(), Value::Number(0.)),
            ]),
            frame(&[]),
        )
        .unwrap();
        let values = program.evaluate_batch(&[0.]).unwrap();
        assert!((values["opacity"].signal().unwrap().values()[[0, 0, 0]] - 0.65).abs() < 1e-12);
        let color = values["color"].signal().unwrap().values();
        if perceptual {
            assert!((color[[0, 0, 0]] - 0.25).abs() > 0.01);
        } else {
            assert_eq!(color.as_slice().unwrap(), [0.25, 0., 0.75]);
        }
    }
}
