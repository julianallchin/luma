//! `color@1` and the space source.
use luma_patterns::*;
use std::collections::BTreeMap;

/// Two rows of heads in group `a` and a column in group `b`.
fn cells() -> Vec<Cell> {
    let rows: [(&str, [[f64; 3]; 4]); 3] = [
        (
            "a",
            [[0., 0., 3.], [1., 0., 3.], [2., 0., 3.], [3., 0., 3.]],
        ),
        (
            "a",
            [
                [0.5, 2., 3.],
                [1.8, 2., 3.2],
                [3.1, 2., 3.4],
                [4.4, 2., 3.6],
            ],
        ),
        (
            "b",
            [[5., -0.5, 2.], [5., 0.4, 2.], [5., 1.3, 2.], [5., 2.2, 2.]],
        ),
    ];
    let mut cells = Vec::new();
    for (fixture, (group, heads)) in rows.into_iter().enumerate() {
        for (head, uvz) in heads.into_iter().enumerate() {
            cells.push(Cell {
                id: format!("f{fixture}:{head}"),
                group: group.into(),
                world: uvz,
                uvz,
            });
        }
    }
    cells
}

const START: f64 = 8.0;

fn clip_json(graph: &str, inputs: &BTreeMap<String, Value>) -> String {
    serde_json::json!({"clips": {"a": {
        "graph": graph, "start": START, "duration": 16.0, "seed": 7, "inputs": inputs
    }}})
    .to_string()
}

#[test]
fn an_old_color_form_id_is_refused() {
    let error = Score::from_json(&standard_library(), &clip_json("color.chase@1", &wash()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("color.chase@1 is not a form"), "{error}");
}

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
        "gradient",
    );
    expect(
        "brightness",
        serde_json::json!({"axis": axis(), "gradient": bw()}),
        "input",
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
    // Stroke controls validate at the source boundary.
    for (field, value) in [
        ("width", serde_json::json!({"type":"number","value":5})),
        ("boundary", serde_json::json!("natural")),
    ] {
        let mut source = serde_json::json!({"axis":axis(),"curve":ramp(),"offset":{"type":"time","value":{"events":{"every":{"type":"beats","value":2}},"points":[[0,0],[1,1]]}}});
        source[field] = value;
        expect("brightness", source, field);
    }
    let parsed = serde_json::from_value::<Value>(
        serde_json::json!({"type":"space","value":{"axis":axis(),"curve":ramp(),"move":{}}}),
    );
    assert!(parsed.is_err());
}

#[test]
fn color_can_follow_overlapping_strokes() {
    let mut inputs = chase();
    let Value::Space(space) = inputs.get_mut("brightness").unwrap() else {
        panic!()
    };
    let Value::Time(offset) = space.offset.as_deref_mut().unwrap() else {
        panic!()
    };
    offset.events = Some(Events::repeating(Value::Beats(2.), Some(Value::Beats(6.))));
    inputs.insert("color".into(),serde_json::from_value(serde_json::json!({"type":"time","value":{"events":{"same_as":"brightness"},"points":[[0,[1,0,0]],[1,[0,0,1]]]}})).unwrap());
    check(&inputs).unwrap();
    assert!(brightness(&inputs, 5.).iter().any(|v| *v > 0.));
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
    assert!(error(&inputs).contains("input"));
    inputs.insert(
        "every".into(),
        serde_json::from_value(
            serde_json::json!({"type": "time", "value": {"gradient": bw(), "curve": ramp()}}),
        )
        .unwrap(),
    );
    assert!(check(&inputs).is_err());
}

/// Brightness per head at `beat` beats into a 16-beat clip over
/// [`cells`].
fn brightness(inputs: &BTreeMap<String, Value>, beat: f64) -> Vec<f64> {
    let cells = cells();
    let program = PreparedGraph::new(
        &standard_library(),
        "color@1",
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
    .unwrap();
    let result = program.evaluate(START + beat).unwrap();
    let Value::Lighting(lit) = &result["lighting"] else {
        panic!()
    };
    cells
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
    let level = brightness(&inputs, 3.0);
    // Along U the heads further right are brighter, and the ends reach the
    // ends of the curve.
    let mut by_u: Vec<(f64, f64)> = cells()
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
fn an_inherited_curve_fades_each_stroke() {
    let mut inputs = chase();
    let Value::Space(space) = inputs.get_mut("brightness").unwrap() else {
        panic!()
    };
    space.gain = Box::new(
        serde_json::from_value(serde_json::json!({"type":"time","value":{"points":[[0,1],[1,0]]}}))
            .unwrap(),
    );
    let early: f64 = brightness(&inputs, 4.2).into_iter().fold(0., f64::max);
    let late: f64 = brightness(&inputs, 5.8).into_iter().fold(0., f64::max);
    assert!(early > late && late > 0., "{early} {late}");
}
