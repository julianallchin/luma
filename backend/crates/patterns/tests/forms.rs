use luma_patterns::*;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Two fixtures of four heads on one line along stage X.
fn cells() -> Vec<Cell> {
    (0..8)
        .map(|n| Cell {
            id: format!("{}:{}", if n < 4 { "left" } else { "right" }, n % 4),
            group: "bar".into(),
            world: [f64::from(n), 0.0, 0.0],
            uvz: [f64::from(n), 0.0, 0.0],
        })
        .collect()
}
/// Head ids in stage order.
fn heads() -> Vec<String> {
    cells().into_iter().map(|cell| cell.id).collect()
}

const START: f64 = 8.0;

fn prepare(form: &str, inputs: &BTreeMap<String, Value>) -> Result<PreparedGraph> {
    let cells = cells();
    PreparedGraph::new(
        &standard_library(),
        form,
        inputs,
        Frame {
            cells: &cells,
            features: None,
            beat: START,
            clip_start: START,
            clip_duration: 16.0,
            seed: 7,
        },
    )
}

fn preset(name: &str) -> (String, BTreeMap<String, Value>) {
    let preset = presets().preset(name).unwrap_or_else(|| panic!("{name}"));
    (preset.form.clone(), preset.inputs.clone())
}

fn lighting(value: &Value) -> &BTreeMap<String, FixtureOutput> {
    let Value::Lighting(lighting) = value else {
        panic!("expected lighting, got {value:?}")
    };
    lighting
}

/// Brightness per head in stage order, at `beat` clip beats after the start.
fn render(form: &str, inputs: &BTreeMap<String, Value>, beat: f64) -> Vec<f64> {
    let program = prepare(form, inputs).unwrap();
    let result = program.evaluate(START + beat).unwrap();
    let lit = lighting(&result["lighting"]);
    heads()
        .iter()
        .map(|id| lit[id].dimmer.unwrap_or(0.0))
        .collect()
}
fn lit(values: &[f64]) -> Vec<bool> {
    values.iter().map(|v| *v > 1e-9).collect()
}
fn set(inputs: &mut BTreeMap<String, Value>, key: &str, value: Value) {
    assert!(inputs.insert(key.into(), value).is_some(), "{key}");
}
fn curve(points: &[[f64; 2]], segment: Segment) -> Keyframes {
    Keyframes::numbers(points, &vec![segment; points.len() - 1])
}

#[test]
fn every_preset_is_complete_valid_and_places_as_a_clip() {
    let library = standard_library();
    let shipped = presets();
    for form in FORMS {
        assert!(
            shipped.presets.iter().any(|preset| preset.form == form),
            "{form} has no preset"
        );
        assert!(library.definitions[form].playable(), "{form}");
    }
    for name in [
        "Wash",
        "Color fade",
        "Rainbow",
        "Gradient",
        "Chase",
        "Wave",
        "Ripple",
        "Spin",
        "Bounce",
        "Alternating sides",
        "Stepped chase",
        "Pulse",
        "Dissolve",
        "Build",
        "Random heads",
        "Shimmer",
        "Drift",
        "Atmosphere",
        "Aurora",
        "Strobe",
    ] {
        let preset = shipped.preset(name).unwrap_or_else(|| panic!("{name}"));
        preset.validate(&library).unwrap();
        let mut score = Score::default();
        score.clips.insert("clip".into(), preset.clip(START, 16.0));
        score
            .validate(&library)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let program = prepare(&preset.form, &preset.inputs).unwrap();
        let bright = (0..64)
            .map(|step| program.evaluate(START + f64::from(step) * 0.25).unwrap())
            .flat_map(|result| {
                lighting(&result["lighting"])
                    .values()
                    .map(|head| head.dimmer.unwrap_or(0.0))
                    .collect::<Vec<_>>()
            })
            .fold(0.0, f64::max);
        assert!(bright > 0.1, "{name} never lights");
    }
    for name in [
        "Ramp up",
        "Ramp down",
        "Swell",
        "Fade in",
        "Fade out",
        "Hold then drop",
        "Spike",
    ] {
        let curve = shipped.curve(name).unwrap_or_else(|| panic!("{name}"));
        curve.validate().unwrap();
        assert!(curve.values().all(|v| (0.0..=1.0).contains(&v)), "{name}");
    }
}

