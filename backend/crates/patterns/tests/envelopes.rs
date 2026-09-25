use luma_patterns::{Ease, Envelope};

#[test]
fn a_hold_jumps_at_the_next_point() {
    let e = Envelope::eased(
        vec![[0., 1.], [0.5, 0.2], [1., 0.]],
        &[Ease::Hold, Ease::Hold],
    );
    e.validate().unwrap();
    for (time, expected) in [(0., 1.), (0.499, 1.), (0.5, 0.2), (0.999, 0.2), (1., 0.)] {
        assert_eq!(e.sample(time), expected);
    }
}

#[test]
fn editable_envelope_samples_each_vector_channel() {
    use luma_patterns::*;
    use std::collections::BTreeMap;
    let frame = Frame {
        cells: &[],
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 1.,
        seed: 0,
    };
    let library = standard_library();
    let shape = Envelope::linear(vec![[0., 0.], [0.25, 1.], [0.75, 0.4], [1., 0.]]);
    let result = PreparedGraph::new(
        &library,
        "envelope",
        &BTreeMap::from([
            ("shape".into(), Value::Envelope(shape.clone())),
            (
                "progress".into(),
                Value::Signal(
                    Signal::vector(vec![0., 0.125, 0.25, 0.5, 1.], Unit::Number).unwrap(),
                ),
            ),
        ]),
        frame,
    )
    .unwrap()
    .evaluate_batch(&[0.])
    .unwrap();
    let signal = result["value"].signal().unwrap();
    for (actual, expected) in signal.values().iter().zip([0., 0.5, 1., 0.7, 0.]) {
        assert!((actual - expected).abs() < 1e-12);
    }
}

fn curved() -> Envelope {
    Envelope::eased(
        vec![[0., 0.], [1., 1.]],
        &[Ease::Bezier([0., 1. / 3., 0., 2. / 3.])],
    )
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
fn an_ease_is_local_to_its_segment() {
    // The same ease on a rise and on a fall: the fall mirrors the rise.
    let e = Envelope::eased(
        vec![[0., 0.2], [0.5, 1.], [1., 0.2]],
        &[Ease::EaseIn, Ease::EaseIn],
    );
    e.validate().unwrap();
    for i in 0..=10 {
        let t = f64::from(i) / 10.;
        let rise = (e.sample(t * 0.5) - 0.2) / 0.8;
        let fall = (1. - e.sample(0.5 + t * 0.5)) / 0.8;
        assert!((rise - fall).abs() < 1e-9, "{t}");
        assert!((rise - Ease::EaseIn.apply(t)).abs() < 1e-9, "{t}");
    }
    // CSS ease-in-out is symmetric about the middle.
    assert!((Ease::EaseInOut.apply(0.5) - 0.5).abs() < 1e-9);
    assert!(Ease::EaseIn.apply(0.25) < 0.25 && Ease::EaseOut.apply(0.25) > 0.25);
}

#[test]
fn inserting_anchors_preserves_the_entire_curve_and_roundtrips() {
    for original in [
        curved(),
        Envelope::eased(vec![[0., 0.9], [1., 0.1]], &[Ease::EaseInOut]),
    ] {
        let mut split = original.clone();
        split.insert_point(0.25).unwrap();
        split.insert_point(0.75).unwrap();
        let split: Envelope =
            serde_json::from_str(&serde_json::to_string(&split).unwrap()).unwrap();
        for i in 0..1001 {
            let x = i as f64 / 1000.;
            assert!((original.sample(x) - split.sample(x)).abs() < 1e-9, "{x}");
        }
    }
}

#[test]
fn a_flat_segment_stays_flat_whatever_its_ease() {
    let e = Envelope::eased(
        vec![[0., 0.4], [1., 0.4]],
        &[Ease::Bezier([0.1, 1., 0.9, 1.])],
    );
    e.validate().unwrap();
    assert_eq!(e.sample(0.5), 0.4);
}

#[test]
fn edits_keep_handles_valid_and_reject_invalid_changes_atomically() {
    let mut e = curved();
    let i = e.insert_point(0.5).unwrap();
    e.move_point(i, [0.7, 0.4]).unwrap();
    e.validate().unwrap();
    let before = e.clone();
    assert!(e.move_point(i, [f64::NAN, 0.]).is_err());
    assert!(e.move_point(i, [1.01, 0.]).is_err());
    assert!(
        e.move_point(i, [1., 0.]).is_err(),
        "x must stay below the next x"
    );
    assert!(e.set_ease(0, Ease::Bezier([1.2, 0., 0.5, 1.])).is_err());
    assert!(e.set_ease(0, Ease::Bezier([0.5, -0.1, 0.5, 1.])).is_err());
    assert!(
        e.set_ease(2, Ease::Hold).is_err(),
        "the last point has no ease"
    );
    assert_eq!(e, before);
    e.set_handles(0, [0.1, 0.35], [0.6, 0.]).unwrap();
    let Ease::Bezier(handles) = e.ease(0) else {
        panic!("{:?}", e.ease(0))
    };
    for (a, b) in handles.iter().zip([1. / 7., 0.875, 6. / 7., 0.]) {
        assert!((a - b).abs() < 1e-12, "{handles:?}");
    }
    e.remove_point(i).unwrap();
    assert_eq!(e.points.len(), 2);
    e.validate().unwrap();
    assert!(e.remove_point(0).is_err());
}

#[test]
fn curves_serialize_as_points_with_optional_eases() {
    let source = r#"{"points":[[0.0,1.0],[1.0,0.0]]}"#;
    let e: Envelope = serde_json::from_str(source).unwrap();
    assert_eq!(e.sample(0.25), 0.75);
    assert_eq!(serde_json::to_string(&e).unwrap(), source);
    // Named eases keep their names; a drawn ease is its four numbers.
    let source = r#"{"points":[[0.0,0.0,"ease-in"],[0.5,1.0,"hold"],[0.8,1.0,[0.2,0.0,0.4,1.0]],[1.0,0.0]]}"#;
    let e: Envelope = serde_json::from_str(source).unwrap();
    e.validate().unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), source);
    let e = curved();
    let mut bad = e.clone();
    bad.points[0].ease = Ease::Bezier([0., f64::INFINITY, 1., 1.]);
    assert!(bad.validate().is_err());
}

