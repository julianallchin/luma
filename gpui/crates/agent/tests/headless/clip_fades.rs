//! A form clip's alpha on the timeline: the fade handles, the bend handle and
//! the level line write the clip's stored `alpha`, undo takes the edit back,
//! and a clip moved over the end of another crosses the two with fades.

use super::support::{self, Fixture};
use gpui_agent::Mode;
use serde_json::{json, Value};
use std::time::Duration;

fn stored(name: &str) -> Value {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(support::stored_score_json(&support::config_dir(name)))
}

fn alpha(name: &str, clip: &str) -> Value {
    stored(name)["clips"][clip]["inputs"]["alpha"].clone()
}

/// The points of a stored alpha curve, as `(x, y)` pairs.
fn points(alpha: &Value) -> Vec<(f64, f64)> {
    assert_eq!(alpha["type"], "time", "{alpha}");
    alpha["value"]["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
        .collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.02
}

const HELPERS: &str = r#"
    const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
    const settle=()=>app.frames(20,{waitMs:60});
"#;

/// Open the timeline, then `body`.
fn opened(body: &str) -> String {
    support::script(&format!(
        r#"nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        {{ {HELPERS}
        until("waveform",s=>s.find({{role:"card",label:"Waveform"}}));
        settle();
        {body} }}"#
    ))
}

/// More steps on the timeline already open.
fn then(body: &str) -> String {
    support::script(&format!("{{ {HELPERS}\n{body} }}"))
}

