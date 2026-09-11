use luma_patterns::*;
use std::collections::BTreeMap;

fn graph() -> Definition {
    Definition {
        name: "Signal graph".into(),
        inputs: BTreeMap::new(),
        outputs: BTreeMap::new(),
        body: Body::Graph(Graph::default()),
    }
}
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
fn bind(
    def: &mut Definition,
    lib: &Library,
    node: &str,
    input: &str,
    binding: Binding,
) -> Result<()> {
    def.edit(
        lib,
        GraphEdit::Bind {
            node: node.into(),
            input: input.into(),
            binding: Some(binding),
        },
    )
}
fn add(def: &mut Definition, lib: &Library, id: &str, node: &str) {
    def.edit(
        lib,
        GraphEdit::Add {
            id: id.into(),
            definition: node.into(),
        },
    )
    .unwrap();
}

#[test]
fn intensity_and_rgb_compose_directly_into_an_output() {
    let mut library = standard_library();
    let mut def = graph();
    add(&mut def, &library, "mapping", "resolve_mapping");
    add(&mut def, &library, "chase", "chase");
    add(&mut def, &library, "tint", "core/multiply");
    add(&mut def, &library, "output", "output");
    bind(
        &mut def,
        &library,
        "chase",
        "mapping",
        wire("mapping", "coordinates"),
    )
    .unwrap();
    bind(
        &mut def,
        &library,
        "chase",
        "trigger",
        Value::Events(Events::Beats {
            times: EventTimes::new(vec![1.0, 2.0]).unwrap(),
        })
        .into(),
    )
    .unwrap();
    bind(
        &mut def,
        &library,
        "chase",
        "width",
        Value::Proportion(0.3).into(),
    )
    .unwrap();
    bind(&mut def, &library, "tint", "a", wire("chase", "mask")).unwrap();
    bind(
        &mut def,
        &library,
        "tint",
        "b",
        Value::Color([0.8, 0.2, 0.4]).into(),
    )
    .unwrap();
    let Body::Graph(body) = &def.body else {
        panic!()
    };
    let (kind, _) = library
        .binding_type(&def.inputs, body, &wire("tint", "value"))
        .unwrap();
    assert_eq!(
        kind,
        ValueType::Signal(SignalType::new(Unit::Proportion, Channels::Rgb))
    );
    let before = def.clone();
    assert!(
        bind(&mut def, &library, "output", "pan", wire("tint", "value"))
            .unwrap_err()
            .0
            .contains("expected")
    );
    assert_eq!(def, before, "reject incompatible channels atomically");
    bind(&mut def, &library, "output", "color", wire("tint", "value")).unwrap();
    let Body::Graph(body) = &def.body else {
        panic!()
    };
    assert_eq!(
        body.nodes.len(),
        4,
        "no scalar/field/color broadcast adapter nodes"
    );
    assert_eq!(body.outputs["lighting"], wire("output", "lighting"));
    library.definitions.insert("test".into(), def);
    let cells = cells();
    let program = PreparedGraph::new(&library, "test", &BTreeMap::new(), frame(&cells)).unwrap();
    let times = [2.5, 1.0, 4.0, 1.5];
    let batch = program.evaluate_batch(&times).unwrap();
    let output = batch["lighting"].lighting().unwrap();
    assert_eq!(output.values().dim(), (5, 4, 8));
    assert_eq!(output.writes(), [true, true, false, false, false]);
    let mapping = MappingSpec {
        source: MappingSource::Z,
        mirror: None,
        per_group: false,
        reverse: false,
    }
    .resolve(&cells)
    .unwrap();
    let mask = PreparedGraph::new(
        &library,
        "chase",
        &BTreeMap::from([
            ("mapping".into(), Value::Coordinates(mapping)),
            (
                "trigger".into(),
                Value::Events(Events::Beats {
                    times: EventTimes::new(vec![1.0, 2.0]).unwrap(),
                }),
            ),
            ("width".into(), Value::Proportion(0.3)),
        ]),
        frame(&cells),
    )
    .unwrap()
    .evaluate_batch(&times)
    .unwrap();
    let mask = mask["mask"].signal().unwrap();
    for (t, _) in times.iter().enumerate() {
        let values = output.sample(t).unwrap();
        for (n, id) in mask.fixtures().unwrap().iter().enumerate() {
            for (actual, color) in values[id].rgb().into_iter().zip([0.8, 0.2, 0.4]) {
                assert!((actual - color * mask.values()[[n, t, 0]]).abs() < 1e-12);
            }
        }
    }
}

