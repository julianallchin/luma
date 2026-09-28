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

/// The light preset called `name`: these tests cover the color and strobe
/// forms, whose preset names are unique among them. Aim has its own, in
/// `aim.rs`.
fn light(name: &str) -> &'static luma_patterns::FormPreset {
    presets()
        .presets
        .iter()
        .find(|preset| preset.name == name && preset.form != "aim@1")
        .unwrap_or_else(|| panic!("{name}"))
}

fn preset(name: &str) -> (String, BTreeMap<String, Value>) {
    let preset = light(name);
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
/// Set an input. A chase's `axis`, `shape`, `path`, `travel`, `width`,
/// `width_relative` and `boundary` are parts of its moving brightness, and
/// a color fade's `colors` and `curve` are parts of its color per hit.
fn set(inputs: &mut BTreeMap<String, Value>, key: &str, value: Value) {
    if let Some(old) = inputs.get_mut(key) {
        *old = value;
        return;
    }
    if let Some(Value::Space(space)) = inputs.get_mut("brightness") {
        let movement = space.movement.as_mut().expect("a moving brightness");
        match (key, value) {
            ("axis", Value::Mapping(axis)) => space.axis = axis,
            ("shape", Value::Envelope(shape)) => space.curve = Some(shape),
            ("path", Value::Envelope(path)) => movement.path = path,
            ("travel", travel) => movement.travel = travel,
            ("width", width) => movement.width = width,
            ("width_relative", Value::Boolean(relative)) => movement.width_relative = relative,
            ("boundary", Value::Boundary(boundary)) => movement.boundary = boundary,
            (key, value) => panic!("{key}: {value:?}"),
        }
        return;
    }
    let Some(Value::Hit(SourceCurve::Gradient(read))) = inputs.get_mut("color") else {
        panic!("{key}")
    };
    match (key, value) {
        ("colors", Value::Gradient(gradient)) => read.gradient = gradient,
        ("curve", Value::Envelope(curve)) => read.curve = curve,
        (key, value) => panic!("{key}: {value:?}"),
    }
}
/// The axis of a clip's space source.
fn axis_of(inputs: &BTreeMap<String, Value>) -> Value {
    inputs
        .values()
        .find_map(|value| match value {
            Value::Space(space) => Some(Value::Mapping(space.axis.clone())),
            _ => None,
        })
        .expect("a space source")
}
fn curve(points: &[[f64; 2]], ease: Ease) -> SourceCurve {
    Keyframes::numbers(points, &vec![ease; points.len() - 1]).into()
}

#[test]
fn every_preset_is_complete_valid_and_places_as_a_clip() {
    let library = standard_library();
    let shipped = presets();
    // A name is unique within its form; two forms may share one.
    let mut seen = std::collections::BTreeSet::new();
    for preset in &shipped.presets {
        assert!(
            seen.insert((preset.form.as_str(), preset.name.as_str())),
            "{} {} twice",
            preset.form,
            preset.name
        );
    }
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
        "Grow",
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
        let preset = light(name);
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
                    .map(|head| head.dimmer.or(head.strobe).unwrap_or(0.0))
                    .collect::<Vec<_>>()
            })
            .fold(0.0, f64::max);
        assert!(bright > 0.1, "{name} never lights or strobes");
    }
    for preset in &shipped.curves {
        let (name, curve) = (&preset.name, &preset.curve);
        curve.validate().unwrap();
        assert!(curve.values().all(|v| (0.0..=1.0).contains(&v)), "{name}");
    }
    for preset in &shipped.gradients {
        preset.gradient.validate().unwrap();
        assert!(preset.gradient.stops.len() >= 2, "{}", preset.name);
    }
    let names = |input| {
        shipped
            .curves_for(input)
            .map(|curve| curve.name.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names("alpha"),
        [
            "Full",
            "Fade in",
            "Fade out",
            "Fade in-out",
            "Swell",
            "Breathe",
            "Cut in",
            "Cut out"
        ]
    );
    assert_eq!(
        names("travel"),
        [
            "Ramp up",
            "Ramp down",
            "Swell",
            "Fade in",
            "Fade out",
            "Hold then drop",
            "Spike"
        ]
    );
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
    missing.remove("every");
    let error = prepare(&form, &missing).unwrap_err();
    assert!(error.0.contains("missing input every"), "{error}");

    let mut unknown = inputs.clone();
    unknown.insert("delay".into(), Value::Beats(1.0));
    assert!(prepare(&form, &unknown)
        .unwrap_err()
        .0
        .contains("unknown input delay"));

    let mut several_missing = inputs.clone();
    several_missing.remove("every");
    several_missing.remove("alpha");
    let error = prepare(&form, &several_missing).unwrap_err();
    assert!(
        error.0.contains("alpha") && error.0.contains("every"),
        "one error should name every missing input, not just the first: {error}"
    );

    let mut score = Score::default();
    let mut clip = presets().preset("color@1", "Chase").unwrap().clip(0.0, 4.0);
    clip.inputs = missing;
    score.clips.insert("clip".into(), clip);
    assert!(score.validate(&standard_library()).is_err());

    let ramp = curve(&[[0.0, 0.0], [1.0, 1.0]], Ease::Linear);
    for (key, value) in [
        ("color", Value::Hit(ramp.clone())),
        (
            "color",
            Value::Noise(NoiseSource {
                speed: 4.0,
                range: [0.0, 1.0],
            }),
        ),
        // A curve must stay within the input's range: width 0 to 4.
        (
            "width",
            Value::Time(curve(&[[0.0, 0.0], [1.0, 5.0]], Ease::Linear)),
        ),
        ("travel", Value::Number(2.0)),
        (
            "alpha",
            Value::Time(curve(&[[0.0, 0.0], [1.0, 2.0]], Ease::Linear)),
        ),
        // A speed curve must stay positive.
        (
            "every",
            Value::Time(curve(&[[0.0, 0.0], [1.0, 2.0]], Ease::Linear)),
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
    let color = &standard_library().definitions["color@1"];
    assert_eq!(
        color.inputs["alpha"].promotable,
        [
            SourceKind::Time,
            SourceKind::Hit,
            SourceKind::Noise,
            SourceKind::Audio
        ]
    );
    assert_eq!(
        color.inputs["color"].promotable,
        [SourceKind::Time, SourceKind::Hit, SourceKind::Space]
    );
    assert_eq!(color.inputs["every"].promotable, [SourceKind::Time]);
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
        Value::Time(curve(&[[0.0, 0.0], [1.0, 1.0]], Ease::Linear)),
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
    // Rel 1 at every = travel is capped at a stroke four axes wide: rel
    // 4/5 of the gap, as wide as the largest absolute width.
    let mut wide = inputs.clone();
    set(&mut wide, "width", Value::Number(1.0));
    let mut four = wide.clone();
    set(&mut four, "width_relative", Value::Boolean(false));
    set(&mut four, "width", Value::Number(MAX_WIDTH));
    for beat in [0.3, 1.0, 1.7] {
        assert_eq!(
            render(&form, &wide, beat),
            render(&form, &four, beat),
            "{beat}"
        );
    }
    assert_eq!(lit(&render(&form, &wide, 1.0)), [true; 8]);
}

#[test]
fn a_stroke_wider_than_the_axis_keeps_the_rig_partly_lit() {
    // A soft stroke 1.36 axes wide, one stroke every 2 beats, with its
    // center on the axis from start to end (the old graphs' centers).
    let (form, mut inputs) = preset("Wave");
    let width = 1.36;
    set(&mut inputs, "width", Value::Number(width));
    let (from, to) = (
        width / 2.0 / (1.0 + width),
        (1.0 + width / 2.0) / (1.0 + width),
    );
    set(
        &mut inputs,
        "path",
        Value::Envelope(Envelope::linear(vec![[0.0, from], [1.0, to]])),
    );
    // Halfway through its life it covers the whole rig, brightest in the
    // middle.
    let middle = render(&form, &inputs, 1.0);
    assert!(middle.iter().all(|v| *v > 0.0), "{middle:?}");
    assert!(middle[0] < middle[3] && middle[7] < middle[4], "{middle:?}");
    // Through the rest of its life part of the rig is lit and part is dim.
    for step in 1..20 {
        let beat = f64::from(step) * 0.1;
        let frame = render(&form, &inputs, beat);
        let (low, high) = frame
            .iter()
            .fold((f64::MAX, 0.0_f64), |(l, h), v| (l.min(*v), h.max(*v)));
        assert!(high > 0.2, "{beat}: {frame:?}");
        assert!(high - low > 0.05, "{beat}: {frame:?}");
    }
    // Wider than the largest width, it is refused.
    set(&mut inputs, "width", Value::Number(MAX_WIDTH + 0.5));
    assert!(prepare(&form, &inputs).is_err());
}

#[test]
fn clipped_strokes_enter_and_leave_fully() {
    for name in ["Chase", "Wave", "Ripple"] {
        let (form, mut inputs) = preset(name);
        set(&mut inputs, "axis", axis_of(&preset("Chase").1));
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
        set(&mut inputs, "axis", axis_of(&preset("Chase").1));
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
    set(&mut inputs, "width", Value::Number(1.0));
    set(&mut inputs, "width_relative", Value::Boolean(false));
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
fn time_curves_on_speed_inputs_are_seek_safe() {
    let (form, mut inputs) = preset("Chase");
    set(
        &mut inputs,
        "every",
        Value::Time(curve(&[[0.0, 2.0], [1.0, 0.5]], Ease::Linear)),
    );
    set(
        &mut inputs,
        "travel",
        Value::Time(
            Keyframes::numbers(
                &[[0.0, 2.0], [0.5, 1.0], [1.0, 3.0]],
                &[Ease::EaseInOut, Ease::Bezier([0.2, 0.0, 0.8, 0.75])],
            )
            .into(),
        ),
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
            Value::Time(curve(&[[0.0, 2.0], [1.0, 0.5]], Ease::Linear)),
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
    for (coverage, expected) in [(0.5, 4), (0.25, 2), (0.75, 6), (0.0, 0)] {
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
    let (form, inputs) = preset("Dissolve");
    let count = |beat| {
        lit(&render(&form, &inputs, beat))
            .iter()
            .filter(|v| **v)
            .count()
    };
    assert_eq!(count(0.0), 8);
    assert!(count(1.0) > count(3.0));

    let (form, mut inputs) = preset("Random heads");
    set(
        &mut inputs,
        "brightness",
        Value::Hit(curve(&[[0.0, 1.0], [1.0, 0.0]], Ease::Linear)),
    );
    set(&mut inputs, "duration", Value::Beats(2.0));
    // Two events overlap: event 0 is at 0.375 and event 1 at 0.875. A head
    // in both sets keeps the maximum.
    let values = render(&form, &inputs, 1.25);
    assert!(
        values.iter().any(|v| (v - 0.875).abs() < 1e-12),
        "{values:?}"
    );
    for value in values {
        assert!(
            [0.0, 0.375, 0.875]
                .iter()
                .any(|expected| (value - expected).abs() < 1e-12),
            "{value}"
        );
    }
}

#[test]
fn a_sparkle_never_lights_every_head() {
    let (form, mut inputs) = preset("Random heads");
    set(&mut inputs, "coverage", Value::Proportion(1.0));
    let error = prepare(&form, &inputs).unwrap_err();
    assert!(error.0.contains("use a Wash"), "{error}");
    // A curve may pass through 100%.
    set(
        &mut inputs,
        "coverage",
        Value::Hit(curve(&[[0.0, 1.0], [1.0, 0.0]], Ease::Linear)),
    );
    prepare(&form, &inputs).unwrap();
}

#[test]
fn pulse_is_a_wash_with_a_brightness_per_hit() {
    let (form, mut inputs) = preset("Pulse");
    assert_eq!(form, "color@1");
    assert_eq!(render(&form, &inputs, 0.25), vec![1.0; 8]);
    for (beat, expected) in [(0.75, 0.5), (1.25, 1.0), (1.75, 0.5), (15.9, 0.2)] {
        for value in render(&form, &inputs, beat) {
            assert!((value - expected).abs() < 1e-9, "{beat}: {value}");
        }
    }
    // Brightness and alpha multiply.
    set(&mut inputs, "alpha", Value::Proportion(0.5));
    for value in render(&form, &inputs, 0.75) {
        assert!((value - 0.25).abs() < 1e-12, "{value}");
    }
    // Every 0 is one hit over the whole clip of 16 beats.
    set(&mut inputs, "alpha", Value::Proportion(1.0));
    set(&mut inputs, "every", Value::Beats(0.0));
    for (beat, expected) in [(0.75, 1.0), (7.9, 1.0), (12.0, 0.5)] {
        for value in render(&form, &inputs, beat) {
            assert!((value - expected).abs() < 1e-9, "{beat}: {value}");
        }
    }
    // A time curve on every counts hits like an odometer: the first hit
    // lasts 1 beat, then they grow longer.
    set(
        &mut inputs,
        "every",
        Value::Time(curve(&[[0.0, 1.0], [1.0, 3.0]], Ease::Linear)),
    );
    prepare(&form, &inputs).unwrap();

    // A fixed brightness darkens the color; every then has no effect.
    let (form, mut inputs) = preset("Wash");
    set(&mut inputs, "brightness", Value::Proportion(0.25));
    set(&mut inputs, "every", Value::Beats(1.0));
    assert_eq!(render(&form, &inputs, 0.5), vec![0.25; 8]);
    let wash = &standard_library().definitions["color@1"];
    assert_eq!(
        wash.inputs["brightness"].promotable,
        [
            SourceKind::Time,
            SourceKind::Hit,
            SourceKind::Noise,
            SourceKind::Audio,
            SourceKind::Space
        ]
    );
    assert_eq!(wash.inputs["every"].promotable, [SourceKind::Time]);
}

#[test]
fn grow_lights_each_bar_from_its_middle_to_both_ends_and_holds() {
    // Two straight bars of five heads along stage X, 10 units apart.
    let cells: Vec<Cell> = (0..10)
        .map(|n| Cell {
            id: format!("{}:{}", if n < 5 { "left" } else { "right" }, n % 5),
            group: "bar".into(),
            world: [f64::from(n % 5) + if n < 5 { 0.0 } else { 10.0 }, 0.0, 0.0],
            uvz: [f64::from(n % 5) + if n < 5 { 0.0 } else { 10.0 }, 0.0, 0.0],
        })
        .collect();
    let (form, inputs) = preset("Grow");
    let program = PreparedGraph::new(
        &standard_library(),
        &form,
        &inputs,
        Frame {
            cells: &cells,
            features: None,
            beat: START,
            clip_start: START,
            clip_duration: 16.0,
            seed: 7,
        },
    )
    .unwrap();
    let frame = |beat: f64| -> Vec<f64> {
        let result = program.evaluate(START + beat).unwrap();
        let lit = lighting(&result["lighting"]);
        cells
            .iter()
            .map(|cell| lit[&cell.id].dimmer.unwrap_or(0.0))
            .collect()
    };
    // The first beat each head lights, sampled every 1/16 beat.
    let mut first = [f64::INFINITY; 10];
    let mut was = [false; 10];
    for step in 0..256 {
        let beat = f64::from(step) / 16.0;
        for (head, value) in frame(beat).into_iter().enumerate() {
            let on = value > 0.5;
            // Once lit, a head stays lit.
            assert!(!was[head] || on, "head {head} went dark at {beat}");
            if on && !was[head] {
                first[head] = beat;
            }
            was[head] = on;
        }
    }
    for bar in [&first[..5], &first[5..]] {
        // The middle head first, then its neighbours, then both ends.
        assert!(bar[2] < bar[1] && bar[1] < bar[0], "{first:?}");
        assert_eq!(bar[1], bar[3], "{first:?}");
        assert_eq!(bar[0], bar[4], "{first:?}");
    }
    assert_eq!(first[..5], first[5..]);
    assert!(first[2] < 1.0, "{first:?}");
    // Width 1.25 reaches both ends 8/9 of the way through the clip.
    assert!((first[0] - 128.0 / 9.0).abs() <= 1.0 / 16.0, "{first:?}");
    // Every head is lit at the end of the clip.
    assert_eq!(frame(15.99), vec![1.0; 10]);
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

    // The strobe writes only the shutter, so it strobes the color under it.
    let (form, mut inputs) = preset("Strobe");
    let result = prepare(&form, &inputs).unwrap().evaluate(START).unwrap();
    for head in lighting(&result["lighting"]).values() {
        assert_eq!(head.strobe, Some(0.9));
        assert_eq!(head.dimmer, None);
        assert_eq!(head.color, None);
    }
    // Alpha scales the rate; at 0 the strobe stops.
    set(&mut inputs, "alpha", Value::Proportion(0.0));
    let result = prepare(&form, &inputs).unwrap().evaluate(START).unwrap();
    for head in lighting(&result["lighting"]).values() {
        assert_eq!(head.strobe, Some(0.0));
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
        fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<f64> {
            // A custom range reaches the analysis as it was set.
            assert_eq!((request.low_hz, request.high_hz), (55.0, 130.0));
            Ok(0.5 + 0.4 * beat.sin())
        }
    }
    set(
        &mut inputs,
        "alpha",
        Value::Audio(AudioLevel {
            from_hz: 55.0,
            to_hz: 130.0,
            floor: 0.3,
            threshold: 0.0,
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
    // The quietest moment of the clip gives the floor, the loudest gives 1,
    // and between them value = floor + (1 - floor) × energy.
    assert!(
        values.iter().all(|v| (0.3 - 1e-9..=1.0 + 1e-9).contains(v)),
        "{values:?}"
    );
    // Samples every quarter beat come near both ends.
    assert!(values.iter().any(|v| (*v - 1.0).abs() < 1e-2));
    assert!(values.iter().any(|v| (*v - 0.3).abs() < 1e-2));

    // With a threshold, energy below it gives 0, under the floor too;
    // energy at or above it gives floor + (1 - floor) × energy.
    set(
        &mut inputs,
        "alpha",
        Value::Audio(AudioLevel {
            from_hz: 55.0,
            to_hz: 130.0,
            floor: 0.3,
            threshold: 0.5,
        }),
    );
    let gated = prepare(&form, &inputs)
        .unwrap()
        .with_features(Arc::new(Mix))
        .unwrap();
    let mut off = 0;
    for (step, open) in values.iter().enumerate() {
        let result = gated.evaluate(START + step as f64 * 0.25).unwrap();
        let v = lighting(&result["lighting"])["left:0"].dimmer.unwrap();
        // The open level v = 0.3 + 0.7 e, so e ≥ 0.5 means v ≥ 0.65.
        if *open >= 0.65 + 1e-9 {
            assert!((v - open).abs() < 1e-9, "{step}: {v} vs {open}");
        } else if *open < 0.65 - 1e-9 {
            assert_eq!(v, 0.0, "{step}: {open}");
            off += 1;
        }
    }
    assert!(off > 0 && off < values.len(), "{off}");

    // A bad range, floor or threshold is rejected.
    for bad in [
        AudioLevel {
            from_hz: 130.0,
            to_hz: 55.0,
            floor: 0.3,
            threshold: 0.0,
        },
        AudioLevel {
            from_hz: 10.0,
            to_hz: 55.0,
            floor: 0.3,
            threshold: 0.0,
        },
        AudioLevel {
            from_hz: 55.0,
            to_hz: 130.0,
            floor: 1.5,
            threshold: 0.0,
        },
        AudioLevel {
            from_hz: 55.0,
            to_hz: 130.0,
            floor: 0.3,
            threshold: 1.5,
        },
    ] {
        set(&mut inputs, "alpha", Value::Audio(bad));
        assert!(prepare(&form, &inputs).is_err());
    }
    let named: Vec<_> = presets()
        .frequencies
        .iter()
        .map(|f| (f.name.as_str(), f.from_hz, f.to_hz))
        .collect();
    assert_eq!(named[0], ("Kick", 40.0, 100.0));
    assert_eq!(named.len(), 5);
}

#[test]
fn sources_are_tagged_values_in_stored_clips() {
    let value: Value = serde_json::from_value(serde_json::json!({
        "type": "time",
        "value": {"points": [[0, 2], [1, 0.5]]}
    }))
    .unwrap();
    assert_eq!(
        value,
        Value::Time(curve(&[[0.0, 2.0], [1.0, 0.5]], Ease::Linear))
    );
    for json in [
        serde_json::json!({"type": "hit", "value": {"points": [[0, 1, "hold"], [0.5, 1], [1, 0]]}}),
        serde_json::json!({"type": "time", "value": {"points": [[0, [1, 0, 0], "ease-in"], [1, [0, 0, 1]]]}}),
        serde_json::json!({"type": "noise", "value": {"speed": 4.0, "range": [0.2, 1.0]}}),
        serde_json::json!({"type": "audio", "value": {"from_hz": 40.0, "to_hz": 100.0, "floor": 0.3}}),
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
            "value": {"points": [[0, [1, 0, 0]], [1, [0, 0, 1]]]}
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
        span: Default::default(),
        plane: Some(AxisPlane::UpDown),
        source,
        per_group: false,
        reverse: false,
        mirror: None,
    };
    let angle = spec(MappingSource::Angle).resolve(&ring, 0).unwrap();
    for (n, coordinate) in angle.coordinates.iter().enumerate() {
        assert!((coordinate.position - n as f64 / 8.0).abs() < 1e-9);
        assert!(coordinate.closed);
    }
    let radial = spec(MappingSource::Radial).resolve(&ring, 0).unwrap();
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
        Value::Hit(curve(&[[0.0, 0.1], [1.0, 0.6]], Ease::Linear)),
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
fn a_score_holds_only_form_clips_with_finite_timing() {
    let library = standard_library();
    let mut score = Score::default();
    score.clips.insert(
        "chase".into(),
        presets()
            .preset("color@1", "Chase")
            .unwrap()
            .clip(START, 4.0),
    );
    score.validate(&library).unwrap();

    let prepared = score
        .prepare_clip(&library, "chase", &BTreeMap::new(), &cells())
        .unwrap();
    assert!(prepared.evaluate(START - 0.01).unwrap().is_empty());
    assert!(prepared.evaluate(START + 4.0).unwrap().is_empty());
    assert!(prepared.evaluate(f64::NAN).is_err());

    let clip = score.clips.get_mut("chase").unwrap();
    clip.duration = f64::MAX;
    clip.start = f64::MAX;
    assert!(score.validate(&library).is_err());

    let clip = score.clips.get_mut("chase").unwrap();
    clip.start = START;
    clip.duration = 4.0;
    clip.graph = "output".into();
    assert!(score
        .validate(&library)
        .unwrap_err()
        .0
        .contains("is not a form"));
}

#[test]
fn a_changed_clip_with_an_envelope_out_of_range_is_refused() {
    let library = standard_library();
    let mut stored = Score::default();
    stored.clips.insert(
        "chase".into(),
        presets()
            .preset("color@1", "Chase")
            .unwrap()
            .clip(START, 4.0),
    );
    let mut candidate = stored.clone();
    set(
        &mut candidate.clips.get_mut("chase").unwrap().inputs,
        "shape",
        Value::Envelope(Envelope::linear(vec![[0.0, 1.1], [1.0, 0.0]])),
    );
    let error = candidate.validate_changes(&library, &stored).unwrap_err().0;
    assert!(error.contains("clip chase"), "{error}");
    assert!(error.contains("must be in 0..1, not 1.1"), "{error}");
}

#[test]
fn stepped_color_curves_show_each_palette_stop_without_blending() {
    let (form, mut inputs) = preset("Color fade");
    let palette = [
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0],
        [1.0, 1.0, 1.0],
    ];
    set(
        &mut inputs,
        "colors",
        Value::Gradient(Gradient {
            stops: palette
                .iter()
                .enumerate()
                .map(|(i, color)| ColorStop {
                    t: i as f64 / 3.0,
                    color: *color,
                    alpha: 1.0,
                })
                .collect(),
        }),
    );
    let options = progress_presets();
    let steps = options
        .iter()
        .find(|(label, _)| *label == "Steps (4)")
        .unwrap();
    assert_eq!(steps.1, Value::Envelope(palette_steps(4)));
    set(&mut inputs, "curve", steps.1.clone());
    set(&mut inputs, "every", Value::Beats(8.0));
    for (beat, expected) in [1.0, 3.0, 5.0, 7.0].into_iter().zip(palette) {
        for color in colors(&form, &inputs, beat) {
            assert_eq!(color, expected, "{beat}");
        }
    }
    for label in ["Steps (2)", "Steps (3)", "Steps (6)", "Steps (8)"] {
        assert!(options.iter().any(|(name, _)| *name == label), "{label}");
    }
}

#[test]
fn bezier_sources_play_exactly_what_the_envelope_draws() {
    // Handles off the thirds: not a standard ease.
    let handles = [0.2, 0.9, 0.7, 0.05];
    let drawn = Envelope::eased(
        vec![[0.0, 0.2], [0.5, 0.6], [1.0, 1.0]],
        &[Ease::Bezier(handles)],
    );
    let stored: Value = serde_json::from_value(serde_json::json!({
        "type": "time",
        "value": {"points": [[0.0, 0.2, handles], [0.5, 0.6], [1.0, 1.0]]}
    }))
    .unwrap();
    let Value::Time(SourceCurve::Keys(curve)) = &stored else {
        unreachable!()
    };
    curve.validate().unwrap();
    for i in 0..=200 {
        let x = f64::from(i) / 200.0;
        assert_eq!(curve.sample(x)[0], drawn.sample(x), "{x}");
    }
    // Played as a clip's alpha, it is the same curve.
    let (form, mut inputs) = preset("Wash");
    set(&mut inputs, "alpha", stored.clone());
    for beat in [1.0, 3.0, 5.5, 12.0] {
        let expected = drawn.sample(beat / 16.0);
        for value in render(&form, &inputs, beat) {
            assert!((value - expected).abs() < 1e-12, "{beat}");
        }
    }
    // Handles out of 0..1 are refused.
    let mut bad = curve.clone();
    bad.points[0].ease = Ease::Bezier([1.4, 0.5, 0.2, 0.5]);
    assert!(bad.validate().is_err());
    let mut high = curve.clone();
    high.points[0].ease = Ease::Bezier([0.1, 1.5, 0.2, 0.5]);
    set(&mut inputs, "alpha", Value::Time(high.into()));
    assert!(prepare(&form, &inputs).is_err());
}

#[test]
fn a_fixture_span_chases_every_bar_at_once() {
    let (form, mut inputs) = preset("Chase");
    let Value::Mapping(mut axis) = axis_of(&inputs) else {
        panic!("axis")
    };
    axis.span = Span::Fixture;
    set(&mut inputs, "axis", Value::Mapping(axis));
    let mut lit = 0;
    for step in 0..20 {
        let beat = f64::from(step) * 0.1 + 0.05;
        let values = render(&form, &inputs, beat);
        // Heads 0–3 are the left bar, 4–7 the right bar.
        assert_eq!(values[..4], values[4..], "{beat}");
        lit += values.iter().filter(|v| **v > 0.0).count();
    }
    assert!(lit > 0);
}

#[test]
fn round_axes_need_a_plane_and_axes_have_no_per_group() {
    let (form, inputs) = preset("Ripple");
    let Value::Mapping(axis) = axis_of(&inputs) else {
        panic!("axis")
    };
    assert_eq!(axis.plane, Some(AxisPlane::Auto));
    let mut flat = axis.clone();
    flat.plane = None;
    let mut grouped = axis.clone();
    grouped.per_group = true;
    for wrong in [flat, grouped] {
        let mut bad = inputs.clone();
        set(&mut bad, "axis", Value::Mapping(wrong));
        assert!(prepare(&form, &bad).is_err());
    }
}

/// A random axis chases every head once per stroke, in a shuffled order
/// that stays the same from stroke to stroke.
#[test]
fn a_random_axis_chases_each_head_once_in_a_stable_order() {
    let (form, mut inputs) = preset("Chase");
    set(
        &mut inputs,
        "axis",
        axis_presets()
            .into_iter()
            .find(|(name, _)| *name == "Random")
            .unwrap()
            .1,
    );
    set(&mut inputs, "width", Value::Number(0.05));
    set(&mut inputs, "width_relative", Value::Boolean(false));
    // The beat at which each head is brightest, for two strokes.
    let every = match inputs["every"] {
        Value::Beats(every) => every,
        _ => panic!("every"),
    };
    let peaks = |from: f64| {
        let mut best = vec![(0.0, f64::NEG_INFINITY); 8];
        for step in 0..400 {
            let beat = from + every * f64::from(step) / 400.0;
            for (head, value) in render(&form, &inputs, beat).into_iter().enumerate() {
                if value > best[head].1 {
                    best[head] = (beat - from, value);
                }
            }
        }
        best
    };
    let (one, two) = (peaks(0.0), peaks(every));
    let order = |peaks: &[(f64, f64)]| {
        let mut heads: Vec<usize> = (0..8).collect();
        heads.sort_by(|a, b| peaks[*a].0.total_cmp(&peaks[*b].0));
        heads
    };
    assert!(one.iter().all(|(_, value)| *value > 0.5));
    assert_eq!(order(&one), order(&two));
    assert_ne!(order(&one), (0..8).collect::<Vec<_>>());
}
