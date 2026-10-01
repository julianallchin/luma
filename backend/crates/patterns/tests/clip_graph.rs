//! Clip graphs: JSON, the checker's rules and messages (spec 3.1, 3.2),
//! the definitions and the summary.
use luma_patterns::clip_graph::{self, definitions, ClipGraph, Input, Kind, Node};
use luma_patterns::{presets, BlendMode, Clip, Selection};
use serde_json::{json, Value};

fn graph(nodes: Value) -> ClipGraph {
    serde_json::from_value(json!({"version": 3, "nodes": nodes})).expect("graph JSON")
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
            "version": 3,
            "nodes": {
                "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 0]]}, "low": 60, "high": 360}},
                "time1":  {"kind": "time",  "inputs": {"every": 2, "phase": {"node": "curve1"}}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 1], [0.1667, 1], [0.1667, 0], [1, 0]]}}},
                "time2":  {"kind": "time",  "inputs": {"every": 2}},
                "curve3": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time2"}, "shape": {"points": [[0, 1], [1, 0]]}}},
                "math1":  {"kind": "math", "settings": {"op": "*"},
                           "inputs": {"values": [{"node": "curve2"}, {"node": "curve3"}, 0.8]}},
                "color1": {"kind": "color", "inputs": {"color": [1, 1, 1], "brightness": {"node": "math1"}}}}}}}});
    let score: luma_patterns::Score = serde_json::from_value(doc.clone()).unwrap();
    let clip = &score.clips["3f24"];
    assert_eq!(
        clip.graph.nodes["color1"].inputs["color"],
        Input::Color([1., 1., 1.])
    );
    assert_eq!(
        clip.graph.nodes["math1"].inputs["values"],
        Input::List(vec![
            Input::wire("curve2"),
            Input::wire("curve3"),
            Input::Number(0.8)
        ])
    );
    // Two time nodes with the same every share one set of events.
    assert_eq!(clip.graph.clock_key("time1"), clip.graph.clock_key("time2"));
    clip_graph::check_clip(clip).unwrap();
    let again: luma_patterns::Score =
        serde_json::from_str(&serde_json::to_string(&score).unwrap()).unwrap();
    assert_eq!(again, score);
    let written: Value = serde_json::to_value(&score).unwrap();
    assert_eq!(
        written["clips"]["3f24"]["graph"]["nodes"]["time2"],
        json!({"kind": "time", "inputs": {"every": 2.0}})
    );
}