#[test]
fn optional_output_sockets_distinguish_unwritten_zero_and_clear() {
    let mut library = standard_library();
    let mut def = graph();
    add(&mut def, &library, "output", "output");
    let before = def.clone();
    assert!(def
        .edit(
            &library,
            GraphEdit::Add {
                id: "second".into(),
                definition: "output".into()
            }
        )
        .is_err());
    assert_eq!(def, before);
    let cells = cells();
    let sample = |library: &mut Library, def: &Definition| {
        library.definitions.insert("test".into(), def.clone());
        PreparedGraph::new(library, "test", &BTreeMap::new(), frame(&cells))
            .unwrap()
            .evaluate_batch(&[0.0])
            .unwrap()["lighting"]
            .lighting()
            .unwrap()
            .clone()
    };
    assert_eq!(sample(&mut library, &def).writes(), [false; 5]);
    bind(
        &mut def,
        &library,
        "output",
        "color",
        Value::Color([0.0; 3]).into(),
    )
    .unwrap();
    let zero = sample(&mut library, &def);
    assert_eq!(zero.writes(), [true, true, false, false, false]);
    assert!(zero
        .sample(0)
        .unwrap()
        .values()
        .all(|v| v.dimmer == Some(0.0)));
    bind(
        &mut def,
        &library,
        "output",
        "pan",
        Value::Degrees(42.0).into(),
    )
    .unwrap();
    let pan = sample(&mut library, &def);
    assert!(pan
        .sample(0)
        .unwrap()
        .values()
        .all(|v| v.position == Some([42.0, 0.0])));
    def.edit(
        &library,
        GraphEdit::Bind {
            node: "output".into(),
            input: "color".into(),
            binding: None,
        },
    )
    .unwrap();
    assert_eq!(
        sample(&mut library, &def).writes(),
        [false, false, true, false, false]
    );
}

#[test]
fn generic_input_inherits_a_bound_color_and_keeps_its_label_independent() {
    let library = standard_library();
    let mut def = graph();
    add(&mut def, &library, "multiply", "core/multiply");
    bind(
        &mut def,
        &library,
        "multiply",
        "b",
        Value::Color([0.25, 0.5, 0.75]).into(),
    )
    .unwrap();
    def.edit(
        &library,
        GraphEdit::AddInput {
            key: "tint".into(),
            name: "Accent".into(),
            position: [0.0, 0.0],
        },
    )
    .unwrap();
    bind(
        &mut def,
        &library,
        "multiply",
        "b",
        Binding::Input {
            input: "tint".into(),
        },
    )
    .unwrap();
    assert_eq!(
        def.inputs["tint"].value_type,
        ValueType::Signal(SignalType::new(Unit::Proportion, Channels::Rgb))
    );
    assert_eq!(
        def.inputs["tint"].default,
        Some(Value::Color([0.25, 0.5, 0.75]))
    );
    def.edit(
        &library,
        GraphEdit::RenameInput {
            key: "tint".into(),
            name: "Snare color".into(),
        },
    )
    .unwrap();
    assert_eq!(def.inputs["tint"].name, "Snare color");
    assert!(!def.inputs["tint"].optional);
}

