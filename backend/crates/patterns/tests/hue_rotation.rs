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
fn strongest_channel_reduces_only_channels_and_breaks_ties_at_the_first_index() {
    let input = Signal::new(
        Array3::from_shape_vec(
            (2, 3, 4),
            vec![
                -5., -2., -2., -4., 0., 0., 0., 0., 1., 2., 3., 4., 4., 3., 2., 1., 0., -0., -1.,
                -2., -5., -4., -3., -2.,
            ],
        )
        .unwrap(),
        Unit::Degrees,
        Channels::components(4).unwrap(),
        Some(vec!["a".into(), "b".into()].into()),
    )
    .unwrap();
    let result = PreparedGraph::new(
        &standard_library(),
        "core/channel_argmax",
        &BTreeMap::from([("value".into(), Value::Signal(input))]),
        frame(),
    )
    .unwrap()
    .evaluate_batch(&[2., 0., 2.])
    .unwrap();
    let value = result["value"].signal().unwrap();
    assert_eq!(value.values().dim(), (2, 3, 1));
    assert_eq!(value.fixtures().unwrap(), &["a", "b"]);
    assert_eq!(value.unit(), Unit::Number);
    assert_eq!(*value.channels(), Channels::Value);
    assert_eq!(
        value.values().as_slice().unwrap(),
        &[1., 0., 3., 0., 0., 3.]
    );
}

#[test]
fn hue_rotation_broadcasts_over_time_and_heads_with_wrapping_in_both_directions() {
    let colors = Signal::new(
        Array3::from_shape_vec((2, 1, 3), vec![1., 0., 0., 0.4, 0.4, 0.4]).unwrap(),
        Unit::Proportion,
        Channels::Rgb,
        Some(vec!["a".into(), "b".into()].into()),
    )
    .unwrap();
    let turns = Signal::new(
        Array3::from_shape_vec((1, 4, 1), vec![0., 1. / 3., -1. / 3., 7. / 3.]).unwrap(),
        Unit::Number,
        Channels::Value,
        None,
    )
    .unwrap();
    let result = PreparedGraph::new(
        &standard_library(),
        "rotate_hue",
        &BTreeMap::from([
            ("color".into(), Value::Signal(colors)),
            ("turns".into(), Value::Signal(turns)),
        ]),
        frame(),
    )
    .unwrap()
    .evaluate_batch(&[3., 0., 1., 3.])
    .unwrap();
    let color = result["color"].signal().unwrap();
    assert_eq!(color.values().dim(), (2, 4, 3));
    assert_eq!(color.fixtures().unwrap(), &["a", "b"]);
    assert_eq!(color.unit(), Unit::Proportion);
    assert_eq!(*color.channels(), Channels::Rgb);
    for (t, rgb) in [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [0., 1., 0.]]
        .into_iter()
        .enumerate()
    {
        for (c, expected) in rgb.into_iter().enumerate() {
            assert!((color.values()[[0, t, c]] - expected).abs() < 1e-12);
            assert_eq!(color.values()[[1, t, c]], 0.4);
        }
    }
}

#[test]
fn hue_rotation_preserves_extrema_and_is_reversible_without_clipping() {
    let original = Signal::new(
        Array3::from_shape_vec((1, 1, 3), vec![-0.5, 0.3, 2.]).unwrap(),
        Unit::Proportion,
        Channels::Rgb,
        None,
    )
    .unwrap();
    let apply = |color: Signal, turns| {
        let result = PreparedGraph::new(
            &standard_library(),
            "rotate_hue",
            &BTreeMap::from([
                ("color".into(), Value::Signal(color)),
                ("turns".into(), Value::Number(turns)),
            ]),
            frame(),
        )
        .unwrap()
        .evaluate_batch(&[0.])
        .unwrap();
        result["color"].signal().unwrap().clone()
    };
    for turns in [-3.25, -0.16, 0., 0.21, 6.75] {
        let rotated = apply(original.clone(), turns);
        assert_eq!(
            rotated
                .values()
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min),
            -0.5
        );
        assert_eq!(
            rotated
                .values()
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
            2.
        );
        let restored = apply(rotated, -turns);
        for (a, b) in original.values().iter().zip(restored.values()) {
            assert!((a - b).abs() < 1e-12);
        }
    }
    let empty = Signal::new(
        Array3::zeros((0, 1, 3)),
        Unit::Proportion,
        Channels::Rgb,
        Some(vec![].into()),
    )
    .unwrap();
    assert_eq!(apply(empty, 0.5).values().dim(), (0, 1, 3));
    assert!(PreparedGraph::new(
        &standard_library(),
        "rotate_hue",
        &BTreeMap::from([
            ("color".into(), Value::Color([1., 0., 0.])),
            ("turns".into(), Value::Degrees(90.)),
        ]),
        frame(),
    )
    .is_err());
}