#[test]
fn unknown_fields_and_shapes_are_refused_when_read() {
    let unknown_kind =
        ClipGraph::from_json(r#"{"version": 3, "nodes": {"fog1": {"kind": "fog"}}}"#)
            .unwrap_err()
            .0;
    assert!(unknown_kind.contains("fog"), "{unknown_kind}");
    let extra = ClipGraph::from_json(
        r#"{"version": 3, "nodes": {"color1": {"kind": "color", "wires": {}}}}"#,
    )
    .unwrap_err()
    .0;
    assert!(extra.contains("wires"), "{extra}");
    let text = ClipGraph::from_json(
        r#"{"version": 3, "nodes": {"color1": {"kind": "color", "inputs": {"color": [1, "red"]}}}}"#,
    )
    .unwrap_err()
    .0;
    assert!(text.contains("three numbers"), "{text}");
    // Two numbers read as a list, which only a math node's values take.
    assert_eq!(
        error(json!({"color1": {"kind": "color", "inputs": {"color": [1, 1]}}})),
        "color1.color: expected a color (r, g, b) with each channel 0–1 or a color curve; got a list of 2 items. Example: color=(1, 1, 1)"
    );
}

// ---- rule 1: graph ----

#[test]
fn rule_1_graph() {
    assert_eq!(
        error(json!({"color1": {"kind": "color"}, "strobe1": {"kind": "strobe"}})),
        "graph: expected one output node; got color1 and strobe1. Example: one clip per output"
    );
    let mut wrong_version = graph(json!({"color1": {"kind": "color"}}));
    wrong_version.version = 1;
    assert_eq!(
        wrong_version.check().unwrap_err().0,
        r#"graph: expected version 3; got 1. Example: "version": 3"#
    );
    assert_eq!(
        error(json!({"Color 1": {"kind": "color"}})),
        r#"graph: expected node ids that are Python names of at most 32 letters, digits and _, such as cut or curve2; got "Color 1". Example: cut"#
    );
    assert_eq!(
        error(
            json!({"time": {"kind": "time"}, "color1": {"kind": "color", "inputs": {"alpha": {"node": "fade"}}},
                     "fade": {"kind": "curve", "inputs": {"x": {"node": "time"}}}})
        ),
        r#"graph: expected node ids that are not a builder name or a Python keyword; got "time". Example: time_1"#
    );
    // Any other Python name is an id: code, graph and card say the same.
    let named = graph(json!({"t": {"kind": "time"},
                             "fade_in": {"kind": "curve", "inputs": {"x": {"node": "t"}}},
                             "color1": {"kind": "color", "inputs": {"alpha": {"node": "fade_in"}}}}));
    named.check().unwrap();
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
            "time1": {"kind": "time", "inputs": {"every": every}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})
    };
    assert_eq!(
        error(clocked(json!(0))),
        "time1.every: expected beats above 0; got 0. Example: every=1, or leave it out for once over the clip"
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
        error(json!({"aim1": {"kind": "aim", "inputs": {"yaw": [10, 20, 30]}}})),
        "aim1.yaw: expected a number -180–180 (degrees) or a number curve; got (10,20,30). Example: yaw=0"
    );
    // A list goes only into a math node; brightness takes one wire.
    assert_eq!(
        error(json!({"color1": {"kind": "color", "inputs": {"brightness": [0.5, 1]}}})),
        "color1.brightness: expected a number 0–1 (share) or a number curve; got a list of 2 items. Example: brightness=1. Items multiply in a math node: brightness=a * b"
    );
    let math = |op: &str, values: Value| {
        json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "math1": {"kind": "math", "settings": {"op": op}, "inputs": {"values": values}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}}})
    };
    graph(math("*", json!([{"node": "curve1"}, 0.5, 2])))
        .check()
        .unwrap();
    assert_eq!(
        error(math("*", json!([{"node": "curve1"}]))),
        "math1.values: expected two or more items for *; got 1. Example: values=[curve1, curve2]"
    );
    assert_eq!(
        error(math("-", json!([1, {"node": "curve1"}, 0.5]))),
        "math1.values: expected exactly two items for -; got 3. Example: values=[curve1, curve2]"
    );
    graph(math("-", json!([1, {"node": "curve1"}])))
        .check()
        .unwrap();
    // A jump is two points at one x; three are refused.
    let jump = |points: Value| {
        json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "shape": {"points": points}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})
    };
    graph(jump(json!([[0, 0], [0, 1], [0.5, 1], [0.5, 0], [1, 0]])))
        .check()
        .unwrap();
    assert_eq!(
        error(jump(json!([[0, 0], [0.5, 1], [0.5, 0], [0.5, 1], [1, 0]]))),
        r#"curve1.shape: expected points with v 0–1; got points[3]: at most two points share an x (a jump); x 0.5 has three. Example: shape="Ramp up""#
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
            "time2": {"kind": "time", "inputs": {"delay": {"node": "curve3"}}},
            "curve4": {"kind": "curve", "inputs": {"x": {"node": "time2"}}},
            "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve3"}, "alpha": {"node": "curve4"}}}})),
        "curve3: expected one unit; it feeds aim1.yaw (degrees) and time2.delay (beats). Example: make two curves"
    );
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}}})),
        r#"curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow""#
    );
    // The checker names the curve bound to change, not the input.
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": -1, "high": 0.5}},
            "color1": {"kind": "color", "inputs": {"alpha": {"node": "curve1"}}}})),
        "curve1.low: expected a share between 0 and 1 for color1.alpha; got -1. Example: low=0"
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