#[test]
fn old_and_malformed_curves_fail_with_the_shape_to_write() {
    let error = |source: &str| {
        let e: Result<Envelope, _> = serde_json::from_str(source);
        let error = match e {
            Ok(e) => e.validate().unwrap_err().to_string(),
            Err(error) => error.to_string(),
        };
        assert!(error.len() < 300, "{error}");
        error
    };
    for (source, needle) in [
        (
            r#"{"points":[[0,0],[1,1]],"curves":[{"kind":"linear"}]}"#,
            r#"[[0, 0, "ease-in"]"#,
        ),
        (
            r#"{"points":[[0,0],[1,1]],"segments":["linear"]}"#,
            r#"[[0, 0, "ease-in"]"#,
        ),
        (r#"{"points":[[0,0,"step"],[1,1]]}"#, "\"ease-in-out\""),
        (r#"{"points":[[0,0],[1,1,"hold"]]}"#, "[1, 0]"),
        (r#"{"points":[[0,0,"linear",1],[1,1]]}"#, "[x, value, ease]"),
        (r#"{"points":[[0.2,0],[1,1]]}"#, "x 0"),
        (
            r#"{"points":[[0,0],[0.5,1],[0.5,0],[1,1]]}"#,
            "above the previous x",
        ),
        (
            r#"{"points":[[0,0,[0,0,2,1]],[1,1]]}"#,
            "[0.42, 0, 0.58, 1]",
        ),
    ] {
        let message = error(source);
        assert!(message.contains(needle), "{source}: {message}");
    }
}

#[test]
fn held_segments_keep_one_value_and_survive_edits() {
    let mut e = Envelope::eased(vec![[0., 0.2], [0.5, 1.], [1., 1.]], &[Ease::Hold]);
    e.validate().unwrap();
    for (time, expected) in [(0., 0.2), (0.49, 0.2), (0.5, 1.), (0.9, 1.), (1., 1.)] {
        assert_eq!(e.sample(time), expected);
    }
    let index = e.insert_point(0.25).unwrap();
    assert_eq!(e.point(index), [0.25, 0.2]);
    assert_eq!([e.ease(0), e.ease(1)], [Ease::Hold, Ease::Hold]);
    assert_eq!(e.sample(0.4), 0.2);
    e.remove_point(index).unwrap();
    assert_eq!(e.ease(0), Ease::Hold);
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        serde_json::json!({"points": [[0.0, 0.2, "hold"], [0.5, 1.0], [1.0, 1.0]]})
    );
}
