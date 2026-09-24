use luma_patterns::*;
use ndarray::{array, Array3};

#[test]
fn signal_broadcasting_preserves_domains_units_and_channels() {
    let ids = Some(vec!["left".into(), "right".into()].into());
    let envelope = Signal::new(
        array![[[0.25], [0.75]]],
        Unit::Proportion,
        Channels::Value,
        None,
    )
    .unwrap();
    let colors = Signal::new(
        array![[[1., 0., 0.]], [[0., 0., 1.]]],
        Unit::Proportion,
        Channels::Rgb,
        ids,
    )
    .unwrap();
    let result = colors
        .zip(&envelope, Unit::Proportion, |a, b| a * b)
        .unwrap();
    assert_eq!(
        result.values(),
        &array![
            [[0.25, 0., 0.], [0.75, 0., 0.]],
            [[0., 0., 0.25], [0., 0., 0.75]]
        ]
    );
    assert_eq!(result.channels(), &Channels::Rgb);
    let foreign = Signal::new(
        Array3::zeros((2, 1, 3)),
        Unit::Proportion,
        Channels::Rgb,
        Some(vec!["right".into(), "left".into()].into()),
    )
    .unwrap();
    assert!(colors.zip(&foreign, Unit::Number, f64::max).is_err());
    let position = Signal::new(
        Array3::zeros((1, 1, 2)),
        Unit::Degrees,
        Channels::PanTilt,
        None,
    )
    .unwrap();
    assert!(colors.zip(&position, Unit::Number, f64::max).is_err());
    assert_eq!(
        serde_json::from_str::<Signal>(&serde_json::to_string(&result).unwrap()).unwrap(),
        result
    );
}

