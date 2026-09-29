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

fn source_program(
    name: &str,
    edits: impl FnOnce(&mut std::collections::BTreeMap<String, luma_patterns::Value>),
) -> luma_patterns::Result<luma_patterns::PreparedGraph> {
    use luma_patterns::*;
    let form = if name == "Position" {
        "aim@1"
    } else {
        "color@1"
    };
    let mut inputs = presets().preset(form, name).unwrap().inputs.clone();
    edits(&mut inputs);
    let cells: Vec<_> = (0..8)
        .map(|n| Cell {
            id: format!("f{}:{}", n / 4, n % 4),
            group: "all".into(),
            world: [n as f64, 0., 0.],
            uvz: [n as f64, 0., 0.],
        })
        .collect();
    PreparedGraph::new(
        &standard_library(),
        form,
        &inputs,
        Frame {
            cells: &cells,
            features: None,
            beat: 0.,
            clip_start: 0.,
            clip_duration: 16.,
            seed: 41,
        },
    )
}
fn source(value: serde_json::Value) -> luma_patterns::Value {
    serde_json::from_value(value).unwrap()
}

#[test]
fn overlapping_colors_follow_the_brightest_event_without_mixing_rgb_with_events() {
    use luma_patterns::*;
    let program=source_program("Wash",|inputs|{
        inputs.insert("brightness".into(),source(serde_json::json!({"type":"time","value":{"events":{"every":{"type":"beats","value":1},"life":{"type":"beats","value":3}},"points":[[0,0],[1,1]]}})));
        inputs.insert("color".into(),source(serde_json::json!({"type":"time","value":{"events":{"same_as":"brightness"},"points":[[0,[1,0,0]],[1,[0,0,1]]]}})));
    }).unwrap();
    let Value::Lighting(heads) = &program.evaluate(2.5).unwrap()["lighting"] else {
        panic!()
    };
    let age = 2.5 / 3.;
    for head in heads.values() {
        let rgb = head.color.unwrap();
        let dimmer = head.dimmer.unwrap();
        for (actual, want) in
            rgb.map(|v| v * dimmer)
                .into_iter()
                .zip([(1. - age) * age, 0., age * age])
        {
            assert!((actual - want).abs() < 1e-10);
        }
    }
    let batch = program.evaluate_batch(&[2.5, 0.2, 9.9]).unwrap();
    for (at, beat) in [2.5, 0.2, 9.9].into_iter().enumerate() {
        assert_eq!(
            batch["lighting"].sample(at).unwrap(),
            program.evaluate(beat).unwrap()["lighting"]
        );
    }
}

#[test]
fn event_references_reject_cycles_and_inputs_without_clocks() {
    for reference in ["brightness", "fade", "missing"] {
        let result = source_program("Wash", |inputs| {
            inputs.insert("brightness".into(),source(serde_json::json!({"type":"time","value":{"events":{"same_as":reference},"points":[[0,0],[1,1]]}})));
        });
        assert!(result.is_err(), "{reference}");
    }
}

#[test]
fn grain_groups_space_and_noise_values_by_fixture() {
    use luma_patterns::*;
    for value in [
        serde_json::json!({"type":"space","value":{"axis":{"source":{"kind":"u"},"per_group":false,"reverse":false},"curve":{"points":[[0,0],[1,1]]},"grain":"fixture"}}),
        serde_json::json!({"type":"noise","value":{"speed":{"type":"beats","value":4},"scale":{"type":"number","value":0.4},"range":[{"type":"number","value":0},{"type":"number","value":1}],"grain":"fixture"}}),
    ] {
        let program = source_program("Wash", |inputs| {
            inputs.insert("brightness".into(), source(value));
        })
        .unwrap();
        let Value::Lighting(heads) = &program.evaluate(1.3).unwrap()["lighting"] else {
            panic!()
        };
        for f in 0..2 {
            let first = heads[&format!("f{f}:0")].dimmer;
            for n in 1..4 {
                assert_eq!(first, heads[&format!("f{f}:{n}")].dimmer);
            }
        }
        assert_ne!(heads["f0:0"].dimmer, heads["f1:0"].dimmer);
    }
}

