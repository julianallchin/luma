//! `color@1` and the space source. The four old color forms were merged into
//! `color@1`; `fixtures/old_color_forms.json` holds their light, recorded
//! before they were deleted, for presets and varied inputs. It was recorded
//! when colors were linear sRGB: its inputs are converted as stored rows are
//! (`rec2020_upgrade`), and the light is compared back in linear sRGB. The
//! cases marked `rerecorded` read a gradient between stops: gradients now
//! blend in OKLab of the light itself, where they blended the stored numbers
//! as if gamma-encoded, so their light was recorded again
//! (`examples/rec2020_presets.rs`). Every other case keeps its old light.
use luma_patterns::*;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Recording {
    cells: Vec<Cell>,
    start: f64,
    duration: f64,
    seed: u64,
    times: Vec<f64>,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    label: String,
    form: String,
    inputs: BTreeMap<String, Value>,
    /// Per time, per cell: the light as linear sRGB.
    rgb: Vec<Vec<[f64; 3]>>,
    /// Recorded again when gradients began to blend the light itself.
    #[serde(default)]
    rerecorded: bool,
}

fn recording() -> Recording {
    let mut recording: Recording =
        serde_json::from_str(include_str!("fixtures/old_color_forms.json")).unwrap();
    for case in &mut recording.cases {
        let stored = serde_json::to_value(&case.inputs).unwrap();
        let converted = rec2020_upgrade::convert_clip_inputs(&case.form, &stored).unwrap();
        case.inputs = serde_json::from_value(converted).unwrap();
    }
    recording
}

fn program(recording: &Recording, form: &str, inputs: &BTreeMap<String, Value>) -> PreparedGraph {
    PreparedGraph::new(
        &standard_library(),
        form,
        inputs,
        Frame {
            cells: &recording.cells,
            features: None,
            beat: recording.start,
            clip_start: recording.start,
            clip_duration: recording.duration,
            seed: recording.seed,
        },
    )
    .unwrap()
}

/// The light of a clip, per time and cell, as linear sRGB.
fn light(
    recording: &Recording,
    form: &str,
    inputs: &BTreeMap<String, Value>,
) -> Vec<Vec<[f64; 3]>> {
    let program = program(recording, form, inputs);
    recording
        .times
        .iter()
        .map(|t| {
            let result = program.evaluate(recording.start + t).unwrap();
            let Value::Lighting(lit) = &result["lighting"] else {
                panic!("expected lighting")
            };
            recording
                .cells
                .iter()
                .map(|cell| color_space::Gamut::SRGB.fit(lit[&cell.id].rgb()))
                .collect()
        })
        .collect()
}

/// Float noise through the change of primaries and back.
const TOLERANCE: f64 = 1e-7;

fn same_light(label: &str, got: &[Vec<[f64; 3]>], want: &[Vec<[f64; 3]>]) {
    for (t, (got, want)) in got.iter().zip(want).enumerate() {
        for (cell, (got, want)) in got.iter().zip(want).enumerate() {
            for ch in 0..3 {
                assert!(
                    (got[ch] - want[ch]).abs() < TOLERANCE,
                    "{label}: time {t} cell {cell}: {got:?} != {want:?}"
                );
            }
        }
    }
}

#[test]
fn every_old_color_clip_converts_to_color_with_the_same_light() {
    let recording = recording();
    let mut forms = std::collections::BTreeSet::new();
    for case in &recording.cases {
        assert!(!is_form(&case.form), "{} is still a form", case.form);
        forms.insert(case.form.as_str());
        let (form, inputs) = upgrade(&case.form, &case.inputs)
            .unwrap_or_else(|| panic!("{}: no conversion", case.label));
        assert_eq!(form, "color@1");
        same_light(&case.label, &light(&recording, form, &inputs), &case.rgb);
    }
    assert_eq!(
        forms.into_iter().collect::<Vec<_>>(),
        [
            "color.chase@1",
            "color.constant@1",
            "color.space@1",
            "color.time@1"
        ]
    );
}

#[test]
fn only_gradient_reads_were_recorded_again() {
    let recording = recording();
    for case in recording.cases.iter().filter(|case| case.rerecorded) {
        let reads_a_gradient = case.inputs.values().any(|value| match value {
            Value::Gradient(_) | Value::Hit(SourceCurve::Gradient(_)) => true,
            Value::Time(SourceCurve::Gradient(_)) => true,
            Value::Space(space) => space.gradient.is_some(),
            _ => false,
        });
        assert!(reads_a_gradient, "{}", case.label);
    }
    let kept = recording
        .cases
        .iter()
        .filter(|case| !case.rerecorded)
        .count();
    assert!(kept * 4 >= recording.cases.len() * 3, "{kept}");
}

#[test]
fn the_recording_is_not_all_dark() {
    // The equivalence above means little over dark frames.
    let recording = recording();
    let lit = recording
        .cases
        .iter()
        .filter(|case| case.rgb.iter().flatten().flatten().any(|v| *v > 0.01))
        .count();
    assert!(lit * 10 >= recording.cases.len() * 9, "{lit}");
}