#[test]
fn input_order_names_every_input_of_each_form_once() {
    let library = standard_library();
    for form in FORMS {
        let order = input_order(form).expect(form);
        let mut sorted: Vec<&str> = order.to_vec();
        sorted.sort_unstable();
        let keys: Vec<&str> = library.definitions[form]
            .inputs
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(sorted, keys, "{form}");
    }
    assert!(input_order("chase").is_none());
}

#[test]
fn form_inputs_must_be_complete_known_and_promotable() {
    let (form, inputs) = preset("Chase");
    let mut missing = inputs.clone();
    missing.remove("width");
    let error = prepare(&form, &missing).unwrap_err();
    assert!(error.0.contains("missing input width"), "{error}");

    let mut unknown = inputs.clone();
    unknown.insert("delay".into(), Value::Beats(1.0));
    assert!(prepare(&form, &unknown)
        .unwrap_err()
        .0
        .contains("unknown input delay"));

    let mut score = Score::default();
    let mut clip = presets().preset("Chase").unwrap().clip(0.0, 4.0);
    clip.inputs = missing;
    score.clips.insert("clip".into(), clip);
    assert!(score.validate(&standard_library()).is_err());

    let ramp = curve(&[[0.0, 0.0], [1.0, 1.0]], Segment::Linear);
    for (key, value) in [
        ("axis", Value::Time(ramp.clone())),
        ("color", Value::Hit(ramp.clone())),
        (
            "color",
            Value::Noise(NoiseSource {
                speed: 4.0,
                range: [0.0, 1.0],
            }),
        ),
        ("shape", Value::Time(ramp.clone())),
        // A proportion curve must stay within 0..1.
        (
            "width",
            Value::Time(curve(&[[0.0, 0.0], [1.0, 2.0]], Segment::Linear)),
        ),
        // A speed curve must stay positive.
        (
            "every",
            Value::Time(curve(&[[0.0, 0.0], [1.0, 2.0]], Segment::Linear)),
        ),
    ] {
        let mut wrong = inputs.clone();
        set(&mut wrong, key, value);
        assert!(
            prepare(&form, &wrong).is_err(),
            "{key} accepted a bad source"
        );
    }
    let (sparkle, mut inputs) = preset("Random heads");
    set(&mut inputs, "alpha", Value::Hit(ramp));
    assert!(prepare(&sparkle, &inputs).is_err());

    // Promotable sources are data on the form's inputs.
    let chase = &standard_library().definitions["color.chase@1"];
    assert_eq!(
        chase.inputs["alpha"].promotable,
        [
            SourceKind::Time,
            SourceKind::Hit,
            SourceKind::Noise,
            SourceKind::Audio
        ]
    );
    assert_eq!(
        chase.inputs["every"].promotable,
        [SourceKind::Time, SourceKind::Events]
    );
    assert!(chase.inputs["axis"].promotable.is_empty());
}

#[test]
fn constant_color_follows_alpha() {
    let (form, mut inputs) = preset("Wash");
    assert_eq!(render(&form, &inputs, 1.0), vec![1.0; 8]);
    set(&mut inputs, "alpha", Value::Proportion(0.5));
    assert_eq!(render(&form, &inputs, 1.0), vec![0.5; 8]);
    set(
        &mut inputs,
        "alpha",
        Value::Time(curve(&[[0.0, 0.0], [1.0, 1.0]], Segment::Linear)),
    );
    for (beat, expected) in [(0.0, 0.0), (4.0, 0.25), (8.0, 0.5)] {
        for value in render(&form, &inputs, beat) {
            assert!((value - expected).abs() < 1e-12, "{beat}: {value}");
        }
    }
}