#[test]
fn numeric_clocks_accept_noise_and_seek_without_playback_history() {
    let program=source_program("Pulse",|inputs|{
        let luma_patterns::Value::Time(time)=inputs.get_mut("brightness").unwrap()else{panic!()};
        time.events=Some(luma_patterns::Events::repeating(source(serde_json::json!({"type":"noise","value":{"speed":{"type":"beats","value":2},"range":[{"type":"number","value":0.5},{"type":"number","value":2}]}})),None));
    }).unwrap();
    let beats = [0., 7.3, 1.1, 12.2];
    let batch = program.evaluate_batch(&beats).unwrap();
    for (i, beat) in beats.into_iter().enumerate() {
        assert_eq!(
            batch["lighting"].sample(i).unwrap(),
            program.evaluate(beat).unwrap()["lighting"]
        );
    }
}

#[test]
fn independent_noise_inputs_do_not_wander_together() {
    for independent in [false, true] {
        let program=source_program("Position",|inputs|{
        for key in ["horizontal","vertical"]{inputs.insert(key.into(),source(serde_json::json!({"type":"noise","value":{"independent":independent,"speed":{"type":"beats","value":2},"range":[{"type":"number","value":-20},{"type":"number","value":20}]}})));}
    }).unwrap();
        let out = program.evaluate_batch(&[0.5, 1.2, 2.9]).unwrap();
        let signal = out[luma_patterns::aim::TURN_OUTPUT].signal().unwrap();
        assert!(
            (0..3).any(|t| (signal.values()[[0, t, 3]] - signal.values()[[0, t, 4]]).abs() > 1e-3)
        );
    }
}

#[test]
fn equal_brightness_uses_the_newest_events_color() {
    let program = source_program("Wash", |inputs| {
        inputs.insert("brightness".into(),source(serde_json::json!({"type":"time","value":{"events":{"every":{"type":"beats","value":1},"life":{"type":"beats","value":3}},"points":[[0,1],[1,1]]}})));
        inputs.insert("color".into(),source(serde_json::json!({"type":"time","value":{"events":{"same_as":"brightness"},"points":[[0,[1,0,0]],[1,[0,0,1]]]}})));
    }).unwrap();
    let Value::Lighting(heads) = &program.evaluate(2.5).unwrap()["lighting"] else {
        panic!()
    };
    for head in heads.values() {
        let color = head.color.unwrap();
        assert!(color[0] > color[2], "newest event is still red: {color:?}");
    }
}

#[test]
fn random_coverage_can_follow_audio() {
    #[derive(Debug)]
    struct Energy;
    impl FeatureSource for Energy {
        fn sample(&self, _: &FeatureRequest, beat: f64) -> Result<f64> {
            Ok(beat)
        }
    }
    let program = source_program("Random heads", |inputs| {
        let Value::Random(random) = inputs.get_mut("brightness").unwrap() else {
            panic!()
        };
        random.coverage = Box::new(source(
            serde_json::json!({"type":"audio","value":{"from_hz":20,"to_hz":150,"floor":0}}),
        ));
    })
    .unwrap()
    .with_features(std::sync::Arc::new(Energy))
    .unwrap();
    let lit = |beat| {
        let result = program.evaluate(beat).unwrap();
        let Value::Lighting(heads) = &result["lighting"] else {
            panic!()
        };
        heads
            .values()
            .filter(|head| head.dimmer.unwrap_or(0.) > 0.)
            .count()
    };
    assert!(lit(12.) > lit(1.), "more energy selects more units");
}

#[test]
fn clip_fade_cannot_hide_a_repeating_source_in_its_amount() {
    assert!(source_program("Wash",|inputs|{
        inputs.insert("fade".into(),source(serde_json::json!({"type":"time","value":{"points":[[0,1],[1,1]],"gain":{"type":"time","value":{"events":{"every":{"type":"beats","value":1}},"points":[[0,0],[1,1]]}}}})));
    }).is_err());
}

#[test]
fn a_form_interface_cannot_omit_inputs_its_evaluator_needs() {
    let mut library = standard_library();
    library
        .definitions
        .get_mut("color@1")
        .unwrap()
        .inputs
        .remove("brightness");
    assert!(library.validate("color@1").is_err());
}

