use luma_patterns::*;
use ndarray::Array3;
use std::collections::BTreeMap;

fn frame() -> Frame<'static> {
    Frame {
        cells: &[],
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    }
}
#[test]
fn power_broadcasts_fixture_time_and_channels_without_losing_identity() {
    let base = Signal::new(
        Array3::from_shape_vec((2, 1, 3), vec![2., 3., 4., 5., 6., 7.]).unwrap(),
        Unit::Proportion,
        Channels::Rgb,
        Some(vec!["z".into(), "a".into()].into()),
    )
    .unwrap();
    let exponent = Signal::new(
        Array3::from_shape_vec((1, 3, 1), vec![0., 1., 2.]).unwrap(),
        Unit::Number,
        Channels::Value,
        None,
    )
    .unwrap();
    let graph = PreparedGraph::new(
        &standard_library(),
        "core/power",
        &BTreeMap::from([
            ("base".into(), Value::Signal(base)),
            ("exponent".into(), Value::Signal(exponent)),
        ]),
        frame(),
    )
    .unwrap();
    let result = graph.evaluate_batch(&[0., 1., 2.]).unwrap();
    let signal = result["value"].signal().unwrap();
    assert_eq!(signal.fixtures().unwrap(), &["a", "z"]);
    assert_eq!(*signal.channels(), Channels::Rgb);
    assert_eq!(signal.unit(), Unit::Number);
    assert_eq!(signal.values().dim(), (2, 3, 3));
    assert_eq!(
        signal.values().iter().copied().collect::<Vec<_>>(),
        vec![1., 1., 1., 5., 6., 7., 25., 36., 49., 1., 1., 1., 2., 3., 4., 4., 9., 16.]
    );
}
#[test]
fn power_rejects_physical_units_nonreal_results_and_overflow() {
    for (base, exponent) in [
        (Value::Beats(2.), Value::Number(2.)),
        (Value::Number(2.), Value::Degrees(2.)),
        (Value::Number(-1.), Value::Number(0.5)),
        (Value::Number(0.), Value::Number(-1.)),
        (Value::Number(1e200), Value::Number(2.)),
    ] {
        assert!(PreparedGraph::new(
            &standard_library(),
            "core/power",
            &BTreeMap::from([("base".into(), base), ("exponent".into(), exponent),]),
            frame()
        )
        .is_err());
    }
    let graph = PreparedGraph::new(
        &standard_library(),
        "core/power",
        &BTreeMap::from([
            ("base".into(), Value::Number(-2.)),
            ("exponent".into(), Value::Number(3.)),
        ]),
        frame(),
    )
    .unwrap();
    assert_eq!(
        graph.evaluate_batch(&[0.]).unwrap()["value"]
            .signal()
            .unwrap()
            .values()[[0, 0, 0]],
        -8.
    );
    let library = standard_library();
    let graph = Graph {
        nodes: BTreeMap::from([(
            "pow".into(),
            Node {
                definition: "core/power".into(),
                position: None,
                inputs: BTreeMap::from([
                    ("base".into(), Value::Beats(2.).into()),
                    ("exponent".into(), Value::Number(2.).into()),
                ]),
            },
        )]),
        ..Default::default()
    };
    assert!(library
        .binding_type(
            &BTreeMap::new(),
            &graph,
            &Binding::Connection {
                node: "pow".into(),
                output: "value".into(),
            }
        )
        .unwrap_err()
        .0
        .contains("dimensionless"));
}
