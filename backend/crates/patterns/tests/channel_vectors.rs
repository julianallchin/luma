use luma_patterns::*;
use ndarray::Array3;
use std::collections::BTreeMap;

#[test]
fn joining_channels_broadcasts_fixture_and_time_axes_and_keeps_the_domain() {
    let clock = Signal::new(
        Array3::from_shape_vec((1, 3, 1), vec![1., 2., 3.]).unwrap(),
        Unit::Number,
        Channels::Value,
        None,
    )
    .unwrap();
    let field = Signal::new(
        Array3::from_shape_vec((2, 1, 1), vec![10., 20.]).unwrap(),
        Unit::Number,
        Channels::Value,
        Some(vec!["a".into(), "b".into()].into()),
    )
    .unwrap();
    let joined = clock.join_channels(&field).unwrap();
    assert_eq!(joined.values().dim(), (2, 3, 2));
    assert_eq!(*joined.channels(), Channels::components(2).unwrap());
    assert_eq!(joined.fixtures(), field.fixtures());
    assert_eq!(
        joined.values().iter().copied().collect::<Vec<_>>(),
        &[1., 10., 2., 10., 3., 10., 1., 20., 2., 20., 3., 20.]
    );
    let wrong_domain = Signal::new(
        Array3::zeros((2, 1, 1)),
        Unit::Number,
        Channels::Value,
        Some(vec!["b".into(), "a".into()].into()),
    )
    .unwrap();
    assert!(joined
        .join_channels(&wrong_domain)
        .unwrap_err()
        .0
        .contains("fixture domains"));
    assert!(clock
        .join_channels(&Signal::scalar(2., Unit::Degrees).unwrap())
        .is_ok());
    assert!(Signal::scalar(2., Unit::Beats)
        .unwrap()
        .join_channels(&Signal::scalar(90., Unit::Degrees).unwrap())
        .is_err());
}

#[test]
fn vectors_have_arbitrary_nonzero_width_and_named_sockets_adopt_matching_channels() {
    let spectrum = Signal::new(
        Array3::from_elem((1, 1, 12), 0.25),
        Unit::Number,
        Channels::components(12).unwrap(),
        None,
    )
    .unwrap();
    let rgb = Signal::new(
        Array3::from_shape_vec((1, 1, 3), vec![0.2, 0.5, 1.]).unwrap(),
        Unit::Proportion,
        Channels::Rgb,
        None,
    )
    .unwrap();
    let extended = spectrum.join_channels(&rgb).unwrap();
    assert_eq!(extended.values().dim(), (1, 1, 15));
    let vector = Signal::new(
        Array3::from_shape_vec((1, 1, 3), vec![2., 3., 4.]).unwrap(),
        Unit::Number,
        Channels::components(3).unwrap(),
        None,
    )
    .unwrap();
    let tinted = vector.zip(&rgb, Unit::Proportion, |a, b| a * b).unwrap();
    assert_eq!(*tinted.channels(), Channels::Rgb);
    assert_eq!(tinted.values().as_slice().unwrap(), &[0.4, 1.5, 4.]);
    assert!(
        SignalType::new(Unit::Proportion, Channels::Rgb).accepts(SignalType::new(
            Unit::Number,
            Channels::components(3).unwrap()
        ))
    );
    assert!(
        !SignalType::new(Unit::Proportion, Channels::Rgb).accepts(SignalType::new(
            Unit::Number,
            Channels::components(2).unwrap()
        ))
    );
    assert!(Channels::components(0).is_err());
    assert!(Channels::components(65_536).is_err());
    assert!(serde_json::from_str::<Channels>(r#"{"components":0}"#).is_err());
    // A single-fixture sample is still a field, not an editable broadcast
    // constant. Extracting it as a scalar would discard its fixture identity.
    let single_head = Signal::new(
        Array3::from_elem((1, 1, 1), 0.25),
        Unit::Number,
        Channels::Value,
        Some(vec!["head".into()].into()),
    )
    .unwrap();
    assert_eq!(Value::Signal(single_head).scalar_value(), None);
    let cells = vec![Cell {
        id: "head".into(),
        group: "all".into(),
        world: [0.; 3],
        uvz: [0.; 3],
    }];
    let input = Signal::new(
        Array3::from_shape_vec((1, 1, 3), vec![0.2, 0.4, 0.8]).unwrap(),
        Unit::Number,
        Channels::components(3).unwrap(),
        None,
    )
    .unwrap();
    let output = PreparedGraph::new(
        &standard_library(),
        "output",
        &BTreeMap::from([("color".into(), Value::Signal(input))]),
        Frame {
            cells: &cells,
            features: None,
            beat: 0.,
            clip_start: 0.,
            clip_duration: 4.,
            seed: 0,
        },
    )
    .unwrap()
    .evaluate_batch(&[0.])
    .unwrap();
    assert_eq!(
        output["lighting"].lighting().unwrap().sample(0).unwrap()["head"].rgb(),
        [0.2, 0.4, 0.8]
    );
}

#[test]
fn graph_inference_and_execution_agree_on_joined_channels() {
    let mut library = standard_library();
    let mut graph = Graph::default();
    graph.nodes.insert(
        "join".into(),
        Node {
            position: None,
            definition: "core/join_channels".into(),
            inputs: BTreeMap::from([
                ("a".into(), Value::Number(1.2).into()),
                ("b".into(), Value::Number(-0.7).into()),
            ]),
        },
    );
    let binding = Binding::Connection {
        node: "join".into(),
        output: "value".into(),
    };
    let (kind, rate) = library
        .binding_type(&BTreeMap::new(), &graph, &binding)
        .unwrap();
    assert_eq!(
        kind,
        ValueType::Signal(SignalType::new(
            Unit::Number,
            Channels::components(2).unwrap()
        ))
    );
    graph.outputs.insert("vector".into(), binding);
    library.definitions.insert(
        "vector".into(),
        Definition {
            name: "Vector".into(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([(
                "vector".into(),
                Output {
                    value_type: kind,
                    rate,
                },
            )]),
            body: Body::Graph(graph),
        },
    );
    let frame = Frame {
        cells: &[],
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    let prepared = PreparedGraph::new(&library, "vector", &BTreeMap::new(), frame).unwrap();
    let values = prepared.evaluate_batch(&[2., 0., 3.]).unwrap();
    let signal = values["vector"].signal().unwrap();
    assert_eq!(signal.values().as_slice().unwrap(), &[1.2, -0.7]);
    assert_eq!(*signal.channels(), Channels::components(2).unwrap());
}