fn run(harness: &mut gpui_agent::Harness, script: String) -> Value {
    let result = harness.exec(&script, Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    result.result
}

#[test]
fn fade_bend_and_level_handles_write_the_clip_alpha() {
    const NAME: &str = "clip-fades-handles";
    let mut harness = Fixture::new(NAME, 20, vec![])
        .with_graph_score(support::preset_score("Chase"))
        .with_rig()
        .window(1400., 900.)
        .open(Mode::Headless);

    // Drag the fade-in handle a quarter of the way into the clip.
    let out = run(
        &mut harness,
        opened(
            r#"
        const card=node("card","Chase").bounds;
        const handle=node("slider","Chase fade in");
        const start=handle.bounds.x+handle.bounds.width/2-card.x;
        app.drag(handle,{dx:card.width/4,dy:0},{steps:6});
        settle();
        until("form inputs",s=>s.find({role:"row",label:"Travel"}));
        ({start, handles:app.snapshot().findAll({role:"slider"}).map(n=>n.label).filter(l=>l.startsWith("Chase ")),
          rows:app.snapshot().findAll({role:"row"}).map(n=>n.label)})
    "#,
        ),
    );
    assert!(out["start"].as_f64().unwrap().abs() < 2., "{out}");
    let faded = alpha(NAME, "form-clip");
    let curve = points(&faded);
    assert_eq!(curve.len(), 3, "{faded}");
    assert!(close(curve[0].0, 0.) && close(curve[0].1, 0.), "{faded}");
    assert!(close(curve[1].0, 0.25) && close(curve[1].1, 1.), "{faded}");
    assert!(close(curve[2].0, 1.) && close(curve[2].1, 1.), "{faded}");
    let handles: Vec<&str> = out["handles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(handles.contains(&"Chase fade in bend"), "{out}");
    assert!(
        !out["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row == "Alpha"),
        "the sheet has no Alpha row: {out}"
    );

    // Bend the fade up: it becomes a Bézier above the straight line.
    run(
        &mut harness,
        then(
            r#"
        app.drag(node("slider","Chase fade in bend"),{dx:0,dy:-20},{steps:6});
        settle();
        ({})
    "#,
        ),
    );
    let bent = alpha(NAME, "form-clip");
    let bend = &bent["value"]["segments"][0]["bezier"];
    let (start, end) = (points(&bent)[0], points(&bent)[1]);
    let straight = start.1 + (end.1 - start.1) / 3.;
    assert!(
        bend["control1"][1].as_f64().unwrap() > straight + 1e-3,
        "{bent}"
    );

    // Pull the hold of the line halfway down.
    run(
        &mut harness,
        then(
            r#"
        const lane=app.snapshot().findAll({role:"row"}).find(n=>n.label==="Lane 1").bounds;
        const travel=lane.height-2-18-6;
        app.drag(node("slider","Chase alpha 2"),{dx:0,dy:travel/2},{steps:6});
        settle();
        ({travel})
    "#,
        ),
    );
    let lowered = alpha(NAME, "form-clip");
    let curve = points(&lowered);
    assert!(
        close(curve[1].1, 0.5) && close(curve[2].1, 0.5),
        "{lowered}"
    );
    assert!(
        lowered["value"]["segments"][0].get("bezier").is_some(),
        "the bend stays: {lowered}"
    );

    // Undo takes the level back.
    run(
        &mut harness,
        then(
            r#"
        app.key("secondary-z"); settle();
        ({})
    "#,
        ),
    );
    assert_eq!(alpha(NAME, "form-clip"), bent, "undo restores the level");

    // Drag the fade back to the clip's edge: alpha is Fixed again.
    run(
        &mut harness,
        then(
            r#"
        const edge=node("card","Chase").bounds.x;
        const handle=node("slider","Chase fade in");
        app.drag(handle,{dx:edge-(handle.bounds.x+handle.bounds.width/2),dy:0},{steps:8});
        settle();
        ({})
    "#,
        ),
    );
    assert_eq!(
        alpha(NAME, "form-clip"),
        json!({"type": "proportion", "value": 1.0}),
        "no fade left stores a plain value"
    );
}

#[test]
fn moving_a_clip_over_the_end_of_another_crosses_them() {
    const NAME: &str = "clip-fades-crossfade";
    let mut document: luma_patterns::Score =
        serde_json::from_value(support::score(json!({}))).unwrap();
    let presets = luma_patterns::presets();
    document.clips.insert(
        "first".into(),
        presets.preset("Wash").unwrap().clip(0.0, 4.0),
    );
    document.clips.insert(
        "second".into(),
        presets.preset("Chase").unwrap().clip(8.0, 4.0),
    );
    let mut harness = Fixture::new(NAME, 20, vec![])
        .with_graph_score(serde_json::to_value(document).unwrap())
        .with_rig()
        .window(1400., 900.)
        .open(Mode::Headless);
    let result = harness.exec(
        &opened(
            r#"
        const chase=node("card","Chase").bounds;
        const beat=chase.width/4;
        // From beats 8–12 to 2–6: two beats over the end of the first clip.
        app.drag(node("card","Chase"),{dx:-6*beat,dy:0},{steps:8});
        settle();
        ({beat, cards:app.snapshot().findAll({role:"card"}).map(n=>n.label)})
    "#,
        ),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let score = stored(NAME);
    let second = &score["clips"]["second"];
    assert!(
        close(second["start"].as_f64().unwrap(), 2.),
        "{second} {}",
        result.result
    );
    let out = points(&score["clips"]["first"]["inputs"]["alpha"]);
    assert_eq!(out.len(), 3, "{out:?}");
    assert!(
        close(out[1].0, 0.5) && close(out[2].0, 1.) && close(out[2].1, 0.),
        "{out:?}"
    );
    let into = points(&second["inputs"]["alpha"]);
    assert!(
        close(into[0].1, 0.) && close(into[1].0, 0.5) && close(into[1].1, 1.),
        "{into:?}"
    );

    // One undo takes back the move and both fades.
    let result = harness.exec(
        &then(r#"app.key("secondary-z"); settle(); ({})"#),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let score = stored(NAME);
    assert!(
        close(score["clips"]["second"]["start"].as_f64().unwrap(), 8.),
        "{}",
        result.result
    );
    for clip in ["first", "second"] {
        assert_eq!(
            score["clips"][clip]["inputs"]["alpha"],
            json!({"type": "proportion", "value": 1.0}),
            "{clip}"
        );
    }
}

#[test]
fn a_resize_keeps_the_fade_lengths() {
    const NAME: &str = "clip-fades-resize";
    let mut harness = Fixture::new(NAME, 20, vec![])
        .with_graph_score(support::preset_score("Chase"))
        .with_rig()
        .window(1400., 900.)
        .open(Mode::Headless);
    // A one-beat fade-in on the four-beat clip, then the clip's end pulled out
    // four beats further.
    run(
        &mut harness,
        opened(
            r#"
        const beat=node("card","Chase").bounds.width/4;
        app.drag(node("slider","Chase fade in"),{dx:beat,dy:0},{steps:6});
        settle();
        app.drag(node("slider","Chase end"),{dx:4*beat,dy:0},{steps:8});
        settle();
        ({})
    "#,
        ),
    );
    let score = stored(NAME);
    let clip = &score["clips"]["form-clip"];
    assert!(close(clip["duration"].as_f64().unwrap(), 8.), "{clip}");
    let curve = points(&clip["inputs"]["alpha"]);
    assert!(
        close(curve[1].0, 0.125) && close(curve[1].1, 1.),
        "the fade stays one beat long: {curve:?}"
    );

    // One undo takes back the resize and the refit together.
    run(
        &mut harness,
        then(r#"app.key("secondary-z"); settle(); ({})"#),
    );
    let score = stored(NAME);
    let clip = &score["clips"]["form-clip"];
    assert!(close(clip["duration"].as_f64().unwrap(), 4.), "{clip}");
    assert!(close(points(&clip["inputs"]["alpha"])[1].0, 0.25), "{clip}");
}

#[test]
fn a_segment_of_the_line_moves_up_and_down() {
    const NAME: &str = "clip-fades-segments";
    let mut harness = Fixture::new(NAME, 20, vec![])
        .with_graph_score(support::preset_score("Chase"))
        .with_rig()
        .window(1400., 900.)
        .open(Mode::Headless);
    const TRAVEL: &str = r#"
        const lane=app.snapshot().findAll({role:"row"}).find(n=>n.label==="Lane 1").bounds;
        const travel=lane.height-2-18-6;
    "#;
    // A fixed alpha is one segment. Dragged below the clip it stops at 0.
    run(
        &mut harness,
        opened(&format!(
            r#"{TRAVEL}
        // To the window's bottom edge, below the clip.
        const line=node("slider","Chase alpha 1");
        app.drag(line,{{dx:0,dy:899-(line.bounds.y+line.bounds.height/2)}},{{steps:8}});
        settle();
        ({{}})
    "#
        )),
    );
    assert_eq!(
        alpha(NAME, "form-clip"),
        json!({"type": "proportion", "value": 0.0})
    );
    // Back up to half, then a fade-in over the first quarter.
    run(
        &mut harness,
        then(&format!(
            r#"{TRAVEL}
        app.drag(node("slider","Chase alpha 1"),{{dx:0,dy:-travel/2}},{{steps:6}});
        settle();
        const card=node("card","Chase").bounds;
        app.drag(node("slider","Chase fade in"),{{dx:card.width/4,dy:0}},{{steps:6}});
        settle();
        ({{}})
    "#
        )),
    );
    let faded = points(&alpha(NAME, "form-clip"));
    assert!(
        close(faded[0].1, 0.) && close(faded[1].0, 0.25) && close(faded[1].1, 0.5),
        "{faded:?}"
    );
    // Lift the ramp a quarter: both of its points move, and the line is a
    // custom curve with no fade handles.
    let out = run(
        &mut harness,
        then(&format!(
            r#"{TRAVEL}
        app.drag(node("slider","Chase alpha 1"),{{dx:0,dy:-travel/4}},{{steps:6}});
        settle();
        ({{sliders:app.snapshot().findAll({{role:"slider"}}).map(n=>n.label).filter(l=>l.startsWith("Chase "))}})
    "#
        )),
    );
    let lifted = points(&alpha(NAME, "form-clip"));
    assert!(
        close(lifted[0].1, 0.25) && close(lifted[1].1, 0.75) && close(lifted[2].1, 0.5),
        "{lifted:?}"
    );
    let sliders = out["sliders"].to_string();
    assert!(!sliders.contains("fade in"), "{sliders}");
    assert!(sliders.contains("Chase alpha 2"), "{sliders}");

    // One undo takes the lift back.
    run(
        &mut harness,
        then(r#"app.key("secondary-z"); settle(); ({})"#),
    );
    let undone = points(&alpha(NAME, "form-clip"));
    assert!(
        close(undone[0].1, 0.) && close(undone[1].1, 0.5),
        "{undone:?}"
    );
}
