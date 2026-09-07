use luma_patterns::{Envelope, EnvelopeCurve};

fn curved() -> Envelope {
    Envelope {
        points: vec![[0., 0.], [1., 1.]],
        curves: vec![EnvelopeCurve::Bezier {
            control1: [0., 1. / 3.],
            control2: [0., 2. / 3.],
        }],
    }
}

#[test]
fn bezier_sampling_solves_time_instead_of_treating_it_as_the_parameter() {
    let e = curved();
    e.validate().unwrap();
    for x in [0., 0.001, 0.125, 0.5, 1.] {
        assert!((e.sample(x) - x.cbrt()).abs() < 1e-10);
    }
    assert_eq!(e.sample(-1.), 0.);
    assert_eq!(e.sample(2.), 1.);
}

#[test]
fn inserting_anchors_preserves_the_entire_curve_and_roundtrips() {
    let original = curved();
    let mut split = original.clone();
    split.insert_point(0.25).unwrap();
    split.insert_point(0.75).unwrap();
    let split: Envelope = serde_json::from_str(&serde_json::to_string(&split).unwrap()).unwrap();
    for i in 0..1001 {
        let x = i as f64 / 1000.;
        assert!((original.sample(x) - split.sample(x)).abs() < 1e-9, "{x}");
    }
}

#[test]
fn handles_can_shape_a_flat_segment_without_adding_anchors() {
    let e = Envelope {
        points: vec![[0., 0.], [1., 0.]],
        curves: vec![EnvelopeCurve::Bezier {
            control1: [1. / 3., 1.],
            control2: [2. / 3., 1.],
        }],
    };
    e.validate().unwrap();
    assert!((e.sample(0.5) - 0.75).abs() < 1e-10);
    assert_eq!(e.points.len(), 2);
}

#[test]
fn edits_keep_handles_valid_and_reject_invalid_changes_atomically() {
    let mut e = curved();
    let i = e.insert_point(0.5).unwrap();
    e.move_point(i, [0.7, 0.4]).unwrap();
    e.validate().unwrap();
    let before = e.clone();
    assert!(e.move_point(i, [f64::NAN, 0.]).is_err());
    assert!(e.move_point(i, [1., 0.]).is_err());
    assert!(e
        .set_curve(
            0,
            EnvelopeCurve::Bezier {
                control1: [0.8, 0.],
                control2: [0.1, 1.]
            }
        )
        .is_err());
    assert_eq!(e, before);
    e.remove_point(i).unwrap();
    assert_eq!(e.points.len(), 2);
    e.validate().unwrap();
    assert!(e.remove_point(0).is_err());
}

#[test]
fn saved_linear_envelopes_remain_linear_and_invalid_handle_payloads_fail() {
    let source = r#"{"points":[[0.0,1.0],[1.0,0.0]]}"#;
    let e: Envelope = serde_json::from_str(source).unwrap();
    assert_eq!(e.sample(0.25), 0.75);
    assert_eq!(serde_json::to_string(&e).unwrap(), source);
    let mut e = curved();
    e.curves.push(EnvelopeCurve::Linear);
    assert!(e.validate().is_err());
    e.curves.pop();
    e.curves[0] = EnvelopeCurve::Bezier {
        control1: [0., f64::INFINITY],
        control2: [1., 1.],
    };
    assert!(e.validate().is_err());
}