#[test]
fn every_old_preset_name_is_a_color_preset_with_the_same_light() {
    let recording = recording();
    for case in &recording.cases {
        let Some(name) = case.label.strip_prefix("preset ") else {
            continue;
        };
        let preset = presets()
            .preset("color@1", name)
            .unwrap_or_else(|| panic!("no color@1 preset {name}"));
        same_light(
            name,
            &light(&recording, &preset.form, &preset.inputs),
            &case.rgb,
        );
    }
}

fn clip_json(graph: &str, inputs: &BTreeMap<String, Value>) -> String {
    serde_json::json!({"clips": {"a": {
        "graph": graph, "start": 8.0, "duration": 16.0, "seed": 7, "inputs": inputs
    }}})
    .to_string()
}

#[test]
fn an_old_clip_loads_as_color_and_saves_and_loads_the_same() {
    let recording = recording();
    let library = standard_library();
    for case in recording.cases.iter().step_by(5) {
        let loaded = Score::from_json(&library, &clip_json(&case.form, &case.inputs))
            .unwrap_or_else(|e| panic!("{}: {e}", case.label));
        let clip = &loaded.clips["a"];
        assert_eq!(clip.graph, "color@1", "{}", case.label);
        let saved = loaded.to_json(&library).unwrap();
        assert!(!saved.contains("color.constant@1") && !saved.contains("color.chase@1"));
        let again = Score::from_json(&library, &saved).unwrap();
        assert_eq!(again, loaded, "{}", case.label);
        same_light(
            &case.label,
            &light(
                &recording,
                &again.clips["a"].graph,
                &again.clips["a"].inputs,
            ),
            &case.rgb,
        );
    }
}

#[test]
fn a_clip_row_read_back_upgrades_too() {
    let recording = recording();
    let case = recording
        .cases
        .iter()
        .find(|case| case.form == "color.time@1")
        .unwrap();
    let clip = Clip {
        graph: case.form.clone(),
        start: 0.0,
        duration: 4.0,
        seed: 0,
        selection_seed: None,
        selection: Selection::all(),
        z_index: 0,
        blend_mode: BlendMode::Replace,
        inputs: case.inputs.clone(),
    }
    .upgraded();
    assert_eq!(clip.graph, "color@1");
    assert!(matches!(
        clip.inputs["color"],
        Value::Hit(SourceCurve::Gradient(_))
    ));
    // A current clip passes through unchanged.
    assert_eq!(clip.clone().upgraded(), clip);
}

// ---------------------------------------------------------------------------
// The space source

fn wash() -> BTreeMap<String, Value> {
    presets().preset("color@1", "Wash").unwrap().inputs.clone()
}
fn chase() -> BTreeMap<String, Value> {
    presets().preset("color@1", "Chase").unwrap().inputs.clone()
}
fn space(value: serde_json::Value) -> Value {
    serde_json::from_value(serde_json::json!({"type": "space", "value": value})).unwrap()
}
fn check(inputs: &BTreeMap<String, Value>) -> Result<()> {
    Score::from_json(&standard_library(), &clip_json("color@1", inputs)).map(|_| ())
}
fn error(inputs: &BTreeMap<String, Value>) -> String {
    check(inputs).unwrap_err().to_string()
}
const U: &str = r#"{"source": {"kind": "u"}, "per_group": false, "reverse": false}"#;
fn axis() -> serde_json::Value {
    serde_json::from_str(U).unwrap()
}
fn bw() -> serde_json::Value {
    serde_json::json!({"stops": [{"t": 0, "color": [0, 0, 0]}, {"t": 1, "color": [1, 1, 1]}]})
}
fn ramp() -> serde_json::Value {
    serde_json::json!({"points": [[0, 0], [1, 1]]})
}

