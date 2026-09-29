//! Clip graphs: JSON, the checker's rules and messages (spec 3.1, 3.2),
//! the definitions and the summary.
use luma_patterns::clip_graph::{self, definitions, ClipGraph, Input, Kind, Node};
use luma_patterns::{presets, BlendMode, Clip, Selection};
use serde_json::{json, Value};

fn graph(nodes: Value) -> ClipGraph {
    serde_json::from_value(json!({"version": 1, "nodes": nodes})).expect("graph JSON")
}

fn error(nodes: Value) -> String {
    graph(nodes).check().expect_err("the checker refuses it").0
}

fn clip(graph: ClipGraph) -> Clip {
    Clip {
        name: "Chase".into(),
        start: 32.,
        duration: 8.,
        seed: 7,
        selection_seed: None,
        selection: Selection::all(),
        z_index: 0,
        blend_mode: BlendMode::Replace,
        graph,
    }
}

#[test]
fn every_preset_round_trips_and_passes() {
    let shipped = presets();
    assert!(!shipped.clips.is_empty());
    for preset in &shipped.clips {
        let json = preset.graph.to_json();
        let back = ClipGraph::from_json(&json).unwrap();
        assert_eq!(back, preset.graph, "{} round trip", preset.name);
        preset
            .graph
            .check()
            .unwrap_or_else(|e| panic!("{}: {e}", preset.name));
        clip_graph::check_clip(&preset.clip(0., 16.))
            .unwrap_or_else(|e| panic!("{}: {e}", preset.name));
    }
    let whole = serde_json::to_string(shipped).unwrap();
    assert_eq!(
        &serde_json::from_str::<luma_patterns::Presets>(&whole).unwrap(),
        shipped
    );
}

#[test]
fn preset_names_are_unique_and_shapes_resolve() {
    let shipped = presets();
    let mut names: Vec<&str> = shipped.clips.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), shipped.clips.len());
    for name in [
        "On", "Ramp up", "Soft", "Comet", "Sine", "Cosine", "Steps 4",
    ] {
        assert!(shipped.curve(name).is_some(), "{name}");
    }
    assert!(shipped.gradient("Rainbow").is_some());
    assert_eq!(shipped.band("Kick"), Some((40., 100.)));
    let aim = shipped.clip("Circle").unwrap();
    assert_eq!(aim.output_kind(), Kind::Aim);
    assert_eq!(aim.blend_mode, BlendMode::Offset);
}

#[test]
fn score_json_example_round_trips() {
    let doc = json!({"clips": {"3f24": {
        "name": "Chase",
        "start": 32.0, "duration": 8.0, "seed": 6348896133488684926u64,
        "selection": {"expression": "led_bars_vertical"},
        "z_index": 0, "blend_mode": "replace",
        "graph": {
            "version": 1,
            "nodes": {
                "clock1": {"kind": "clock", "inputs": {"every": 2}},
                "time1":  {"kind": "time",  "inputs": {"clock": {"node": "clock1"}}},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 0], [1, 1]]}, "low": -0.2, "high": 1}},
                "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                           "inputs": {"offset": {"node": "curve1"}, "width": 0.2}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
                "color1": {"kind": "color", "inputs": {"color": [1, 1, 1], "brightness": {"node": "curve2"}}}}}}}});
    let score: luma_patterns::Score = serde_json::from_value(doc.clone()).unwrap();
    let clip = &score.clips["3f24"];
    assert_eq!(
        clip.graph.nodes["color1"].inputs["color"],
        Input::Color([1., 1., 1.])
    );
    assert_eq!(
        clip.graph.nodes["space1"].inputs["width"],
        Input::Number(0.2)
    );
    clip_graph::check_clip(clip).unwrap();
    let again: luma_patterns::Score =
        serde_json::from_str(&serde_json::to_string(&score).unwrap()).unwrap();
    assert_eq!(again, score);
    let written: Value = serde_json::to_value(&score).unwrap();
    assert_eq!(
        written["clips"]["3f24"]["graph"]["nodes"]["time1"],
        json!({"kind": "time", "inputs": {"clock": {"node": "clock1"}}})
    );
}