#[test]
fn rule_4_math_takes_values_and_broadcasts() {
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "math1": {"kind": "math", "inputs": {"values": [{"node": "curve1"}, {"node": "time1"}]}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}}})),
        r#"math1.values: expected a list of two or more numbers and value wires; got a coordinate wire from time1. Example: values=curve(time1, "Ramp up")"#
    );
    // A number times a color is a color: it feeds color, not brightness.
    let gradient = json!({"stops": [{"t": 0, "color": [1, 0, 0]}, {"t": 1, "color": [0, 0, 1]}]});
    let tinted = |into: &str| {
        let mut nodes = json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "curve2": {"kind": "curve", "settings": {"kind": "color"},
                       "inputs": {"x": {"node": "time1"}, "gradient": gradient}},
            "math1": {"kind": "math", "inputs": {"values": [{"node": "curve1"}, {"node": "curve2"}]}},
            "color1": {"kind": "color", "inputs": {}}});
        nodes["color1"]["inputs"][into] = json!({"node": "math1"});
        nodes
    };
    let tint = graph(tinted("color"));
    tint.check().unwrap();
    assert_eq!(tint.value_kind("math1"), Some("color"));
    assert_eq!(
        error(tinted("brightness")),
        "color1.brightness: expected a number 0–1 (share) or a number curve; got a color wire from math1. Example: brightness=1"
    );
    // A curve through a math node keeps its unit.
    assert_eq!(
        error(json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "high": 30}},
            "math1": {"kind": "math", "inputs": {"values": [{"node": "curve1"}, 0.5]}},
            "time2": {"kind": "time", "inputs": {"delay": {"node": "curve1"}}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "time2"}}},
            "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "math1"}, "alpha": {"node": "curve2"}}}})),
        "curve1: expected one unit; it feeds aim1.yaw (degrees) and time2.delay (beats). Example: make two curves"
    );
}

// ---- rule 5: axes ----