#[test]
fn a_space_source_is_checked() {
    let expect = |key: &str, value: serde_json::Value, words: &str| {
        let mut inputs = wash();
        inputs.insert(key.into(), space(value));
        let error = error(&inputs);
        assert!(error.contains(words), "{key}: {error}");
    };
    // A color reads a gradient; a number reads a curve.
    expect(
        "color",
        serde_json::json!({"axis": axis(), "curve": ramp()}),
        "does not fit",
    );
    expect(
        "brightness",
        serde_json::json!({"axis": axis(), "gradient": bw()}),
        "does not fit",
    );
    expect(
        "brightness",
        serde_json::json!({"axis": axis(), "gradient": bw(), "curve": ramp()}),
        "not both",
    );
    expect(
        "brightness",
        serde_json::json!({"axis": axis()}),
        "gradient",
    );
    // The axis keeps the rules of an axis input.
    let mut reversed = axis();
    reversed["reverse"] = true.into();
    expect(
        "brightness",
        serde_json::json!({"axis": reversed, "curve": ramp()}),
        "reverse",
    );
    let radial =
        serde_json::json!({"source": {"kind": "radial"}, "per_group": false, "reverse": false});
    expect(
        "brightness",
        serde_json::json!({"axis": radial, "curve": ramp()}),
        "plane",
    );
    // A curve of brightness stays in 0..1.
    expect(
        "brightness",
        serde_json::json!({"axis": axis(), "curve": {"points": [[0, 0], [1, 2]]}}),
        "0..1",
    );
    // Only a number moves: a color stroke has no light outside it.
    let movement = serde_json::json!({
        "path": ramp(), "travel": {"type": "beats", "value": 2}, "width": {"type": "number", "value": 0.2},
        "width_relative": true, "boundary": "clip"
    });
    expect(
        "color",
        serde_json::json!({"axis": axis(), "gradient": bw(), "move": movement}),
        "does not fit",
    );
    // A stroke's fields have the ranges of the chase they replace.
    for (field, value, words) in [
        (
            "width",
            serde_json::json!({"type": "number", "value": 5}),
            "move.width",
        ),
        (
            "travel",
            serde_json::json!({"type": "beats", "value": -1}),
            "move.travel",
        ),
        (
            "travel",
            serde_json::json!({"type": "number", "value": 2}),
            "move.travel",
        ),
        ("boundary", serde_json::json!("natural"), "move.boundary"),
    ] {
        let mut movement = movement.clone();
        movement[field] = value;
        expect(
            "brightness",
            serde_json::json!({"axis": axis(), "curve": ramp(), "move": movement}),
            words,
        );
    }
    let mut unknown = movement.clone();
    unknown["speed"] = 1.into();
    let parsed = serde_json::from_value::<Value>(serde_json::json!({
        "type": "space", "value": {"axis": axis(), "curve": ramp(), "move": unknown}
    }));
    assert!(parsed.is_err());
}

#[test]
fn a_color_hit_cannot_follow_strokes() {
    let mut inputs = chase();
    inputs.insert(
        "color".into(),
        serde_json::from_value(
            serde_json::json!({"type": "hit", "value": {"gradient": bw(), "curve": ramp()}}),
        )
        .unwrap(),
    );
    assert!(error(&inputs).contains("cannot follow the strokes"));
    // Over time is fine.
    inputs.insert(
        "color".into(),
        serde_json::from_value(
            serde_json::json!({"type": "time", "value": {"gradient": bw(), "curve": ramp()}}),
        )
        .unwrap(),
    );
    check(&inputs).unwrap();
}

#[test]
fn a_gradient_curve_needs_a_color_input() {
    let mut inputs = wash();
    inputs.insert(
        "brightness".into(),
        serde_json::from_value(
            serde_json::json!({"type": "time", "value": {"gradient": bw(), "curve": ramp()}}),
        )
        .unwrap(),
    );
    assert!(error(&inputs).contains("does not fit"));
    inputs.insert(
        "every".into(),
        serde_json::from_value(
            serde_json::json!({"type": "time", "value": {"gradient": bw(), "curve": ramp()}}),
        )
        .unwrap(),
    );
    assert!(check(&inputs).is_err());
}

/// Brightness per head at `beat` beats into a 16-beat clip over the
/// recording's cells.
fn brightness(inputs: &BTreeMap<String, Value>, beat: f64) -> Vec<f64> {
    let recording = recording();
    let program = program(&recording, "color@1", inputs);
    let result = program.evaluate(recording.start + beat).unwrap();
    let Value::Lighting(lit) = &result["lighting"] else {
        panic!()
    };
    recording
        .cells
        .iter()
        .map(|cell| lit[&cell.id].dimmer.unwrap_or(0.0))
        .collect()
}

#[test]
fn a_still_brightness_across_space_follows_its_curve_along_the_axis() {
    let mut inputs = wash();
    inputs.insert(
        "brightness".into(),
        space(serde_json::json!({"axis": axis(), "curve": ramp()})),
    );
    let recording = recording();
    let level = brightness(&inputs, 3.0);
    // Along U the heads further right are brighter, and the ends reach the
    // ends of the curve.
    let mut by_u: Vec<(f64, f64)> = recording
        .cells
        .iter()
        .zip(&level)
        .map(|(cell, level)| (cell.uvz[0], *level))
        .collect();
    by_u.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(
        by_u.windows(2).all(|w| w[0].0 == w[1].0 || w[0].1 < w[1].1),
        "{by_u:?}"
    );
    assert!(by_u[0].1 < 1e-9 && (by_u.last().unwrap().1 - 1.0).abs() < 1e-9);
    // It does not change over time.
    assert_eq!(level, brightness(&inputs, 11.0));
}

#[test]
fn a_hit_on_alpha_follows_each_stroke() {
    let mut inputs = chase();
    // A stroke fades out over its life.
    inputs.insert(
        "alpha".into(),
        serde_json::from_value(
            serde_json::json!({"type": "hit", "value": {"points": [[0, 1], [1, 0]]}}),
        )
        .unwrap(),
    );
    let early: f64 = brightness(&inputs, 4.2).into_iter().fold(0.0, f64::max);
    let late: f64 = brightness(&inputs, 5.8).into_iter().fold(0.0, f64::max);
    assert!(early > late && late > 0.0, "{early} {late}");
}