fn colors(form: &str, inputs: &BTreeMap<String, Value>, beat: f64) -> Vec<[f64; 3]> {
    let result = prepare(form, inputs)
        .unwrap()
        .evaluate(START + beat)
        .unwrap();
    let lit = lighting(&result["lighting"]);
    heads().iter().map(|id| lit[id].rgb()).collect()
}

#[test]
fn time_and_space_gradients() {
    let (form, inputs) = preset("Color fade");
    let first = colors(&form, &inputs, 0.0);
    assert!(first.iter().all(|c| *c == first[0]));
    assert!((first[0][0] - 1.0).abs() < 1e-9 && first[0][2] < 1e-9);
    assert_ne!(colors(&form, &inputs, 8.0)[0], first[0]);

    let (form, inputs) = preset("Rainbow");
    assert_eq!(colors(&form, &inputs, 1.0), colors(&form, &inputs, 5.0));
    assert_ne!(colors(&form, &inputs, 1.0), colors(&form, &inputs, 2.0));

    let (form, inputs) = preset("Gradient");
    let across = colors(&form, &inputs, 0.0);
    assert!((across[0][0] - 1.0).abs() < 1e-9);
    assert!((across[7][2] - 1.0).abs() < 1e-9);
    assert_eq!(across, colors(&form, &inputs, 3.0));
}

#[test]
fn chase_width_is_relative_to_the_gap_between_strokes() {
    let (form, inputs) = preset("Chase");
    // Every = travel, rel 0.2: width 0.2 / 0.8 = 0.25 of the axis. Halfway
    // through its life the stroke is centered on the axis.
    assert_eq!(
        lit(&render(&form, &inputs, 1.0)),
        [false, false, false, true, true, false, false, false]
    );
    // Every 1, travel 2: g = 0.5. Rel 0.4 is width 0.2 / 0.8 = 0.25.
    let mut relative = inputs.clone();
    set(&mut relative, "every", Value::Beats(1.0));
    set(&mut relative, "width", Value::Proportion(0.4));
    let mut absolute = relative.clone();
    set(&mut absolute, "width_relative", Value::Boolean(false));
    set(&mut absolute, "width", Value::Proportion(0.25));
    for step in 0..40 {
        let beat = f64::from(step) * 0.1 + 0.05;
        assert_eq!(
            lit(&render(&form, &relative, beat)),
            lit(&render(&form, &absolute, beat)),
            "{beat}"
        );
    }
    // Two strokes are on the axis at once when travel is twice every.
    let on = lit(&render(&form, &relative, 1.5));
    let runs = on.windows(2).filter(|pair| pair[1] && !pair[0]).count() + usize::from(on[0]);
    assert_eq!(runs, 2, "{on:?}");
    // Rel 1 at every = travel is capped at a stroke as wide as the axis.
    let mut wide = inputs.clone();
    set(&mut wide, "width", Value::Proportion(1.0));
    // Its open edges sit on the end heads at that moment.
    assert_eq!(
        lit(&render(&form, &wide, 1.0)),
        [false, true, true, true, true, true, true, false]
    );
}

#[test]
fn clipped_strokes_enter_and_leave_fully() {
    for name in ["Chase", "Wave", "Ripple"] {
        let (form, mut inputs) = preset(name);
        set(
            &mut inputs,
            "axis",
            presets().preset("Chase").unwrap().inputs["axis"].clone(),
        );
        // A rest between strokes: every 4, travel 2.
        let mut rest = inputs.clone();
        set(&mut rest, "every", Value::Beats(4.0));
        for beat in [0.0, 2.0, 4.0, 6.0] {
            assert_eq!(render(&form, &rest, beat), vec![0.0; 8], "{name} at {beat}");
        }
        assert!(render(&form, &rest, 1.0).iter().any(|v| *v > 0.0), "{name}");
    }
}