#[test]
fn cyclic_period_sources_are_rejected_without_a_venue() {
    let mut clip = presets().preset("color@1", "Pulse").unwrap().clip(0., 16.);
    let Value::Time(time) = clip.inputs.get_mut("brightness").unwrap() else {
        panic!()
    };
    time.events = Some(Events::repeating(
        source(
            serde_json::json!({"type":"time","value":{"events":{"same_as":"brightness"},"points":[[0,1],[1,2]]}}),
        ),
        None,
    ));
    assert!(Score::validate_clip(&standard_library(), "cycle", &clip)
        .unwrap_err()
        .to_string()
        .contains("cycle"));
}

#[test]
fn fixed_forms_fold_before_playback() {
    for name in ["Wash", "Position"] {
        let program = source_program(name, |_| {}).unwrap();
        assert_eq!(
            program.dynamic_step_count(),
            0,
            "a static form needs no frame work"
        );
        assert_eq!(
            program.evaluate(1.).unwrap(),
            program.evaluate(12.).unwrap()
        );
    }
}

#[test]
fn an_audio_driven_clock_is_integrated_once_not_resampled_each_frame() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[derive(Debug)]
    struct Energy(Arc<AtomicUsize>);
    impl FeatureSource for Energy {
        fn sample(&self, _: &FeatureRequest, beat: f64) -> Result<f64> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(beat)
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let program=source_program("Pulse",|inputs| {
        let Value::Time(time)=inputs.get_mut("brightness").unwrap() else {panic!()};
        time.curve=SourceCurve::Keys(Keyframes::numbers(&[[0.,0.],[1.,1.]],&[]));
        time.events=Some(Events::repeating(source(serde_json::json!({"type":"audio","value":{"from_hz":20,"to_hz":150,"floor":0.25,"gain":{"type":"number","value":4}}})),None));
    }).unwrap().with_features(Arc::new(Energy(calls.clone()))).unwrap();
    let prepared_calls = calls.load(Ordering::Relaxed);
    assert!(prepared_calls > 0);
    let frames = program.evaluate_batch(&[0.1, 0.2, 0.3]).unwrap();
    assert_ne!(
        frames["lighting"].sample(0).unwrap(),
        frames["lighting"].sample(2).unwrap()
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        prepared_calls,
        "playback must only read the prepared clock"
    );
}

#[test]
fn color_follows_the_winning_random_event_with_fixture_grain() {
    let program=source_program("Wash",|inputs|{
        inputs.insert("brightness".into(),source(serde_json::json!({"type":"random","value":{"grain":"fixture","events":{"every":{"type":"beats","value":1},"life":{"type":"beats","value":3}},"coverage":{"type":"proportion","value":1},"level":{"type":"time","value":{"points":[[0,0],[1,1]]}}}})));
        inputs.insert("color".into(),source(serde_json::json!({"type":"time","value":{"events":{"same_as":"brightness"},"points":[[0,[1,0,0]],[1,[0,0,1]]]}})));
    }).unwrap();
    let Value::Lighting(heads) = &program.evaluate(2.5).unwrap()["lighting"] else {
        panic!()
    };
    for head in heads.values() {
        let color = head.color.unwrap();
        assert!(
            color[2] > color[0],
            "the strongest event is the oldest, blue event"
        );
    }
}

#[test]
fn independent_color_and_brightness_clocks_keep_separate_event_axes() {
    let program=source_program("Wash",|inputs|{
        inputs.insert("brightness".into(),source(serde_json::json!({"type":"time","value":{"events":{"every":{"type":"beats","value":1},"life":{"type":"beats","value":4}},"points":[[0,0],[1,1]]}})));
        inputs.insert("color".into(),source(serde_json::json!({"type":"time","value":{"events":{"every":{"type":"beats","value":2},"life":{"type":"beats","value":3}},"points":[[0,[1,0,0]],[1,[0,0,1]]]}})));
    }).unwrap();
    let beats = [0.2, 2.5, 4.7];
    let batch = program.evaluate_batch(&beats).unwrap();
    for (t, beat) in beats.into_iter().enumerate() {
        assert_eq!(
            batch["lighting"].sample(t).unwrap(),
            program.evaluate(beat).unwrap()["lighting"]
        );
    }
}

#[test]
fn dynamic_sources_accept_an_empty_batch() {
    for name in ["Chase", "Shimmer", "Drift", "Pulse"] {
        source_program(name, |_| {})
            .unwrap()
            .evaluate_batch(&[])
            .unwrap();
    }
}
