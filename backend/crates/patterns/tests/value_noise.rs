use luma_patterns::*;
use ndarray::Array3;
use std::collections::BTreeMap;

fn frame(cells: &[Cell], seed: u64) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed,
    }
}
#[test]
fn seeds_roundtrip_exactly_and_do_not_become_floating_point_numbers() {
    for seed in [0, 1, 9_007_199_254_740_993, u64::MAX - 1, u64::MAX] {
        let value = Value::Seed(seed);
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["value"], seed.to_string());
        assert_eq!(serde_json::from_value::<Value>(json).unwrap(), value);
        assert_eq!(value.scalar_value(), None);
        assert!(!ValueType::Number.accepts(value.value_type()));
    }
    for value in ["-1", "18446744073709551616", "1.5", "NaN", ""] {
        assert!(
            serde_json::from_value::<Value>(serde_json::json!({"type":"seed","value":value}))
                .is_err()
        );
    }
}
#[test]
fn lattice_noise_broadcasts_all_axes_and_uses_every_seed_bit() {
    let library = standard_library();
    let x = Signal::new(
        Array3::from_shape_vec((2, 1, 2), vec![-0.75, 0.35, 1.25, 0.6]).unwrap(),
        Unit::Number,
        Channels::components(2).unwrap(),
        Some(vec!["a".into(), "z".into()].into()),
    )
    .unwrap();
    let y = Signal::new(
        Array3::from_shape_vec((1, 2, 1), vec![0.2, -0.8]).unwrap(),
        Unit::Number,
        Channels::Value,
        None,
    )
    .unwrap();
    let octaves = Signal::new(
        Array3::from_shape_vec((1, 2, 1), vec![1., 8.]).unwrap(),
        Unit::Number,
        Channels::Value,
        None,
    )
    .unwrap();
    for (definition, position) in [
        ("core/value_noise_1d", "position"),
        ("core/value_noise_3d", "x"),
    ] {
        let mut args = BTreeMap::from([
            (position.into(), Value::Signal(x.clone())),
            ("octaves".into(), Value::Signal(octaves.clone())),
            ("seed".into(), Value::Seed(u64::MAX)),
        ]);
        if position == "x" {
            args.insert("y".into(), Value::Signal(y.clone()));
        }
        let graph = PreparedGraph::new(&library, definition, &args, frame(&[], 0)).unwrap();
        let output = graph.evaluate_batch(&[2., 0.]).unwrap();
        let values = output["value"].signal().unwrap();
        assert_eq!(values.values().dim(), (2, 2, 2));
        assert_eq!(values.fixtures(), x.fixtures());
        for n in 0..2 {
            for t in 0..2 {
                for c in 0..2 {
                    let mut scalar = BTreeMap::from([
                        (position.into(), Value::Number(x.values()[[n, 0, c]])),
                        ("octaves".into(), Value::Number(octaves.values()[[0, t, 0]])),
                        ("seed".into(), Value::Seed(u64::MAX)),
                    ]);
                    if position == "x" {
                        scalar.insert("y".into(), Value::Number(y.values()[[0, t, 0]]));
                    }
                    let output =
                        PreparedGraph::new(&library, definition, &scalar, frame(&[], 1234))
                            .unwrap()
                            .evaluate_batch(&[0.])
                            .unwrap();
                    assert_eq!(
                        values.values()[[n, t, c]],
                        output["value"].signal().unwrap().values()[[0, 0, 0]]
                    );
                }
            }
        }
        args.insert("seed".into(), Value::Seed(u64::MAX - 1));
        let changed = PreparedGraph::new(&library, definition, &args, frame(&[], 0))
            .unwrap()
            .evaluate_batch(&[2., 0.])
            .unwrap();
        assert_ne!(values, changed["value"].signal().unwrap());
        args.insert(position.into(), Value::Number(1e13));
        assert!(PreparedGraph::new(&library, definition, &args, frame(&[], 0)).is_err());
    }
    let stream = |seed| {
        PreparedGraph::new(
            &library,
            "core/seed_stream",
            &BTreeMap::from([
                ("seed".into(), Value::Seed(seed)),
                ("stream".into(), Value::Seed(u64::MAX)),
            ]),
            frame(&[], 0),
        )
        .unwrap()
        .evaluate(0.)
        .unwrap()["seed"]
            .clone()
    };
    assert_ne!(stream(u64::MAX), stream(u64::MAX - 1));
}
#[test]
fn domain_alignment_is_explicit_and_indices_follow_the_prepared_selection() {
    let cells: Vec<_> = ["z", "a"]
        .into_iter()
        .map(|id| Cell {
            id: id.into(),
            group: "all".into(),
            world: [0.; 3],
            uvz: [0.; 3],
        })
        .collect();
    let value = Signal::new(
        Array3::from_shape_vec((2, 1, 1), vec![10., 20.]).unwrap(),
        Unit::Degrees,
        Channels::Value,
        Some(vec!["a".into(), "z".into()].into()),
    )
    .unwrap();
    let library = standard_library();
    let index = PreparedGraph::new(
        &library,
        "core/domain_index",
        &BTreeMap::from([("value".into(), Value::Signal(value.clone()))]),
        frame(&cells, 0),
    )
    .unwrap()
    .evaluate_batch(&[0.])
    .unwrap();
    assert_eq!(
        index["value"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![1., 0.]
    );
    let inputs = BTreeMap::from([
        ("value".into(), Value::Signal(value.clone())),
        ("reference".into(), Value::Number(1.)),
        (
            "order".into(),
            Value::Signal(index["value"].signal().unwrap().clone()),
        ),
    ]);
    let first = PreparedGraph::new(&library, "core/align_domain", &inputs, frame(&cells, 0))
        .unwrap()
        .evaluate_batch(&[0.])
        .unwrap();
    assert_eq!(first["value"].signal().unwrap().values()[[0, 0, 0]], 20.);
    assert!(first["value"].signal().unwrap().fixtures().is_none());
    let inputs = BTreeMap::from([
        ("value".into(), Value::Degrees(30.)),
        ("reference".into(), Value::Signal(value.clone())),
    ]);
    let broadcast = PreparedGraph::new(&library, "core/align_domain", &inputs, frame(&cells, 0))
        .unwrap()
        .evaluate_batch(&[0.])
        .unwrap();
    assert_eq!(
        broadcast["value"].signal().unwrap().fixtures(),
        value.fixtures()
    );
    assert_eq!(
        broadcast["value"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![30., 30.]
    );
    assert!(PreparedGraph::new(
        &library,
        "core/domain_index",
        &BTreeMap::from([("value".into(), Value::Signal(value))]),
        frame(&[], 0)
    )
    .unwrap_err()
    .0
    .contains("not in the prepared selection"));
}