#[test]
fn back_to_back_strokes_are_never_cut_off() {
    for name in ["Wave", "Ripple"] {
        let (form, mut inputs) = preset(name);
        set(
            &mut inputs,
            "axis",
            presets().preset("Chase").unwrap().inputs["axis"].clone(),
        );
        let program = prepare(&form, &inputs).unwrap();
        let beats: Vec<f64> = (0..=800)
            .map(|step| START + f64::from(step) * 0.01)
            .collect();
        let batch = program.evaluate_batch(&beats).unwrap();
        let frames: Vec<Vec<f64>> = (0..beats.len())
            .map(|t| {
                let value = batch["lighting"].sample(t).unwrap();
                let lit = lighting(&value);
                heads()
                    .iter()
                    .map(|id| lit[id].dimmer.unwrap_or(0.0))
                    .collect()
            })
            .collect();
        // Every head changes smoothly from one sample to the next, also
        // across the start of each new stroke.
        for (t, pair) in frames.windows(2).enumerate() {
            for (head, (a, b)) in pair[0].iter().zip(&pair[1]).enumerate() {
                assert!(
                    (a - b).abs() < 0.1,
                    "{name}: head {head} jumps {a} -> {b} at {}",
                    beats[t]
                );
            }
        }
    }
}

#[test]
fn alternating_sides_lights_the_left_half_then_the_right_half() {
    let (form, inputs) = preset("Alternating sides");
    let left = [true, true, true, true, false, false, false, false];
    let right = left.map(|on| !on);
    for (beat, expected) in [
        (0.1, left),
        (0.9, left),
        (1.1, right),
        (1.9, right),
        (2.5, left),
    ] {
        let values = render(&form, &inputs, beat);
        assert_eq!(lit(&values), expected, "{beat}");
        assert!(values.iter().all(|v| *v == 0.0 || *v == 1.0), "{values:?}");
    }
}

#[test]
fn chase_paths_and_direction_following_shapes() {
    let (form, mut inputs) = preset("Chase");
    let head = |inputs: &BTreeMap<String, Value>, beat| {
        let values = render(&form, inputs, beat);
        (0..8)
            .max_by(|a, b| values[*a].total_cmp(&values[*b]))
            .unwrap()
    };
    let comet = shape_presets()
        .into_iter()
        .find(|(name, _)| *name == "Comet")
        .unwrap()
        .1;
    set(&mut inputs, "shape", comet);
    set(&mut inputs, "width", Value::Proportion(1.0));
    // Forward: the bright head leads toward the end of the axis.
    let forward = render(&form, &inputs, 1.0);
    assert!(forward[5] > forward[3], "{forward:?}");
    let backward = path_presets()
        .into_iter()
        .find(|(name, _)| *name == "Backward")
        .unwrap()
        .1;
    set(&mut inputs, "path", backward);
    // Backward: the comet's head still leads, now toward the start.
    let reversed = render(&form, &inputs, 1.0);
    assert!(reversed[2] > reversed[4], "{reversed:?}");
    assert!(head(&inputs, 0.5) > head(&inputs, 1.5));
}

#[test]
fn stamped_events_count_from_the_clip_start() {
    let (form, mut inputs) = preset("Chase");
    set(
        &mut inputs,
        "every",
        Value::Events(Events::Beats {
            times: EventTimes::new(vec![0.0, 3.0]).unwrap(),
        }),
    );
    let first = render(&form, &inputs, 1.0);
    assert!(first.iter().any(|v| *v > 0.0));
    assert_eq!(render(&form, &inputs, 2.5), vec![0.0; 8]);
    assert_eq!(render(&form, &inputs, 4.0), first);
    assert_eq!(render(&form, &inputs, 6.0), vec![0.0; 8]);
}

