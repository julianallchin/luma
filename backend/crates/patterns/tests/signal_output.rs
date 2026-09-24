use luma_patterns::*;
use std::collections::BTreeMap;

fn cells() -> Vec<Cell> {
    (0..5)
        .map(|i| Cell {
            id: format!("head-{i}"),
            group: "bars".into(),
            world: [0.0, 0.0, i as f64],
            uvz: [0.0, 0.0, i as f64],
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.0,
        clip_start: 0.0,
        clip_duration: 8.0,
        seed: 1,
    }
}
fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
#[test]
fn optional_output_sockets_distinguish_unwritten_zero_and_clear() {
    let library = standard_library();
    let cells = cells();
    let sample = |inputs: &[(&str, Value)]| {
        let inputs = inputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        PreparedGraph::new(&library, "output", &inputs, frame(&cells))
            .unwrap()
            .evaluate_batch(&[0.0])
            .unwrap()["lighting"]
            .lighting()
            .unwrap()
            .clone()
    };
    assert_eq!(sample(&[]).writes(), [false; 6]);
    let zero = sample(&[("color", Value::Color([0.0; 3]))]);
    assert_eq!(zero.writes(), [true, true, false, false, false, false]);
    assert!(zero
        .sample(0)
        .unwrap()
        .values()
        .all(|v| v.dimmer == Some(0.0)));
    let pan = sample(&[("pan", Value::Degrees(42.0))]);
    assert_eq!(pan.writes(), [false, false, true, false, false, false]);
    assert!(pan
        .sample(0)
        .unwrap()
        .values()
        .all(|v| v.position == Some([42.0, 0.0])));
}

#[test]
fn applied_color_splits_into_chromaticity_and_brightness() {
    let library = standard_library();
    let cells = cells();
    let inputs = BTreeMap::from([("color".into(), Value::Color([0.2, 0.1, 0.0]))]);
    let output = PreparedGraph::new(&library, "output", &inputs, frame(&cells))
        .unwrap()
        .evaluate_batch(&[0.0])
        .unwrap()["lighting"]
        .lighting()
        .unwrap()
        .sample(0)
        .unwrap();
    assert_eq!(output["head-0"].color, Some([1.0, 0.5, 0.0]));
    assert_eq!(output["head-0"].dimmer, Some(0.2));
    for (a, b) in output["head-0"].rgb().into_iter().zip([0.2, 0.1, 0.0]) {
        assert!((a - b).abs() < 1e-12);
    }
}

#[test]
fn channel_maximum_reduces_only_channels_and_preserves_negative_values() {
    let library = standard_library();
    let values =
        ndarray::Array3::from_shape_vec((1, 2, 3), vec![-8.0, -3.0, -4.0, -1.0, -9.0, -2.0])
            .unwrap();
    let input = Signal::new(values, Unit::Number, Channels::Rgb, None).unwrap();
    let cells = cells();
    let output = PreparedGraph::new(
        &library,
        "core/channel_maximum",
        &BTreeMap::from([("value".into(), Value::Signal(input))]),
        frame(&cells),
    )
    .unwrap()
    .evaluate_batch(&[0.0, 1.0])
    .unwrap();
    let result = output["value"].signal().unwrap();
    assert_eq!(result.values().dim(), (1, 2, 1));
    assert_eq!(result.values().as_slice().unwrap(), &[-3.0, -1.0]);
    assert_eq!(result.channels(), &Channels::Value);
    assert_eq!(result.unit(), Unit::Number);
    assert!(result.fixtures().is_none());
}

#[test]
fn channelwise_clamp_and_comparison_keep_tensor_shape() {
    use ndarray::Array3;
    let mut library = standard_library();
    let source = Value::Signal(
        Signal::new(
            Array3::from_shape_vec((1, 2, 3), vec![-1., 0.5, 2., 0.8, 0.2, -0.3]).unwrap(),
            Unit::Number,
            Channels::Rgb,
            None,
        )
        .unwrap(),
    );
    let node = |definition: &str, inputs: Vec<(&str, Binding)>| Node {
        definition: definition.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    };
    let nodes = BTreeMap::from([
        (
            "clamp".into(),
            node(
                "core/clamp_coverage",
                vec![("value", source.clone().into())],
            ),
        ),
        (
            "compare".into(),
            node(
                "core/greater",
                vec![("a", source.into()), ("b", Value::Number(0.4).into())],
            ),
        ),
    ]);
    let outputs = BTreeMap::from([
        ("clamped".into(), wire("clamp", "mask")),
        ("compared".into(), wire("compare", "mask")),
    ]);
    let body = Graph { nodes, outputs };
    let outputs = body
        .outputs
        .iter()
        .map(|(key, binding)| {
            let (value_type, rate) = library
                .binding_type(&BTreeMap::new(), &body, binding)
                .unwrap();
            (key.clone(), Output { value_type, rate })
        })
        .collect();
    library.definitions.insert(
        "channels".into(),
        Definition {
            name: "Channels".into(),
            inputs: BTreeMap::new(),
            outputs,
            body: Body::Graph(body),
        },
    );
    let cells = cells();
    let prepared =
        PreparedGraph::new(&library, "channels", &BTreeMap::new(), frame(&cells)).unwrap();
    let values = prepared.evaluate_batch(&[0., 1.]).unwrap();
    for (key, expected, channels) in [
        ("clamped", vec![0., 0.5, 1., 0.8, 0.2, 0.], Channels::Rgb),
        ("compared", vec![0., 1., 1., 1., 0., 0.], Channels::Rgb),
    ] {
        let actual = values[key].signal().unwrap();
        assert_eq!(*actual.channels(), channels);
        assert_eq!(
            actual.values().iter().copied().collect::<Vec<_>>(),
            expected
        );
        assert_eq!(actual.values().dim().1, 2);
    }
}

#[test]
fn comparison_rejects_mismatched_units() {
    let library = standard_library();
    let cells = cells();
    let error = PreparedGraph::new(
        &library,
        "core/greater",
        &BTreeMap::from([
            ("a".into(), Value::Beats(2.)),
            ("b".into(), Value::Degrees(90.)),
        ]),
        frame(&cells),
    )
    .unwrap_err();
    assert!(error.0.contains("cannot Subtract"), "{error}");
}

#[test]
fn output_preserves_intensity_headroom_until_master_compositing() {
    let library = standard_library();
    let cells = cells();
    let color = Value::Signal(
        Signal::new(
            ndarray::Array3::from_shape_vec((1, 1, 3), vec![4., 2., 1.]).unwrap(),
            Unit::Proportion,
            Channels::Rgb,
            None,
        )
        .unwrap(),
    );
    {
        let (expected_color, expected_dimmer) = ([1., 0.5, 0.25], 4.);
        let inputs = BTreeMap::from([("color".into(), color.clone())]);
        let output = PreparedGraph::new(&library, "output", &inputs, frame(&cells))
            .unwrap()
            .evaluate_batch(&[0., 1.])
            .unwrap();
        let sample = output["lighting"].lighting().unwrap().sample(0).unwrap();
        for head in sample.values() {
            head.validate().unwrap();
            assert_eq!(head.color, Some(expected_color));
            assert_eq!(head.dimmer, Some(expected_dimmer));
        }
    }
}
