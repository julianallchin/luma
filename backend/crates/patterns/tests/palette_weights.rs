use luma_patterns::*;
use ndarray::Array3;
use std::collections::BTreeMap;

#[test]
fn empty_and_single_color_palettes_keep_their_meaning_and_fallback_is_explicit() {
    let empty = Gradient { stops: vec![] };
    let single = Gradient {
        stops: vec![ColorStop {
            t: 0.7,
            color: [1., 0., 0.],
            alpha: 0.25,
        }],
    };
    for palette in [&empty, &single] {
        palette.validate().unwrap();
        let decoded: Gradient =
            serde_json::from_value(serde_json::to_value(palette).unwrap()).unwrap();
        assert_eq!(&decoded, palette);
    }
    for at in [-1., 0., 0.7, 1., 2.] {
        assert_eq!(empty.sample(at), [0.; 3]);
        assert_eq!(empty.sample_alpha(at), 1.);
        assert_eq!(single.sample(at), [1., 0., 0.]);
        assert_eq!(single.sample_alpha(at), 0.25);
    }
    let library = standard_library();
    let frame = Frame {
        cells: &[],
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    let black = Gradient {
        stops: vec![ColorStop {
            t: 0.,
            color: [0.; 3],
            alpha: 1.,
        }],
    };
    for palette in [empty.clone(), single.clone(), black] {
        let program = PreparedGraph::new(
            &library,
            "palette_fallback",
            &BTreeMap::from([
                ("gradient".into(), Value::Gradient(palette.clone())),
                ("fallback".into(), Value::Gradient(single.clone())),
            ]),
            frame,
        )
        .unwrap();
        assert_eq!(program.dynamic_step_count(), 0);
        let result = program.evaluate_batch(&[3., 0., 3.]).unwrap();
        for t in 0..3 {
            let expected = if palette.stops.is_empty() {
                &single
            } else {
                &palette
            };
            assert_eq!(
                result["gradient"].sample(t).unwrap(),
                Value::Gradient(expected.clone())
            );
        }
    }
    for perceptual in [false, true] {
        let result = PreparedGraph::new(
            &library,
            "mix_palette",
            &BTreeMap::from([
                ("gradient".into(), Value::Gradient(empty.clone())),
                (
                    "weights".into(),
                    Value::Signal(Signal::vector(vec![0.25, 0.75], Unit::Number).unwrap()),
                ),
                ("perceptual".into(), Value::Boolean(perceptual)),
            ]),
            frame,
        )
        .unwrap()
        .evaluate_batch(&[0., 1.])
        .unwrap();
        assert!(result["color"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .all(|v| *v == 0.));
        assert!(result["opacity"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .all(|v| *v == 1.));
    }
    let result = PreparedGraph::new(
        &library,
        "sample_gradient",
        &BTreeMap::from([("gradient".into(), Value::Gradient(empty))]),
        frame,
    )
    .unwrap()
    .evaluate_batch(&[0., 1.])
    .unwrap();
    assert!(result["color"]
        .signal()
        .unwrap()
        .values()
        .iter()
        .all(|v| *v == 0.));
    assert!(result["opacity"]
        .signal()
        .unwrap()
        .values()
        .iter()
        .all(|v| *v == 1.));
}

#[test]
fn sampled_opacity_keeps_its_domain_and_can_drive_intensity_independently_of_rgb() {
    let cells: Vec<_> = ["b", "a"]
        .into_iter()
        .map(|id| Cell {
            id: id.into(),
            group: "bars".into(),
            world: [0.; 3],
            uvz: [0.; 3],
        })
        .collect();
    let frame = Frame {
        cells: &cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    let rgb = [0.2, 0.4, 0.8];
    let inputs = BTreeMap::from([
        (
            "gradient".into(),
            Value::Gradient(Gradient {
                stops: vec![
                    ColorStop {
                        t: 0.,
                        color: rgb,
                        alpha: 0.2,
                    },
                    ColorStop {
                        t: 1.,
                        color: rgb,
                        alpha: 0.8,
                    },
                ],
            }),
        ),
        (
            "position".into(),
            Value::Signal(
                Signal::new(
                    Array3::from_shape_vec((2, 2, 1), vec![0., 1., 0.5, 0.25]).unwrap(),
                    Unit::Proportion,
                    Channels::Value,
                    Some(vec!["b".into(), "a".into()].into()),
                )
                .unwrap(),
            ),
        ),
    ]);
    for id in ["sample_gradient", "sample_field_gradient"] {
        let mut library = standard_library();
        let values = PreparedGraph::new(&library, id, &inputs, frame)
            .unwrap()
            .evaluate_batch(&[3., 1.])
            .unwrap();
        let opacity = values["opacity"].signal().unwrap();
        assert_eq!(opacity.fixtures().unwrap(), &["a", "b"]);
        assert_eq!(opacity.values().dim(), (2, 2, 1));
        assert_eq!(opacity.unit(), Unit::Proportion);
        assert_eq!(*opacity.channels(), Channels::Value);
        for (value, expected) in opacity.values().iter().zip([0.5, 0.35, 0.2, 0.8]) {
            assert!((value - expected).abs() < 1e-12);
        }
        for (index, value) in values["color"].signal().unwrap().values().indexed_iter() {
            assert!((value - rgb[index.2]).abs() < 1e-6, "opacity changed RGB");
        }
        let mut definition = library.definitions[id].clip_instance(id).unwrap();
        let Body::Graph(graph) = &definition.body else {
            panic!()
        };
        assert!(
            graph.outputs.contains_key("opacity"),
            "placement lost an inspectable output"
        );
        assert!(!graph.nodes["output"].inputs.contains_key("dimmer"));
        definition
            .edit(
                &library,
                GraphEdit::Bind {
                    node: "output".into(),
                    input: "dimmer".into(),
                    binding: Some(Binding::Connection {
                        node: "effect".into(),
                        output: "opacity".into(),
                    }),
                },
            )
            .unwrap();
        library.definitions.insert("test".into(), definition);
        let values = PreparedGraph::new(&library, "test", &inputs, frame)
            .unwrap()
            .evaluate_batch(&[3., 1.])
            .unwrap();
        assert_eq!(
            values["lighting"].lighting().unwrap().writes(),
            [true, true, false, false, false]
        );
        for (time, expected) in [[0.5, 0.2], [0.35, 0.8]].into_iter().enumerate() {
            let lit = values["lighting"].lighting().unwrap().sample(time).unwrap();
            for (head, alpha) in ["a", "b"].into_iter().zip(expected) {
                assert!((lit[head].dimmer.unwrap() - alpha).abs() < 1e-12);
                for (value, color) in lit[head].rgb().into_iter().zip(rgb) {
                    assert!((value - alpha * color).abs() < 1e-6);
                }
            }
        }
    }
}

#[test]
fn palette_mixing_is_a_channel_projection_and_preserves_time_domain_and_headroom() {
    let weights = Signal::new(
        Array3::from_shape_vec((2, 2, 2), vec![1., 0., 0., 1., 2., 0.5, -1., 0.25]).unwrap(),
        Unit::Number,
        Channels::components(2).unwrap(),
        Some(vec!["z".into(), "a".into()].into()),
    )
    .unwrap();
    let inputs = BTreeMap::from([
        ("weights".into(), Value::Signal(weights.clone())),
        (
            "gradient".into(),
            Value::Gradient(Gradient {
                stops: vec![
                    ColorStop {
                        alpha: 1.,
                        t: 0.,
                        color: [1., 0., 0.],
                    },
                    ColorStop {
                        alpha: 1.,
                        t: 1.,
                        color: [0., 0., 1.],
                    },
                ],
            }),
        ),
    ]);
    let frame = Frame {
        cells: &[],
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    let program = PreparedGraph::new(&standard_library(), "mix_palette", &inputs, frame).unwrap();
    let result = program.evaluate_batch(&[0., 1.]).unwrap();
    let color = result["color"].signal().unwrap();
    assert_eq!(color.fixtures().unwrap(), &["a", "z"]);
    assert_eq!(color.values().dim(), (2, 2, 3));
    assert_eq!(*color.channels(), Channels::Rgb);
    assert_eq!(color.unit(), Unit::Proportion);
    assert_eq!(
        color.values().iter().copied().collect::<Vec<_>>(),
        vec![2., 0., 0.5, -1., 0., 0.25, 1., 0., 0., 0., 0., 1.]
    );
    let count = PreparedGraph::new(
        &standard_library(),
        "core/channel_count",
        &BTreeMap::from([("value".into(), Value::Signal(weights))]),
        frame,
    )
    .unwrap()
    .evaluate_batch(&[0., 1.])
    .unwrap();
    assert_eq!(count["value"].signal().unwrap().values()[[0, 0, 0]], 2.);
    let mut invalid = inputs;
    invalid.insert("weights".into(), Value::Degrees(2.));
    assert!(PreparedGraph::new(&standard_library(), "mix_palette", &invalid, frame).is_err());
}