#[test]
fn time_curves_on_speed_inputs_are_seek_safe() {
    let (form, mut inputs) = preset("Chase");
    set(
        &mut inputs,
        "every",
        Value::Time(curve(&[[0.0, 2.0], [1.0, 0.5]], Segment::Linear)),
    );
    set(
        &mut inputs,
        "travel",
        Value::Time(curve(&[[0.0, 2.0], [0.5, 1.0], [1.0, 3.0]], Segment::Ease)),
    );
    let program = prepare(&form, &inputs).unwrap();
    let beats: Vec<f64> = (0..160).map(|step| START + f64::from(step) * 0.1).collect();
    let batch = program.evaluate_batch(&beats).unwrap();
    let mut strokes = Vec::new();
    for (t, beat) in beats.iter().enumerate().rev() {
        let sought = program.evaluate(*beat).unwrap();
        assert_eq!(
            batch["lighting"].sample(t).unwrap(),
            sought["lighting"],
            "{beat}"
        );
        strokes.push(
            lighting(&sought["lighting"])
                .values()
                .filter(|head| head.dimmer.unwrap_or(0.0) > 0.0)
                .count(),
        );
    }
    assert!(strokes.iter().any(|count| *count > 0));

    // Faster `every` makes more events: count strokes started per half.
    let events = |from: f64, to: f64| {
        let mut sparkle = preset("Random heads").1;
        set(
            &mut sparkle,
            "every",
            Value::Time(curve(&[[0.0, 2.0], [1.0, 0.5]], Segment::Linear)),
        );
        set(&mut sparkle, "duration", Value::Beats(0.05));
        let program = prepare("color.sparkle@1", &sparkle).unwrap();
        let mut count = 0;
        let mut was_lit = false;
        let mut beat = from;
        while beat < to {
            let result = program.evaluate(START + beat).unwrap();
            let is_lit = lighting(&result["lighting"])
                .values()
                .any(|head| head.dimmer.unwrap_or(0.0) > 0.0);
            if is_lit && !was_lit {
                count += 1;
            }
            was_lit = is_lit;
            beat += 0.01;
        }
        count
    };
    // Beats between events fall from 2 to 0.5: the sum of 1 / every is
    // 5.01 at the middle of the clip and 14.79 at its end.
    assert_eq!(events(0.0, 8.0), 6);
    assert_eq!(events(0.0, 16.0), 15);
}

#[test]
fn sparkle_coverage_counts_heads_and_grains() {
    let (form, mut inputs) = preset("Random heads");
    for (coverage, expected) in [(0.5, 4), (0.25, 2), (1.0, 8), (0.0, 0)] {
        set(&mut inputs, "coverage", Value::Proportion(coverage));
        let on = lit(&render(&form, &inputs, 0.5));
        assert_eq!(on.iter().filter(|on| **on).count(), expected, "{coverage}");
    }
    // A new random set per event, and the same set within one event.
    set(&mut inputs, "coverage", Value::Proportion(0.5));
    let sets: Vec<_> = (0..8)
        .map(|event| lit(&render(&form, &inputs, f64::from(event) + 0.5)))
        .collect();
    assert_eq!(lit(&render(&form, &inputs, 0.9)), sets[0]);
    assert!(sets.iter().any(|set| *set != sets[0]));

    set(&mut inputs, "grain", Value::Number(0.0));
    for event in 0..6 {
        let on = lit(&render(&form, &inputs, f64::from(event) + 0.5));
        assert!(on[..4].iter().all(|v| *v == on[0]), "{on:?}");
        assert!(on[4..].iter().all(|v| *v == on[4]), "{on:?}");
        assert_ne!(on[0], on[4]);
    }
    set(&mut inputs, "grain", Value::Number(2.0));
    let on = lit(&render(&form, &inputs, 0.5));
    assert_eq!(on.iter().filter(|on| **on).count(), 4);
    for pair in on.chunks(2) {
        assert_eq!(pair[0], pair[1], "{on:?}");
    }
}