#[test]
fn unknown_fields_and_shapes_are_refused_when_read() {
    let unknown_kind =
        ClipGraph::from_json(r#"{"version": 1, "nodes": {"fog1": {"kind": "fog"}}}"#)
            .unwrap_err()
            .0;
    assert!(unknown_kind.contains("fog"), "{unknown_kind}");
    let extra = ClipGraph::from_json(
        r#"{"version": 1, "nodes": {"color1": {"kind": "color", "wires": {}}}}"#,
    )
    .unwrap_err()
    .0;
    assert!(extra.contains("wires"), "{extra}");
    let pair = ClipGraph::from_json(
        r#"{"version": 1, "nodes": {"color1": {"kind": "color", "inputs": {"color": [1, 1]}}}}"#,
    )
    .unwrap_err()
    .0;
    assert!(pair.contains("three numbers"), "{pair}");
}

// ---- rule 1: graph ----

#[test]
fn rule_1_graph() {
    assert_eq!(
        error(json!({"color1": {"kind": "color"}, "strobe1": {"kind": "strobe"}})),
        "graph: expected one output node; got color1 and strobe1. Example: one clip per output"
    );
    let mut wrong_version = graph(json!({"color1": {"kind": "color"}}));
    wrong_version.version = 2;
    assert_eq!(
        wrong_version.check().unwrap_err().0,
        r#"graph: expected version 1; got 2. Example: "version": 1"#
    );
    assert_eq!(
        error(json!({"Color": {"kind": "color"}})),
        r#"graph: expected node ids such as curve2; got "Color". Example: curve2"#
    );
    assert_eq!(
        error(json!({"color1": {"kind": "color"}, "time1": {"kind": "time"}})),
        "time1: expected a wire into the output through other nodes; nothing reads it. Example: wire it into an input, or delete it"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}, "low": {"node": "curve2"}}},
            "curve2": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}, "low": {"node": "curve1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        "graph: expected no loop; got curve1 → curve2 → curve1. Example: wire each node only into nodes after it"
    );
    assert_eq!(
        error(json!({"color1": {"kind": "color", "inputs": {"brightness": {"node": "curve9"}}}})),
        "color1.brightness: expected a wire to a node in this graph; got a wire to curve9. Example: brightness=1"
    );
}

// ---- rule 2: node ----