#[test]
fn rule_5_axes() {
    assert_eq!(
        error(json!({
            "time2": {"kind": "time", "inputs": {"every": 2}},
            "curve4": {"kind": "curve", "inputs": {"x": {"node": "time2"}, "high": 180}},
            "time1": {"kind": "time", "inputs": {"every": 1, "phase": {"node": "curve4"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        "time1.phase: expected wires of one clock; got time2 through curve4 while time1 has its own events. Example: use the same every and duration for both"
    );
    // The same every and duration are one clock: a phase may follow it.
    graph(json!({
        "time2": {"kind": "time", "inputs": {"every": 2, "duration": 2}},
        "curve4": {"kind": "curve", "inputs": {"x": {"node": "time2"}, "high": 180}},
        "time1": {"kind": "time", "inputs": {"every": 2, "phase": {"node": "curve4"}}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}))
    .check()
    .unwrap();
    // A time node's own every cannot follow events.
    assert_eq!(
        error(json!({
            "time2": {"kind": "time", "inputs": {"every": 2}},
            "curve4": {"kind": "curve", "inputs": {"x": {"node": "time2"}, "low": 1, "high": 2}},
            "time1": {"kind": "time", "inputs": {"every": {"node": "curve4"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}})),
        r#"time1.every: expected a value or a curve over the clip; got events of time2 through curve4. Example: every=curve(time(), "Ramp down", low=0.5, high=2)"#
    );
    assert_eq!(
        error(json!({
            "space1": {"kind": "space"},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "mirror1": {"kind": "mirror", "inputs": {"at": {"node": "curve1"}}},
            "space2": {"kind": "space", "inputs": {"heads": {"node": "mirror1"}}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "space2"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}}})),
        "mirror1.at: expected one value for all heads (a value or a curve over time); got a wire that varies over heads from curve1. Example: at=0.5"
    );
    // Two clocks may meet at the output node.
    graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1}},
        "time2": {"kind": "time", "inputs": {"every": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "curve2": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "time2"}, "gradient": {"stops": [{"t": 0, "color": [0, 0, 0]}, {"t": 1, "color": [1, 1, 1]}]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}, "color": {"node": "curve2"}}}}))
    .check()
    .unwrap();
}

#[test]
fn rule_5_a_math_node_follows_one_clock() {
    let nodes = |second: f64| {
        json!({
            "time1": {"kind": "time", "inputs": {"every": 1}},
            "time2": {"kind": "time", "inputs": {"every": second}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "time2"}}},
            "math1": {"kind": "math", "inputs": {"values": [{"node": "curve1"}, {"node": "curve2"}]}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}}})
    };
    assert_eq!(
        error(nodes(2.)),
        "math1.values: expected wires of one clock; got time2 through curve2 while values follows time1. Example: use the same every and duration for both"
    );
    graph(nodes(1.)).check().unwrap();
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
        r#"clip: expected a blend mode for color: replace, add, multiply, screen, max, min or subtract; got offset. Example: blend="replace""#
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
        Kind::Time | Kind::Space | Kind::Noise | Kind::Audio => {
            nodes.push(("curve9".into(), curve_of(&id)));
            nodes.push(("color9".into(), color("curve9")));
        }
        Kind::Curve => {
            node.inputs.insert("x".into(), Input::wire("time9"));
            nodes.push(("time9".into(), Node::new(Kind::Time)));
            nodes.push(("color9".into(), color(&id)));
        }
        Kind::Math => {
            node.inputs.insert(
                "values".into(),
                Input::List(vec![Input::wire("curve9"), Input::Number(0.5)]),
            );
            nodes.push(("time9".into(), Node::new(Kind::Time)));
            nodes.push(("curve9".into(), curve_of("time9")));
            nodes.push(("color9".into(), color(&id)));
        }
        Kind::Value => {
            node.inputs.insert("value".into(), Input::Number(0.5));
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
    assert!(space["inputs"].get("width").is_none());
    assert_eq!(space["inputs"]["shift"]["range"], json!(null));
    assert_eq!(space["inputs"]["scale"]["range"], json!([0.0, null]));
    assert!(space["inputs"].get("length").is_none());
    let time = serde_json::to_value(clip_graph::definition(Kind::Time)).unwrap();
    assert_eq!(time["inputs"]["delay"]["range"], json!(null));
    assert_eq!(time["inputs"]["delay"]["unit"], "beats");
    assert_eq!(time["inputs"]["every"]["unit"], "beats");
    assert!(time["inputs"].get("clock").is_none());
    // A phase is degrees, 360 one event, and any number: it wraps.
    assert_eq!(time["inputs"]["phase"]["unit"], "degrees");
    assert_eq!(time["inputs"]["phase"]["range"], json!(null));
    let phase = clip_graph::definition(Kind::Time).input("phase").unwrap();
    assert_eq!(phase.example, "90");
    let math = serde_json::to_value(clip_graph::definition(Kind::Math)).unwrap();
    assert_eq!(math["output"], "value");
    assert_eq!(math["settings"]["op"]["default"], "*");
    let mirror = serde_json::to_value(clip_graph::definition(Kind::Mirror)).unwrap();
    assert_eq!(mirror["inputs"]["at"]["range"], json!([0.0, 1.0]));
    assert_eq!(space["settings"]["kind"]["default"], "line");
}

// ---- value nodes ----