#[test]
fn required_numeric_input_gets_an_editable_broadcast_default() {
    let mut library = standard_library();
    let mut def = graph();
    add(&mut def, &library, "clamp", "core/clamp_coverage");
    add(&mut def, &library, "output", "output");
    def.edit(
        &library,
        GraphEdit::AddInput {
            key: "level".into(),
            name: "Level".into(),
            position: [0.0, 0.0],
        },
    )
    .unwrap();
    bind(
        &mut def,
        &library,
        "clamp",
        "value",
        Binding::Input {
            input: "level".into(),
        },
    )
    .unwrap();
    assert_eq!(
        def.inputs["level"].value_type,
        ValueType::Signal(SignalType::new(Unit::Number, Channels::Value))
    );
    assert_eq!(def.inputs["level"].default, Some(Value::Number(0.0)));
    add(&mut def, &library, "tint", "core/multiply");
    bind(&mut def, &library, "tint", "a", wire("clamp", "mask")).unwrap();
    bind(
        &mut def,
        &library,
        "tint",
        "b",
        Value::Color([1.0; 3]).into(),
    )
    .unwrap();
    bind(&mut def, &library, "output", "color", wire("tint", "value")).unwrap();
    library.definitions.insert("test".into(), def);
    let cells = cells();
    let args = BTreeMap::from([("level".into(), Value::Number(0.6))]);
    let result = PreparedGraph::new(&library, "test", &args, frame(&cells))
        .unwrap()
        .evaluate_batch(&[0.0, 2.0])
        .unwrap();
    let result = result["lighting"].lighting().unwrap();
    for time in 0..2 {
        assert!(result
            .sample(time)
            .unwrap()
            .values()
            .all(|head| head.dimmer == Some(0.6)));
    }
}

#[test]
fn dimensions_flow_through_generic_nested_graphs_and_fail_at_the_wire() {
    let mut library = standard_library();
    let inner = library.definitions["core/multiply"].instance("core/multiply");
    library.definitions.insert("scaled".into(), inner);
    let mut def = graph();
    add(&mut def, &library, "scaled", "scaled");
    add(&mut def, &library, "output", "output");
    bind(&mut def, &library, "scaled", "a", Value::Beats(2.0).into()).unwrap();
    bind(&mut def, &library, "scaled", "b", Value::Number(3.0).into()).unwrap();
    let Body::Graph(body) = &def.body else {
        panic!()
    };
    assert_eq!(
        library
            .binding_type(&def.inputs, body, &wire("scaled", "value"))
            .unwrap()
            .0,
        ValueType::Signal(SignalType::new(Unit::Beats, Channels::Value))
    );
    assert!(bind(
        &mut def,
        &library,
        "output",
        "color",
        wire("scaled", "value")
    )
    .is_err());
    // Changing an upstream operand also invalidates the typed output rather
    // than concealing its dimensionality behind a generic Signal label.
    bind(&mut def, &library, "scaled", "b", Value::Beats(3.0).into()).unwrap();
    assert!(bind(
        &mut def,
        &library,
        "output",
        "color",
        wire("scaled", "value")
    )
    .is_err());
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
fn channelwise_clamp_comparison_and_choice_keep_tensor_shape() {
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
        position: None,
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
        (
            "choose".into(),
            node(
                "core/choose",
                vec![
                    ("condition", wire("compare", "mask")),
                    ("yes", Value::Number(8.).into()),
                    ("no", Value::Number(-3.).into()),
                ],
            ),
        ),
        (
            "channel".into(),
            node(
                "core/channel",
                vec![
                    ("value", wire("clamp", "mask")),
                    ("index", Value::Number(1.).into()),
                ],
            ),
        ),
    ]);
    let outputs = BTreeMap::from([
        ("clamped".into(), wire("clamp", "mask")),
        ("chosen".into(), wire("choose", "value")),
        ("green".into(), wire("channel", "value")),
    ]);
    let body = Graph {
        nodes,
        outputs,
        ..Default::default()
    };
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
        ("chosen", vec![-3., 8., 8., 8., -3., -3.], Channels::Rgb),
        ("green", vec![0.5, 0.2], Channels::Value),
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
fn numerical_ports_reject_invalid_channel_indices_and_comparison_units() {
    let library = standard_library();
    let cells = cells();
    for index in [-1., 0.5, 3.] {
        let error = PreparedGraph::new(
            &library,
            "core/channel",
            &BTreeMap::from([
                ("value".into(), Value::Color([0.2, 0.4, 0.6])),
                ("index".into(), Value::Number(index)),
            ]),
            frame(&cells),
        )
        .unwrap_err();
        assert!(error.0.contains("channel index"), "{error}");
    }
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