#[test]
fn sparkle_hit_curves_follow_each_event_and_overlaps_keep_the_maximum() {
    let (form, inputs) = preset("Pulse");
    assert_eq!(render(&form, &inputs, 0.25), vec![1.0; 8]);
    for value in render(&form, &inputs, 0.75) {
        assert!((value - 0.5).abs() < 1e-12, "{value}");
    }
    let (form, inputs) = preset("Dissolve");
    let count = |beat| {
        lit(&render(&form, &inputs, beat))
            .iter()
            .filter(|v| **v)
            .count()
    };
    assert_eq!(count(0.0), 8);
    assert!(count(1.0) > count(3.0));

    let (form, mut inputs) = preset("Pulse");
    set(&mut inputs, "duration", Value::Beats(2.0));
    // Two events overlap: the new one at full brightness wins.
    assert_eq!(render(&form, &inputs, 1.25), vec![1.0; 8]);
}

#[test]
fn noise_and_strobe_forms() {
    let (form, inputs) = preset("Aurora");
    let frames: Vec<_> = (0..8)
        .map(|beat| render(&form, &inputs, f64::from(beat)))
        .collect();
    assert!(frames.iter().flatten().all(|v| (0.0..=1.0).contains(v)));
    assert!(frames[0].iter().any(|v| *v != frames[0][0]));
    assert!(frames.iter().any(|frame| *frame != frames[0]));

    let (form, inputs) = preset("Strobe");
    let result = prepare(&form, &inputs).unwrap().evaluate(START).unwrap();
    for head in lighting(&result["lighting"]).values() {
        assert_eq!(head.strobe, Some(0.9));
        assert_eq!(head.dimmer, Some(1.0));
    }
}

#[test]
fn noise_and_audio_sources_stay_in_their_range() {
    let (form, mut inputs) = preset("Wash");
    set(
        &mut inputs,
        "alpha",
        Value::Noise(NoiseSource {
            speed: 2.0,
            range: [0.25, 0.75],
        }),
    );
    let values: Vec<f64> = (0..32)
        .map(|step| render(&form, &inputs, f64::from(step) * 0.25)[0])
        .collect();
    assert!(
        values.iter().all(|v| (0.25..=0.75).contains(v)),
        "{values:?}"
    );
    assert!(values.iter().any(|v| (v - values[0]).abs() > 1e-3));

    #[derive(Debug)]
    struct Mix;
    impl FeatureSource for Mix {
        fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
            let FeatureRequest::Band { source, low_hz, .. } = request else {
                return Err(Error("only band energy".into()));
            };
            assert_eq!(source.source(), AudioSource::Mix);
            assert_eq!(*low_hz, 20.0);
            Ok(FeatureSample::Energy(0.5 + 0.4 * beat.sin()))
        }
        fn onsets(&self, _: Drum) -> Result<EventTimes> {
            Err(Error("no drums".into()))
        }
    }
    set(
        &mut inputs,
        "alpha",
        Value::Audio(AudioLevel {
            band: Band::Low,
            range: [0.2, 0.6],
        }),
    );
    let program = prepare(&form, &inputs).unwrap();
    assert_eq!(program.feature_requests().len(), 1);
    let program = program.with_features(Arc::new(Mix)).unwrap();
    let values: Vec<f64> = (0..64)
        .map(|step| {
            let result = program.evaluate(START + f64::from(step) * 0.25).unwrap();
            lighting(&result["lighting"])["left:0"].dimmer.unwrap()
        })
        .collect();
    assert!(
        values.iter().all(|v| (0.2 - 1e-9..=0.6 + 1e-9).contains(v)),
        "{values:?}"
    );
    assert!(values.iter().any(|v| (*v - 0.6).abs() < 1e-3));
}