/// A value node takes its type from the inputs it feeds: a number input, a
/// vector input or a color input; through a curve's low or high, the
/// curve's kind. Unwired inputs that only change over time take it too.
#[test]
fn a_value_takes_its_type_from_what_it_feeds() {
    let slash = graph(json!({
        "d": {"kind": "value", "inputs": {"value": [0.57, 0, 0.82]}},
        "at": {"kind": "value", "inputs": {"value": 0.68}},
        "line": {"kind": "mirror", "inputs": {"direction": {"node": "d"}, "at": {"node": "at"}}},
        "dist": {"kind": "space", "inputs": {"heads": {"node": "line"}, "direction": {"node": "d"}, "shift": {"node": "at"}}},
        "bloom": {"kind": "curve", "inputs": {"x": {"node": "dist"}}},
        "red": {"kind": "value", "inputs": {"value": [1, 0, 0]}},
        "low": {"kind": "value", "inputs": {"value": 0.2}},
        "t": {"kind": "time"},
        "fade": {"kind": "curve", "inputs": {"x": {"node": "t"}, "low": {"node": "low"}}},
        "color1": {"kind": "color", "inputs": {"color": {"node": "red"}, "brightness": {"node": "bloom"}, "alpha": {"node": "fade"}}}}));
    slash.check().unwrap();
    assert_eq!(slash.value_kind("d"), Some("vector"));
    assert_eq!(slash.value_kind("at"), Some("number"));
    // Stored as a bare 3-array, read as a color because it feeds one.
    assert_eq!(slash.value_kind("red"), Some("color"));
    assert_eq!(slash.value_kind("low"), Some("number"));
    let json = slash.to_json();
    assert_eq!(ClipGraph::from_json(&json).unwrap(), slash);
    assert!(
        json.contains(r#""d":{"kind":"value","inputs":{"value":[0.57,0.0,0.82]}}"#),
        "{json}"
    );
}

#[test]
fn a_value_feeding_two_units_or_two_types_is_refused_with_an_example() {
    let units = error(json!({
        "d": {"kind": "value", "inputs": {"value": 0.5}},
        "t": {"kind": "time", "inputs": {"delay": {"node": "d"}}},
        "place": {"kind": "space", "inputs": {"shift": {"node": "d"}}},
        "a": {"kind": "curve", "inputs": {"x": {"node": "t"}}},
        "b": {"kind": "curve", "inputs": {"x": {"node": "place"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "a"}, "alpha": {"node": "b"}}}}));
    assert_eq!(
        units,
        "d: expected one unit; it feeds place.shift (share) and t.delay (beats). Example: one value per unit, such as d = value(0.5) and d_2 = value(0.5)"
    );
    let types = error(json!({
        "d": {"kind": "value", "inputs": {"value": [1, 0, 0]}},
        "line": {"kind": "mirror", "inputs": {"direction": {"node": "d"}}},
        "dist": {"kind": "space", "inputs": {"heads": {"node": "line"}}},
        "bloom": {"kind": "curve", "inputs": {"x": {"node": "dist"}}},
        "color1": {"kind": "color", "inputs": {"color": {"node": "d"}, "brightness": {"node": "bloom"}}}}));
    assert_eq!(
        types,
        "d: expected one type; it feeds color1.color (color) and line.direction (vector). Example: one value per type, such as d = value((1, 1, 1)) and d_2 = value((1, 0, 0))"
    );
    let shape = error(json!({
        "d": {"kind": "value", "inputs": {"value": [1, 0, 0]}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "d"}}}}));
    assert_eq!(
        shape,
        "d.value: expected a number because d feeds color1.brightness; got (1, 0, 0). Example: d = value(0.5)"
    );
    // A value is checked in every input it feeds, as if written there.
    let range = error(json!({
        "d": {"kind": "value", "inputs": {"value": 1.5}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "d"}}}}));
    assert_eq!(
        range,
        "color1.brightness: expected a share between 0 and 1; got d = 1.5. Example: brightness=1"
    );
    let bound = error(json!({
        "d": {"kind": "value", "inputs": {"value": 2}},
        "t": {"kind": "time"},
        "fade": {"kind": "curve", "inputs": {"x": {"node": "t"}, "high": {"node": "d"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "fade"}}}}));
    assert!(
        bound.starts_with(
            "fade.high: expected a share between 0 and 1 for color1.brightness; got 2."
        ),
        "{bound}"
    );
}

#[test]
fn a_value_holds_a_value_and_no_wire() {
    let wired = error(json!({
        "t": {"kind": "time"},
        "c": {"kind": "curve", "inputs": {"x": {"node": "t"}}},
        "d": {"kind": "value", "inputs": {"value": {"node": "c"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "d"}}}}));
    assert!(
        wired.starts_with("d.value: expected a finite number, or three"),
        "{wired}"
    );
    let empty = error(json!({
        "d": {"kind": "value"},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "d"}}}}));
    assert!(empty.starts_with("d.value: expected"), "{empty}");
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