#[test]
fn rule_2_node() {
    assert_eq!(
        error(json!({"color1": {"kind": "color", "inputs": {"fade": 1}}})),
        "color1.fade: expected an input of color: color, brightness and alpha; got fade. Example: color=(1, 1, 1)"
    );
    assert_eq!(
        error(json!({
            "space1": {"kind": "space", "settings": {"kind": "diagonal"}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        r#"space1.kind: expected line, order, radial or angle; got "diagonal". Example: kind="line""#
    );
    assert_eq!(
        error(json!({"strobe1": {"kind": "strobe", "settings": {"base": "point"}}})),
        "strobe1.base: expected no settings on strobe; got base. Example: strobe()"
    );
}

// ---- rule 3: value ----

#[test]
fn rule_3_value() {
    let clocked = |every: Value| {
        json!({
            "clock1": {"kind": "clock", "inputs": {"every": every}},
            "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})
    };
    assert_eq!(
        error(clocked(json!(0))),
        "clock1.every: expected beats above 0; got 0. Example: every=1, or leave the clock out for once over the clip"
    );
    assert_eq!(
        error(json!({"color1": {"kind": "color", "inputs": {"brightness": 2}}})),
        "color1.brightness: expected a share between 0 and 1; got 2. Example: brightness=1"
    );
    assert_eq!(
        error(json!({"aim1": {"kind": "aim", "inputs": {"direction": [0, 0, 0]}}})),
        "aim1.direction: expected a direction that is not zero; got (0,0,0). Example: direction=(0, 0.766, -0.643)"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 0], [1, 2]]}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        r#"curve1.shape: expected points with v 0–1; got points[1]: v 2 must be in 0–1. Example: shape="Ramp up""#
    );
    assert_eq!(
        error(json!({
            "audio1": {"kind": "audio", "inputs": {"low_hz": 200}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "audio1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        "audio1.high_hz: expected Hz above low_hz (200); got 100. Example: high_hz=400"
    );
    assert_eq!(
        error(json!({"color1": {"kind": "color", "inputs": {"brightness": [1, 0, 0]}}})),
        "color1.brightness: expected a number 0–1 (share) or a number curve; got (1,0,0). Example: brightness=1"
    );
}

// ---- rule 4: wire ----

#[test]
fn rule_4_wire() {
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "time1"}}}})),
        r#"color1.brightness: expected a number 0–1 (share) or a number curve; got a coordinate wire from time1. Example: brightness=curve(time1, "Ramp up")"#
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 400, "high": 30}},
            "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve2"}}}})),
        "curve2.low: expected degrees between -180 and 180 for aim1.yaw; got 400. Example: low=-30"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve3": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "space1": {"kind": "space", "inputs": {"width": {"node": "curve3"}}},
            "curve4": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve3"}, "alpha": {"node": "curve4"}}}})),
        "curve3: expected one unit; it feeds aim1.yaw (degrees) and space1.width (share). Example: make two curves"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}}})),
        r#"curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow""#
    );
    // The spec's example reports this at space1.width; the checker names
    // the curve bound to change instead.
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": -1, "high": 0.5}},
            "space1": {"kind": "space", "inputs": {"width": {"node": "curve1"}}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}}})),
        "curve1.low: expected a share between 0 and 4 for space1.width; got -1. Example: low=0"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve5": {"kind": "curve", "settings": {"kind": "vector"}, "inputs": {"x": {"node": "time1"}, "low": 0, "high": [0, 0.5, -1]}},
            "aim1": {"kind": "aim", "inputs": {"direction": {"node": "curve5"}}}})),
        "curve5.low: expected a vector for aim1.direction; got 0. Example: low=(0, 1, -0.5), high=(0, 0.5, -1)"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve5": {"kind": "curve", "settings": {"kind": "vector"}, "inputs": {"x": {"node": "time1"}, "low": [1, 0, 0], "high": [-1, 0, 0]}},
            "space1": {"kind": "space", "inputs": {"direction": {"node": "curve5"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        r#"curve5: expected low and high not opposite for space1.direction; got (1,0,0) and (-1,0,0). Example: rotate with kind="angle" instead"#
    );
    assert_eq!(
        error(json!({
            "space1": {"kind": "space"},
            "noise2": {"kind": "noise", "inputs": {"heads": {"node": "space1"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "noise2"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        "noise2.heads: expected a heads wire; got a coordinate wire from space1. Example: heads=group()"
    );
}

// ---- rule 5: axes ----

#[test]
fn rule_5_axes() {
    assert_eq!(
        error(json!({
            "clock1": {"kind": "clock", "inputs": {"every": 1}},
            "clock2": {"kind": "clock", "inputs": {"every": 2}},
            "time2": {"kind": "time", "inputs": {"clock": {"node": "clock2"}}},
            "curve4": {"kind": "curve", "inputs": {"x": {"node": "time2"}, "high": 0.5}},
            "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}, "phase": {"node": "curve4"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        "time1.phase: expected wires of one clock; got clock2 through curve4 while clock follows clock1. Example: use the same clock for both"
    );
    assert_eq!(
        error(json!({
            "space1": {"kind": "space"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "mirror1": {"kind": "mirror", "inputs": {"offset": {"node": "curve1"}}},
            "space2": {"kind": "space", "inputs": {"heads": {"node": "mirror1"}}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "space2"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}}})),
        "mirror1.offset: expected one value for all heads (a value or a curve over time); got a wire that varies over heads from curve1. Example: offset=0"
    );
    // Two clocks may meet at the output node.
    graph(json!({
        "clock1": {"kind": "clock", "inputs": {"every": 1}},
        "clock2": {"kind": "clock", "inputs": {"every": 2}},
        "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
        "time2": {"kind": "time", "inputs": {"clock": {"node": "clock2"}}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "curve2": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "time2"}, "gradient": {"stops": [{"t": 0, "color": [0, 0, 0]}, {"t": 1, "color": [1, 1, 1]}]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}, "color": {"node": "curve2"}}}}))
    .check()
    .unwrap();
}

// ---- rule 6: clip ----

#[test]
fn rule_6_clip() {
    let wash = graph(json!({"color1": {"kind": "color"}}));
    let mut unnamed = clip(wash.clone());
    unnamed.name = " ".into();
    assert_eq!(
        clip_graph::check_clip(&unnamed).unwrap_err().0,
        r#"clip: expected a name; got none. Example: name="Kick chase""#
    );
    let mut offset = clip(wash.clone());
    offset.blend_mode = BlendMode::Offset;
    assert_eq!(
        clip_graph::check_clip(&offset).unwrap_err().0,
        r#"clip: expected a blend mode for color: replace, add, multiply, screen, max, min, lighten, value or subtract; got offset. Example: blend="replace""#
    );
    let mut empty = clip(wash.clone());
    empty.duration = 0.;
    assert_eq!(
        clip_graph::check_clip(&empty).unwrap_err().0,
        "clip: expected a finite start and a duration above 0; got start 32 and duration 0. Example: beats=(32, 40)"
    );
    let score = luma_patterns::Score {
        clips: [("c1".to_string(), unnamed)].into(),
    };
    assert_eq!(
        score
            .validate(&luma_patterns::standard_library())
            .unwrap_err()
            .0,
        r#"clip (c1): clip: expected a name; got none. Example: name="Kick chase""#
    );
}

// ---- definitions ----

/// A node of `kind` from its definition's defaults, placed in the smallest
/// graph that uses it: wire-only inputs a default cannot fill get a
/// minimal source, and the node feeds an output.
fn smallest_graph(kind: Kind) -> ClipGraph {
    let definition = clip_graph::definition(kind);
    let mut node = Node::new(kind);
    for (name, def) in &definition.inputs {
        if let Some(value) = &def.default {
            node.inputs.insert(name.to_string(), value.clone());
        }
    }
    let id = format!("{}1", kind.name());
    let mut nodes = vec![];
    let color = |from: &str| Node::new(Kind::Color).with_input("brightness", Input::wire(from));
    let curve_of = |x: &str| Node::new(Kind::Curve).with_input("x", Input::wire(x));
    match kind {
        Kind::Color | Kind::Aim | Kind::Strobe => {}
        Kind::Clock => {
            nodes.push((
                "time9".into(),
                Node::new(Kind::Time).with_input("clock", Input::wire(&id)),
            ));
            nodes.push(("curve9".into(), curve_of("time9")));
            nodes.push(("color9".into(), color("curve9")));
        }
        Kind::Time | Kind::Space | Kind::Noise | Kind::Audio => {
            nodes.push(("curve9".into(), curve_of(&id)));
            nodes.push(("color9".into(), color("curve9")));
        }
        Kind::Curve => {
            node.inputs.insert("x".into(), Input::wire("time9"));
            nodes.push(("time9".into(), Node::new(Kind::Time)));
            nodes.push(("color9".into(), color(&id)));
        }
        Kind::Mirror | Kind::Shuffle | Kind::Group | Kind::Split => {
            nodes.push((
                "space9".into(),
                Node::new(Kind::Space).with_input("heads", Input::wire(&id)),
            ));
            nodes.push(("curve9".into(), curve_of("space9")));
            nodes.push(("color9".into(), color("curve9")));
        }
    }
    nodes.push((id, node));
    ClipGraph::new(nodes)
}

#[test]
fn definition_defaults_pass_the_checker() {
    assert_eq!(definitions().len(), Kind::ALL.len());
    for kind in Kind::ALL {
        smallest_graph(kind)
            .check()
            .unwrap_or_else(|e| panic!("{}: {e}", kind.name()));
    }
    let space = serde_json::to_value(clip_graph::definition(Kind::Space)).unwrap();
    assert_eq!(space["output"], "coordinate");
    assert_eq!(space["inputs"]["direction"]["axes"], json!(["T"]));
    assert_eq!(space["inputs"]["width"]["range"], json!([0.0, 4.0]));
    assert_eq!(space["settings"]["kind"]["default"], "line");
}

// ---- summary ----

#[test]
fn summary_names_the_moving_parts() {
    let summary = |name: &str| presets().clip(name).unwrap().graph.summary();
    assert_eq!(summary("Chase"), "line · every 2");
    assert_eq!(summary("Wash"), "still");
    assert_eq!(summary("Follows a band"), "audio 40–100 Hz");
    assert_eq!(summary("Sparkle"), "order · every 0.125");
}
