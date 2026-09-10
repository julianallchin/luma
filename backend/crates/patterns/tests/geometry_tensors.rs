use luma_patterns::*;
use ndarray::{Array3, Axis};
use std::collections::BTreeMap;
fn cells() -> Vec<Cell> {
    ["d", "c", "b", "a"]
        .into_iter()
        .enumerate()
        .map(|(i, id)| Cell {
            id: id.into(),
            group: "all".into(),
            world: [i as f64, 4., -2.],
            uvz: [1., 2., 3.],
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    }
}
#[test]
fn geometry_retains_world_coordinates_and_selection_order_by_fixture_identity() {
    let cells = cells();
    let graph = PreparedGraph::new(
        &standard_library(),
        "fixture_geometry",
        &BTreeMap::new(),
        frame(&cells),
    )
    .unwrap();
    assert_eq!(graph.dynamic_step_count(), 0);
    let out = graph.evaluate_batch(&[2., 0., 1.]).unwrap();
    let positions = out["position"].signal().unwrap();
    let index = out["index"].signal().unwrap();
    assert_eq!(positions.fixtures().unwrap(), &["a", "b", "c", "d"]);
    for (row, i) in (0..4).rev().enumerate() {
        assert_eq!(positions.values()[[row, 0, 0]], i as f64);
        assert_eq!(positions.values()[[row, 0, 1]], 4.);
        assert_eq!(positions.values()[[row, 0, 2]], -2.);
        assert_eq!(index.values()[[row, 0, 0]], i as f64);
    }
}
#[test]
fn fixture_reductions_keep_time_channels_units_and_terminate_the_fixture_axis() {
    let cells = cells();
    let input = Signal::new(
        Array3::from_shape_vec(
            (4, 2, 2),
            vec![
                10., 20., 30., 40., 10., 60., 70., 80., 90., 60., 30., 120., 130., 140., 150., 40.,
            ],
        )
        .unwrap(),
        Unit::Degrees,
        Channels::PanTilt,
        Some(
            cells
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>()
                .into(),
        ),
    )
    .unwrap();
    for (op, expected, unit) in [
        (
            "core/field_minimum",
            vec![10., 20., 30., 40.],
            Unit::Degrees,
        ),
        (
            "core/field_maximum",
            vec![130., 140., 150., 120.],
            Unit::Degrees,
        ),
        ("core/field_mean", vec![60., 70., 70., 70.], Unit::Degrees),
        ("core/head_count", vec![4.; 4], Unit::Number),
        ("core/distinct_count", vec![3., 3., 3., 3.], Unit::Number),
    ] {
        let program = PreparedGraph::new(
            &standard_library(),
            op,
            &BTreeMap::from([("value".into(), Value::Signal(input.clone()))]),
            frame(&cells),
        )
        .unwrap();
        let values = program.evaluate_batch(&[0., 1.]).unwrap();
        let result = values["value"].signal().unwrap();
        assert_eq!(result.values().dim(), (1, 2, 2));
        assert_eq!(
            result.values().iter().copied().collect::<Vec<_>>(),
            expected,
            "{op}"
        );
        assert_eq!(result.unit(), unit);
        assert_eq!(*result.channels(), Channels::PanTilt);
        assert!(result.fixtures().is_none());
    }
    // A broadcast constant can be ranked across the host domain; it must not
    // panic merely because its literal carries no fixture axis.
    let ranked = PreparedGraph::new(
        &standard_library(),
        "core/rank",
        &BTreeMap::from([("value".into(), Value::Number(1.))]),
        frame(&cells),
    )
    .unwrap()
    .evaluate_batch(&[0.])
    .unwrap();
    assert_eq!(
        ranked["value"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![0., 1., 2., 3.]
    );
}
#[test]
fn nearby_ranks_and_circle_fits_are_independent_per_sample() {
    let cells = cells();
    let ids = Some(
        cells
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
            .into(),
    );
    let position = Signal::new(
        Array3::from_shape_vec(
            (4, 1, 3),
            vec![0., 0., 0., 0.1, 0., 0., 1., 0., 0., 1.1, 0., 0.],
        )
        .unwrap(),
        Unit::Number,
        Channels::components(3).unwrap(),
        ids.clone(),
    )
    .unwrap();
    let order = Signal::new(
        Array3::from_shape_vec((4, 2, 1), vec![0., 3., 1., 2., 2., 1., 3., 0.]).unwrap(),
        Unit::Number,
        Channels::Value,
        ids.clone(),
    )
    .unwrap();
    let rank = PreparedGraph::new(
        &standard_library(),
        "core/rank_nearby",
        &BTreeMap::from([
            ("position".into(), Value::Signal(position)),
            ("value".into(), Value::Signal(order)),
            ("tolerance".into(), Value::Number(0.5)),
        ]),
        frame(&cells),
    )
    .unwrap()
    .evaluate_batch(&[0., 1.])
    .unwrap();
    // Output rows are a,b,c,d, the reverse of the input arrays above.
    assert_eq!(
        rank["value"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![1., 0., 1., 0., 0., 1., 0., 1.]
    );
    let position = Signal::new(
        Array3::from_shape_fn((4, 2, 3), |(n, t, c)| {
            let angle = n as f64 * std::f64::consts::FRAC_PI_2;
            match c {
                0 => angle.cos() * (t + 1) as f64 + 2.,
                1 => angle.sin() * (t + 1) as f64,
                _ => 3.,
            }
        }),
        Unit::Number,
        Channels::components(3).unwrap(),
        ids.clone(),
    )
    .unwrap();
    let run = |position| {
        PreparedGraph::new(
            &standard_library(),
            "core/fit_circle",
            &BTreeMap::from([("position".into(), Value::Signal(position))]),
            frame(&cells),
        )
        .unwrap()
        .evaluate_batch(&[0., 1.])
        .unwrap()
    };
    let batch = run(position.clone());
    for t in 0..2 {
        let sample = Signal::new(
            position.values().select(Axis(1), &[t]),
            position.unit(),
            *position.channels(),
            ids.clone(),
        )
        .unwrap();
        let sample = run(sample);
        for n in 0..4 {
            assert_eq!(
                sample["phase"].signal().unwrap().values()[[n, 0, 0]],
                batch["phase"].signal().unwrap().values()[[n, t, 0]]
            );
        }
    }
}

#[test]
fn point_algorithms_keep_their_own_domain_and_do_not_treat_order_as_distance() {
    let cells = cells();
    let domain = Some(vec!["x".into(), "y".into()].into());
    let order = Signal::new(
        Array3::from_shape_vec((2, 1, 1), vec![1e200, 0.]).unwrap(),
        Unit::Number,
        Channels::Value,
        domain.clone(),
    )
    .unwrap();
    for op in [
        "core/fit_circle",
        "core/radial_coordinates",
        "core/principal_direction",
    ] {
        let channels = if op == "core/principal_direction" {
            2
        } else {
            3
        };
        let position = Signal::new(
            Array3::from_shape_fn((2, 1, channels), |(n, _, c)| {
                if c == 0 {
                    if n == 0 {
                        1.
                    } else {
                        -1.
                    }
                } else {
                    0.
                }
            }),
            Unit::Number,
            Channels::components(channels).unwrap(),
            domain.clone(),
        )
        .unwrap();
        let out = PreparedGraph::new(
            &standard_library(),
            op,
            &BTreeMap::from([
                ("position".into(), Value::Signal(position)),
                ("order".into(), Value::Signal(order.clone())),
            ]),
            frame(&cells),
        )
        .unwrap()
        .evaluate_batch(&[0.])
        .unwrap();
        if op == "core/principal_direction" {
            let direction = out["direction"].signal().unwrap();
            assert!(direction.fixtures().is_none());
            assert_eq!(
                direction.values().iter().copied().collect::<Vec<_>>(),
                vec![1., 0.]
            );
        } else {
            let phase = out["phase"].signal().unwrap();
            assert_eq!(phase.fixtures().unwrap(), &["x", "y"]);
            assert_eq!(
                phase.values().iter().copied().collect::<Vec<_>>(),
                vec![0.75, 0.25]
            );
            if op == "core/radial_coordinates" {
                assert_eq!(
                    out["radius"]
                        .signal()
                        .unwrap()
                        .values()
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    vec![1., 1.]
                );
            }
        }
    }
}
#[test]
fn first_fixture_uses_explicit_order_per_sample_and_preserves_all_channels() {
    let value = Signal::new(
        Array3::from_shape_vec((2, 1, 2), vec![10., 20., 30., 40.]).unwrap(),
        Unit::Degrees,
        Channels::PanTilt,
        Some(vec!["z".into(), "a".into()].into()),
    )
    .unwrap();
    let order = Signal::new(
        Array3::from_shape_vec((2, 3, 1), vec![0., 3., 0., 1., 2., 0.]).unwrap(),
        Unit::Number,
        Channels::Value,
        Some(vec!["z".into(), "a".into()].into()),
    )
    .unwrap();
    let graph = PreparedGraph::new(
        &standard_library(),
        "core/field_first",
        &BTreeMap::from([
            ("value".into(), Value::Signal(value)),
            ("order".into(), Value::Signal(order)),
        ]),
        frame(&[]),
    )
    .unwrap();
    let output = graph.evaluate_batch(&[0., 1., 2.]).unwrap();
    let value = output["value"].signal().unwrap();
    assert_eq!(
        value.values().iter().copied().collect::<Vec<_>>(),
        vec![10., 20., 30., 40., 30., 40.]
    );
    assert_eq!(value.unit(), Unit::Degrees);
    assert_eq!(*value.channels(), Channels::PanTilt);
    assert!(value.fixtures().is_none());
    // No temporary channel is appended: even the widest legal vector works.
    for fixtures in [
        None,
        Some(Vec::<String>::new().into()),
        Some(vec!["x".into()].into()),
    ] {
        let n = fixtures
            .as_ref()
            .map_or(1, |f: &std::sync::Arc<[String]>| f.len());
        let value = Signal::new(
            Array3::from_elem((n, 1, 65_535), 7.),
            Unit::Number,
            Channels::components(65_535).unwrap(),
            fixtures,
        )
        .unwrap();
        let graph = PreparedGraph::new(
            &standard_library(),
            "core/field_first",
            &BTreeMap::from([("value".into(), Value::Signal(value))]),
            frame(&[]),
        )
        .unwrap();
        let output = graph.evaluate_batch(&[0.]).unwrap();
        let value = output["value"].signal().unwrap();
        assert_eq!(value.values().dim(), (1, 1, 65_535));
        assert!(value
            .values()
            .iter()
            .all(|v| *v == if n == 0 { 0. } else { 7. }));
    }
}