#[test]
fn sources_are_tagged_values_in_stored_clips() {
    let value: Value = serde_json::from_value(serde_json::json!({
        "type": "time",
        "value": {"points": [[0, 2], [1, 0.5]], "segments": ["linear"]}
    }))
    .unwrap();
    assert_eq!(
        value,
        Value::Time(curve(&[[0.0, 2.0], [1.0, 0.5]], Segment::Linear))
    );
    for json in [
        serde_json::json!({"type": "hit", "value": {"points": [[0, 1], [0.5, 1], [1, 0]], "segments": ["hold", "linear"]}}),
        serde_json::json!({"type": "time", "value": {"points": [[0, [1, 0, 0]], [1, [0, 0, 1]]], "segments": ["ease"]}}),
        serde_json::json!({"type": "noise", "value": {"speed": 4.0, "range": [0.2, 1.0]}}),
        serde_json::json!({"type": "audio", "value": {"band": "low", "range": [0.0, 1.0]}}),
        serde_json::json!({"type": "events", "value": {"source": "beats", "times": [0.0, 1.5, 3.0]}}),
    ] {
        let value: Value = serde_json::from_value(json.clone()).unwrap();
        value.validate().unwrap();
        assert!(value.source_kind().is_some());
        assert_eq!(serde_json::to_value(&value).unwrap()["type"], json["type"]);
    }
    // A color time curve drives a color input.
    let (form, mut inputs) = preset("Wash");
    set(
        &mut inputs,
        "color",
        serde_json::from_value(serde_json::json!({
            "type": "time",
            "value": {"points": [[0, [1, 0, 0]], [1, [0, 0, 1]]], "segments": ["linear"]}
        }))
        .unwrap(),
    );
    assert_eq!(colors(&form, &inputs, 0.0)[0], [1.0, 0.0, 0.0]);
    assert_eq!(colors(&form, &inputs, 8.0)[0], [0.5, 0.0, 0.5]);
}

#[test]
fn radial_and_angle_axes_need_no_solved_circle() {
    let ring: Vec<Cell> = (0..8)
        .map(|n| {
            let angle = f64::from(n) / 8.0 * std::f64::consts::TAU;
            let radius = if n % 2 == 0 { 1.0 } else { 2.0 };
            Cell {
                id: format!("ring:{n}"),
                group: "ring".into(),
                world: [0.0; 3],
                uvz: [radius * angle.cos(), radius * angle.sin(), 0.0],
            }
        })
        .collect();
    let spec = |source| MappingSpec {
        source,
        per_group: false,
        reverse: false,
        mirror: None,
    };
    let angle = spec(MappingSource::Angle).resolve(&ring).unwrap();
    for (n, coordinate) in angle.coordinates.iter().enumerate() {
        assert!((coordinate.position - n as f64 / 8.0).abs() < 1e-9);
        assert!(coordinate.closed);
    }
    let radial = spec(MappingSource::Radial).resolve(&ring).unwrap();
    for (n, coordinate) in radial.coordinates.iter().enumerate() {
        assert!((coordinate.position - if n % 2 == 0 { 0.0 } else { 1.0 }).abs() < 1e-9);
    }
}

#[test]
fn hit_width_grows_each_stroke_over_its_life() {
    let (form, mut inputs) = preset("Chase");
    set(&mut inputs, "width_relative", Value::Boolean(false));
    set(
        &mut inputs,
        "width",
        Value::Hit(curve(&[[0.0, 0.1], [1.0, 0.6]], Segment::Linear)),
    );
    // Hold the stroke in the middle of the axis, so only the width moves.
    set(
        &mut inputs,
        "path",
        Value::Envelope(Envelope::linear(vec![[0.0, 0.5], [1.0, 0.5]])),
    );
    let count = |beat| {
        lit(&render(&form, &inputs, beat))
            .iter()
            .filter(|v| **v)
            .count()
    };
    // Width 0.15 early in the life reaches the two middle heads (±0.071);
    // width 0.55 late in the life reaches four (±0.214).
    assert_eq!(count(0.2), 2);
    assert_eq!(count(1.8), 4);
    assert_eq!(count(2.2), 2);
}

#[test]
fn form_clips_stay_forms_when_copied_between_scores() {
    let library = standard_library();
    let mut source = Score::default();
    source.clips.insert(
        "chase".into(),
        presets().preset("Chase").unwrap().clip(0.0, 8.0),
    );
    let mut target = Score::default();
    target
        .import_clip(&library, &source, "chase", "copy")
        .unwrap();
    assert_eq!(target.clips["copy"].graph, "color.chase@1");
    assert!(target.definitions.is_empty());
    assert!(target.make_independent(&library, "copy", "local").is_err());
}
