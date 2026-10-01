//! Clip graphs played: every preset lights a stand-in rig and plays as its
//! version 2 form did, the key presets light the heads they should, and
//! shuffle, curve, per-head clocks, math, overlap, mirrors, wrapping,
//! turning line and noise behave as the spec says.
use luma_patterns::clip_graph::{ClipGraph, OUTPUT};
use luma_patterns::{presets, standard_library, Cell, FeatureRequest, FeatureSource, Frame};
use luma_patterns::{PreparedGraph, Result};
use ndarray::Array3;
use serde_json::{json, Value};
use std::sync::Arc;

const DIMMER: usize = 3;
const STROBE: usize = 6;
const AIM_WEIGHT: usize = 11;

fn cell(id: String, uvz: [f64; 3]) -> Cell {
    Cell {
        id,
        group: "rig".into(),
        world: [uvz[0], -uvz[1], uvz[2]],
        uvz,
    }
}

/// Four vertical bars of three heads, left to right.
fn bars() -> Vec<Cell> {
    (0..4)
        .flat_map(|bar| {
            (1..=3).map(move |head| {
                cell(
                    format!("bar{bar}:{head}"),
                    [bar as f64 - 1.5, 0., head as f64],
                )
            })
        })
        .collect()
}

/// Twelve heads on a line along U, ids in the same order.
fn line() -> Vec<Cell> {
    (0..12)
        .map(|n| cell(format!("p{n:02}"), [n as f64, 0., 3.]))
        .collect()
}

/// Twelve heads on a ring facing the audience, head n at n/12 of a turn.
fn ring() -> Vec<Cell> {
    (0..12)
        .map(|n| {
            let angle = std::f64::consts::TAU * n as f64 / 12.;
            cell(format!("r{n:02}"), [angle.cos(), 0., 5. + angle.sin()])
        })
        .collect()
}

/// A 4 × 4 grid in the U–Z plane, symmetric about its centre.
fn grid() -> Vec<Cell> {
    let at = [-1.5, -0.5, 0.5, 1.5];
    (0..4)
        .flat_map(|i| (0..4).map(move |j| cell(format!("g{i}{j}"), [at[i], 0., at[j]])))
        .collect()
}

#[derive(Debug)]
struct Track;
impl FeatureSource for Track {
    fn sample(&self, _: &FeatureRequest, beat: f64) -> Result<f64> {
        Ok(1. + (beat * 1.7).sin())
    }
    fn range(&self, _: &FeatureRequest) -> Result<(f64, f64)> {
        Ok((0., 2.))
    }
}

fn graph(nodes: Value) -> ClipGraph {
    serde_json::from_value(json!({"version": 3, "nodes": nodes})).expect("graph JSON")
}

/// The lighting of `graph` over `cells` at `beats`, for a clip over beats
/// 0–16: (head, time, channel), heads in id order.
fn play(graph: &ClipGraph, cells: &[Cell], beats: &[f64]) -> Array3<f64> {
    run(graph, cells, beats).0
}

fn run(graph: &ClipGraph, cells: &[Cell], beats: &[f64]) -> (Array3<f64>, Option<Array3<f64>>) {
    run_for(graph, cells, beats, 16.)
}

/// [`run`] for a clip over beats 0 to `clip_duration`.
fn run_for(
    graph: &ClipGraph,
    cells: &[Cell],
    beats: &[f64],
    clip_duration: f64,
) -> (Array3<f64>, Option<Array3<f64>>) {
    let prepared = PreparedGraph::new(
        &standard_library(),
        graph,
        Frame {
            features: None,
            cells,
            beat: 0.,
            clip_start: 0.,
            clip_duration,
            seed: 7,
        },
    )
    .unwrap_or_else(|e| panic!("prepare: {e}"));
    let prepared = if prepared.feature_requests().is_empty() {
        prepared
    } else {
        prepared.with_features(Arc::new(Track)).unwrap()
    };
    let out = prepared
        .evaluate_batch(beats)
        .unwrap_or_else(|e| panic!("evaluate: {e}"));
    let light = out[OUTPUT].lighting().expect("lighting").values().clone();
    let turn = out
        .get(luma_patterns::aim::TURN_OUTPUT)
        .map(|v| v.signal().unwrap().values().clone());
    (light, turn)
}

fn beats(step: f64) -> Vec<f64> {
    (0..(16. / step) as usize)
        .map(|i| i as f64 * step)
        .collect()
}

fn lit(light: &Array3<f64>, t: usize) -> Vec<usize> {
    (0..light.dim().0)
        .filter(|n| light[[*n, t, DIMMER]] > 1e-6)
        .collect()
}

#[test]
fn every_preset_lights_the_rig() {
    let cells = bars();
    let beats = beats(0.25);
    for preset in &presets().clips {
        luma_patterns::Score::validate_clip(&standard_library(), "c1", &preset.clip(0., 16.))
            .unwrap_or_else(|e| panic!("{e}"));
        let light = play(&preset.graph, &cells, &beats);
        let channel = match preset.output_kind() {
            luma_patterns::clip_graph::Kind::Color => DIMMER,
            luma_patterns::clip_graph::Kind::Strobe => STROBE,
            _ => AIM_WEIGHT,
        };
        let peak = light
            .slice(ndarray::s![.., .., channel])
            .iter()
            .copied()
            .fold(0., f64::max);
        assert!(peak > 0., "{} gives no light or aim", preset.name);
    }
}

/// A curve over a space along U: its shape is a static region.
fn over_line(points: Value, wrap: &str) -> ClipGraph {
    graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "line", "wrap": wrap},
                   "inputs": {"direction": [1, 0, 0]}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": points}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}))
}

#[test]
fn a_curve_with_a_jump_over_space_lights_a_region() {
    let light = play(
        &over_line(json!([[0, 1], [0.5, 1], [0.5, 0], [1, 0]]), "no"),
        &line(),
        &[1.],
    );
    assert_eq!(lit(&light, 0), (0..6).collect::<Vec<_>>());
    let far = play(
        &over_line(json!([[0, 0], [0.9, 0], [0.9, 1], [1, 1]]), "no"),
        &line(),
        &[1.],
    );
    assert_eq!(lit(&far, 0), vec![10, 11]);
    // At a jump x itself reads the value after it, as a shader's step:
    // each piece covers [a, b), at x 0 and x 1 too. A head exactly on an
    // edge takes the side it goes into: a pulse on [0, 1) lights every head
    // but the one at 1, and a cut lit below 0 lights none.
    let edge = 5. / 11.;
    let tie = play(
        &over_line(json!([[0, 1], [edge, 1], [edge, 0], [1, 0]]), "no"),
        &line(),
        &[1.],
    );
    assert_eq!(lit(&tie, 0), (0..5).collect::<Vec<_>>());
    let rising = play(
        &over_line(json!([[0, 0], [edge, 0], [edge, 1], [1, 1]]), "no"),
        &line(),
        &[1.],
    );
    assert_eq!(lit(&rising, 0), (5..12).collect::<Vec<_>>());
    let closed = play(
        &over_line(json!([[0, 0], [0, 1], [1, 1], [1, 0]]), "no"),
        &line(),
        &[1.],
    );
    assert_eq!(lit(&closed, 0), (0..11).collect::<Vec<_>>());
    let cut = play(
        &over_line(json!([[0, 1], [0, 0], [1, 0]]), "no"),
        &line(),
        &[1.],
    );
    assert!(lit(&cut, 0).is_empty());
    // Two pills side by side, [0, 0.5) and [0.5, 1), share no head.
    let left = lit(&tie, 0);
    let right = lit(
        &play(
            &over_line(json!([[0, 0], [edge, 0], [edge, 1], [1, 1], [1, 0]]), "no"),
            &line(),
            &[1.],
        ),
        0,
    );
    assert!(left.iter().all(|n| !right.contains(n)));
    assert_eq!(left.len() + right.len(), 11);
}

#[test]
fn a_shuffle_lights_the_same_set_within_an_event_and_a_new_one_after() {
    let graph = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1}},
        "shuffle1": {"kind": "shuffle", "inputs": {"time": {"node": "time1"}}},
        "space1": {"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                   "inputs": {"heads": {"node": "shuffle1"}}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let times: Vec<f64> = (0..8)
        .flat_map(|k| [k as f64 + 0.2, k as f64 + 0.7])
        .collect();
    let light = play(&graph, &bars(), &times);
    let sets: Vec<Vec<usize>> = (0..times.len()).map(|t| lit(&light, t)).collect();
    for set in &sets {
        assert_eq!(set.len(), 6, "round(0.5 × 12) units");
    }
    for k in 0..8 {
        assert_eq!(sets[2 * k], sets[2 * k + 1], "one set within event {k}");
    }
    assert!((0..7).any(|k| sets[2 * k] != sets[2 * k + 2]));
}

#[test]
fn a_delayed_clock_reads_the_first_point_before_it_starts_and_the_last_after() {
    // Head n starts 12 × n / 11 beats late and runs for 4 beats.
    let graph = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0]}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "low": 0, "high": 12}},
        "time1": {"kind": "time", "inputs": {"delay": {"node": "curve1"}, "duration": 4}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "time1"},
                   "shape": {"points": [[0, 0.2], [0, 0.4], [1, 0.6], [1, 0.8]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}}}));
    // At beat 6 head 0 is done, head 11 (12 beats late) has not started,
    // and head 2 (2.18 beats late) is on its way.
    let light = play(&graph, &line(), &[6.]);
    assert!((light[[0, 0, DIMMER]] - 0.8).abs() < 1e-9);
    assert!((light[[11, 0, DIMMER]] - 0.2).abs() < 1e-9);
    let local = (6. - 12. * 2. / 11.) / 4.;
    assert!((light[[2, 0, DIMMER]] - (0.4 + 0.2 * local)).abs() < 1e-9);
}

#[test]
fn a_negative_delay_starts_a_head_early() {
    let graph = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 4, "delay": -1}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    // One beat into the event the clock reads two beats of four.
    let light = play(&graph, &line(), &[1.]);
    assert!((light[[0, 0, DIMMER]] - 0.5).abs() < 1e-9);
}

#[test]
fn a_math_node_multiplies_adds_and_broadcasts() {
    let product = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0]}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "time1": {"kind": "time"},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "math1": {"kind": "math", "inputs": {"values": [{"node": "curve1"}, {"node": "curve2"}, 0.5]}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}}}));
    let light = play(&product, &line(), &[4., 12.]);
    for (t, progress) in [(0, 0.25), (1, 0.75)] {
        for n in 0..12 {
            let expected = n as f64 / 11. * progress * 0.5;
            assert!((light[[n, t, DIMMER]] - expected).abs() < 1e-9);
        }
    }
    // max, min, a difference, and a number times a color.
    let op = |op: &str, values: Value| {
        graph(json!({
            "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0]}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 0]]}}},
            "math1": {"kind": "math", "settings": {"op": op}, "inputs": {"values": values}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}}}))
    };
    let both = json!([{"node": "curve1"}, {"node": "curve2"}]);
    for (name, f) in [
        ("max", f64::max as fn(f64, f64) -> f64),
        ("min", f64::min),
        ("-", |a: f64, b: f64| (a - b).max(0.)),
        ("+", |a: f64, b: f64| a + b),
    ] {
        let light = play(&op(name, both.clone()), &line(), &[1.]);
        for n in 0..12 {
            let a = n as f64 / 11.;
            assert!(
                (light[[n, 0, DIMMER]] - f(a, 1. - a)).abs() < 1e-9,
                "{name} head {n}"
            );
        }
    }
    let tinted = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0]}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "curve2": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "space1"},
                   "gradient": {"stops": [{"t": 0, "color": [1, 0, 0]}, {"t": 1, "color": [1, 0, 0]}]}}},
        "math1": {"kind": "math", "inputs": {"values": [{"node": "curve1"}, {"node": "curve2"}]}},
        "color1": {"kind": "color", "inputs": {"color": {"node": "math1"}}}}));
    let light = play(&tinted, &line(), &[1.]);
    for n in 0..12 {
        let light_n = light[[n, 0, 0]] * light[[n, 0, DIMMER]];
        assert!((light_n - n as f64 / 11.).abs() < 1e-9 && light[[n, 0, 1]] == 0.);
    }
}

/// Red while an event is young, blue once it is old, both at full peak.
fn red_then_blue(clock: &str) -> Value {
    json!({"kind": "curve", "settings": {"kind": "color"},
           "inputs": {"x": {"node": clock}, "shape": {"points": [[0, 1, "hold"], [0.5, 0], [1, 0]]},
                      "gradient": {"stops": [{"t": 0, "color": [1, 0, 0]}, {"t": 1, "color": [0, 0, 1]}]}}})
}

/// The light per channel (color × dimmer) and alpha of head `n` at `t`.
fn rgb(light: &Array3<f64>, n: usize, t: usize) -> [f64; 3] {
    std::array::from_fn(|c| light[[n, t, c]] * light[[n, t, DIMMER]])
}

#[test]
fn overlapping_events_combine_per_channel() {
    // At beat 1.5 event 0 is 0.75 old and event 1 is 0.25 old.
    let brighter = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1, "duration": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&brighter, &line(), &[1.5]);
    assert!((light[[0, 0, DIMMER]] - 0.75).abs() < 1e-9);
    let both = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1, "duration": 2}},
        "curve1": red_then_blue("time1"),
        "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}}}));
    let light = play(&both, &line(), &[1.5]);
    // Event 0, 0.75 old, is red; event 1, 0.25 old, is blue. Each channel
    // takes its largest: red and blue together.
    let [r, g, b] = rgb(&light, 0, 0);
    assert!(r > 0.99 && b > 0.99 && g < 0.01, "{r} {g} {b}");
    // Two dim events of one color: the brighter shows, never their sum.
    let dim_pair = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1, "duration": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 0.3, "high": 0.3}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&dim_pair, &line(), &[1.5]);
    assert!((light[[0, 0, DIMMER]] - 0.3).abs() < 1e-9);
}

#[test]
fn gaps_between_events_are_transparent() {
    // Events of 0.5 beats every 2 beats: between them the clip is not
    // there (alpha 0), for color, strobe and aim; inside them, alpha 1.
    let short = || json!({"kind": "time", "inputs": {"every": 2, "duration": 0.5}});
    let lit_curve =
        json!({"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 1, "high": 1}});
    for output in [
        json!({"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}),
        json!({"kind": "strobe", "inputs": {"rate": {"node": "curve1"}}}),
        json!({"kind": "aim", "inputs": {"yaw": {"node": "curve1"}}}),
    ] {
        let kind = output["kind"].clone();
        let g = graph(json!({"time1": short(), "curve1": lit_curve.clone(), "out": output}));
        let light = play(&g, &line(), &[0.25, 1.25]);
        assert!(
            (light[[0, 0, 11]] - 1.).abs() < 1e-9,
            "{kind}: inside an event"
        );
        assert_eq!(light[[0, 1, 11]], 0., "{kind}: in the gap");
    }
    // Brightness 0 is black light, not a gap: alpha stays 1.
    let black = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 0, "high": 0}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&black, &line(), &[1.25]);
    assert_eq!((light[[0, 0, DIMMER]], light[[0, 0, 11]]), (0., 1.));
}

#[test]
fn overlapping_aim_events_lay_the_newest_on_top() {
    // Yaw follows each event's age; two events live at once. The newest,
    // at full alpha, covers the older; at half alpha it mixes half way.
    let yaw = |alpha: Value| {
        graph(json!({
            "time1": {"kind": "time", "inputs": {"every": 1, "duration": 2}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 0, "high": 40}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": alpha.clone(), "high": alpha}},
            "aim1": {"kind": "aim", "inputs": {"yaw": {"node": "curve1"}, "alpha": {"node": "curve2"}}}}))
    };
    let (_, turn) = run(&yaw(json!(1)), &line(), &[1.5]);
    let turn = turn.expect("an aim turn");
    // Event 1 is 0.25 old: 10°. Event 0 (30°) is under it.
    assert!((turn[[0, 0, 3]] - 10.).abs() < 1e-9, "{}", turn[[0, 0, 3]]);
    let (_, turn) = run(&yaw(json!(0.5)), &line(), &[1.5]);
    let turn = turn.unwrap();
    // Half of 10° over half of 30°: (5 + 0.25 × 30) / 0.75.
    assert!((turn[[0, 0, 3]] - (5. + 7.5) / 0.75).abs() < 1e-9);
}

#[test]
fn a_phase_always_wraps() {
    // A phase of 0 wraps as any other: a head delayed past its event's end
    // comes round again. Without a phase, τ runs on past 1.
    let delayed = |phase: Option<f64>| {
        let mut t = json!({"kind": "time", "inputs": {"every": 2, "delay": 1}});
        if let Some(phase) = phase {
            t["inputs"]["phase"] = json!(phase);
        }
        graph(json!({
            "time1": t,
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}))
    };
    // At beat 0.5, p = 0.25 and τ = 0.25 − 0.5 = −0.25.
    let unwrapped = play(&delayed(None), &line(), &[0.5]);
    assert_eq!(unwrapped[[0, 0, DIMMER]], 0.);
    for phase in [0., 1., -2.] {
        let wrapped = play(&delayed(Some(phase)), &line(), &[0.5]);
        assert!(
            (wrapped[[0, 0, DIMMER]] - 0.75).abs() < 1e-9,
            "phase {phase}"
        );
    }
}

#[test]
fn two_clocks_on_color_and_brightness_are_independent() {
    let graph = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1}},
        "time2": {"kind": "time", "inputs": {"every": 3}},
        "curve1": red_then_blue("time1"),
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "time2"}}},
        "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}, "brightness": {"node": "curve2"}}}}));
    let light = play(&graph, &line(), &[3.25, 4.75]);
    // Brightness follows clock 2's progress; color follows clock 1.
    assert!((light[[0, 0, DIMMER]] - 0.25 / 3.).abs() < 1e-9);
    assert!((light[[0, 1, DIMMER]] - 1.75 / 3.).abs() < 1e-9);
    assert!(light[[0, 0, 2]] > 0.99, "young clock 1 event: blue");
    assert!(light[[0, 1, 0]] > 0.99, "old clock 1 event: red");
}

#[test]
fn a_mirror_flips_yaw_for_folded_heads() {
    let scissor = &presets().clip("Scissor").unwrap().graph;
    let light = play(scissor, &line(), &[1.]);
    let (left, right) = (light[[0, 0, 8]], light[[11, 0, 8]]);
    assert!(right.abs() > 0.1);
    assert!((left + right).abs() < 1e-9, "{left} and {right}");
}

#[test]
fn kaleidoscope_quadrants_match() {
    let kaleidoscope = &presets().clip("Kaleidoscope").unwrap().graph;
    let times = beats(0.5);
    let light = play(kaleidoscope, &grid(), &times);
    let at = |i: usize, j: usize, t: usize| light[[i * 4 + j, t, DIMMER]];
    let mut lit_any = false;
    for t in 0..times.len() {
        for i in 0..4 {
            for j in 0..4 {
                let v = at(i, j, t);
                lit_any |= v > 0.;
                for (a, b) in [(3 - i, j), (i, 3 - j), (3 - i, 3 - j)] {
                    assert!((v - at(a, b, t)).abs() < 1e-9, "t {t}: g{i}{j} vs g{a}{b}");
                }
            }
        }
    }
    assert!(lit_any);
}

#[test]
fn turning_line_lights_two_opposite_arms() {
    let turning = &presets().clip("Turning line").unwrap().graph;
    let times = [2.3, 5.1, 9.7];
    let light = play(turning, &ring(), &times);
    for t in 0..times.len() {
        let on = lit(&light, t);
        assert!(on.len() >= 2, "beat {}: {on:?}", times[t]);
        for n in &on {
            assert!(on.contains(&((n + 6) % 12)), "beat {}: {on:?}", times[t]);
        }
    }
}

#[test]
fn noise_nodes_draw_their_own_streams_and_a_linked_node_is_shared() {
    let ballyhoo = &presets().clip("Ballyhoo").unwrap().graph;
    let times = beats(1.);
    let (_, turn) = run(ballyhoo, &bars(), &times);
    let turn = turn.unwrap();
    let differs =
        (0..12).any(|n| (0..times.len()).any(|t| (turn[[n, t, 3]] - turn[[n, t, 4]]).abs() > 1e-6));
    assert!(differs, "two noise nodes with the same settings differ");
    let linked = graph(json!({
        "noise1": {"kind": "noise", "inputs": {"speed": 4, "scale": 0.02}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "noise1"}, "low": -40, "high": 40}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "noise1"}, "low": -40, "high": 40}},
        "aim1": {"kind": "aim", "settings": {"base": "direction"},
                 "inputs": {"yaw": {"node": "curve1"}, "pitch": {"node": "curve2"}}}}));
    let (_, turn) = run(&linked, &bars(), &times);
    let turn = turn.unwrap();
    for n in 0..12 {
        for t in 0..times.len() {
            assert_eq!(turn[[n, t, 3]], turn[[n, t, 4]]);
        }
    }
}

#[test]
fn radial_runs_from_the_nearest_head_to_the_farthest() {
    // Distances 1, 2 and 3 from the centre on each side.
    let cells: Vec<Cell> = [-3., -2., -1., 1., 2., 3.]
        .into_iter()
        .enumerate()
        .map(|(n, u)| cell(format!("h{n}"), [u, 0., 3.]))
        .collect();
    let radial = graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "radial", "wrap": "no"}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let at: Vec<f64> = radial
        .coordinate_at_heads("space1", frame(&cells, 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    // Distance from the middle of the selection over the largest: 1/3,
    // 2/3, 1. The nearest heads are not at 0: no head sits at the centre.
    let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9);
    let third = 1. / 3.;
    assert!(
        close(&at, &[1., 2. * third, third, third, 2. * third, 1.]),
        "{at:?}"
    );
    // A centre off the middle: at the left end (u 0 of the box).
    let mut left = radial.clone();
    left.nodes.get_mut("space1").unwrap().inputs.insert(
        "at".into(),
        luma_patterns::clip_graph::Input::Vector([0., 0.5, 0.5]),
    );
    let at: Vec<f64> = left
        .coordinate_at_heads("space1", frame(&cells, 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    assert!(
        close(&at, &[0., 1. / 6., 2. / 6., 4. / 6., 5. / 6., 1.]),
        "{at:?}"
    );
    // Angle is measured around the centre too: the heads right of the
    // left-end centre all read one angle.
    let mut angle = left.clone();
    angle
        .nodes
        .get_mut("space1")
        .unwrap()
        .settings
        .insert("kind".into(), "angle".into());
    let turns: Vec<f64> = angle
        .coordinate_at_heads("space1", frame(&cells, 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    assert!(
        turns[1..].iter().all(|t| (t - turns[1]).abs() < 1e-9),
        "{turns:?}"
    );
}

#[test]
fn best_fit_snaps_to_a_stage_axis() {
    // A line tilted 30° off +U reads along +U; a square, which spreads the
    // same along U and Z, reads along +U too, never a diagonal.
    let tilted: Vec<Cell> = (0..5)
        .map(|i| {
            let r = i as f64;
            cell(format!("t{i}"), [r * 0.866, 0., r * 0.5])
        })
        .collect();
    let place = graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let read = |cells: &[Cell]| -> Vec<f64> {
        place
            .coordinate_at_heads("space1", frame(cells, 1.))
            .unwrap()
            .into_iter()
            .map(Option::unwrap)
            .collect()
    };
    let along_u = |cells: &[Cell]| -> Vec<f64> {
        let (lo, hi) = cells.iter().fold((f64::MAX, f64::MIN), |(a, b), c| {
            (a.min(c.uvz[0]), b.max(c.uvz[0]))
        });
        cells.iter().map(|c| (c.uvz[0] - lo) / (hi - lo)).collect()
    };
    for cells in [tilted, grid()] {
        let (got, want) = (read(&cells), along_u(&cells));
        assert!(
            got.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-9),
            "{got:?} vs {want:?}"
        );
    }
}

#[test]
fn a_wrapped_line_is_a_ring_whose_ends_do_not_meet() {
    // Wrapped, the heads sit one spacing apart all the way round, also
    // from the last head back to the first.
    let heads = line().len();
    let at: Vec<f64> = over_line(json!([[0, 0], [1, 1]]), "yes")
        .coordinate_at_heads("space1", frame(&line(), 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    let spacing = 1. / heads as f64;
    for n in 0..heads {
        let gap = (at[(n + 1) % heads] - at[n]).rem_euclid(1.);
        assert!((gap - spacing).abs() < 1e-9, "{at:?}");
    }
    // Not wrapped, the ends stay at 0 and 1.
    let at = over_line(json!([[0, 0], [1, 1]]), "no")
        .coordinate_at_heads("space1", frame(&line(), 1.))
        .unwrap();
    assert_eq!((at[0], at[heads - 1]), (Some(0.), Some(1.)));
}

#[test]
fn a_shift_slides_the_coordinate_and_a_scale_stretches_it() {
    let at = |inputs: Value, wrap: &str| -> Vec<f64> {
        graph(json!({
            "space1": {"kind": "space", "settings": {"kind": "line", "wrap": wrap}, "inputs": inputs},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}))
        .coordinate_at_heads("space1", frame(&line(), 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect()
    };
    let plain = at(json!({"direction": [1, 0, 0]}), "no");
    let slid = at(
        json!({"direction": [1, 0, 0], "shift": 0.25, "scale": 0.5}),
        "no",
    );
    for (a, x) in plain.iter().zip(&slid) {
        assert!((x - (a - 0.25) / 0.5).abs() < 1e-9, "{plain:?} {slid:?}");
    }
    // On a ring the space tiles, as a shader's fract(p / scale): it is
    // stretched first, then repeats every `scale`.
    let ring = at(json!({"direction": [1, 0, 0]}), "yes");
    for scale in [2., 0.25] {
        let tiled = at(
            json!({"direction": [1, 0, 0], "shift": 0.9, "scale": scale}),
            "yes",
        );
        for (a, x) in ring.iter().zip(&tiled) {
            assert!((x - ((a - 0.9) / scale).rem_euclid(1.)).abs() < 1e-9);
        }
    }
}

fn frame(cells: &[Cell], beat: f64) -> Frame<'_> {
    Frame {
        features: None,
        cells,
        beat,
        clip_start: 0.,
        clip_duration: 16.,
        seed: 7,
    }
}

#[test]
fn a_space_node_gives_each_head_its_place() {
    let cells = line();
    let field = over_line(json!([[0, 0], [1, 1]]), "no");
    let at: Vec<f64> = field
        .coordinate_at_heads("space1", frame(&cells, 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]));
    assert!(at.iter().all(|x| (0. ..=1.).contains(x)));
    // Cell order is the caller's, not the id order.
    let reversed: Vec<Cell> = cells.iter().rev().cloned().collect();
    let back: Vec<f64> = field
        .coordinate_at_heads("space1", frame(&reversed, 1.))
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    assert_eq!(back.iter().rev().copied().collect::<Vec<_>>(), at);
}

#[test]
fn sample_noise_matches_the_noise_playback_reads() {
    let cells = line();
    let graph = graph(json!({
        "noise1": {"kind": "noise", "inputs": {"speed": 4, "scale": 0.3, "contrast": 0.2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "noise1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    for beat in [0., 2.5, 9.] {
        let at = graph
            .coordinate_at_heads("noise1", frame(&cells, beat))
            .unwrap();
        for (n, value) in at.iter().enumerate() {
            // The line spans 11 m along U: head n sits at n / 11.
            let place = [n as f64 / 11., 0., 0.];
            let sampled = luma_patterns::clip_graph::sample_noise(
                "noise1",
                7,
                place,
                beat,
                4.,
                Some(0.3),
                0.2,
            )
            .unwrap();
            assert!(
                (value.unwrap() - sampled).abs() < 1e-9,
                "head {n} at beat {beat}"
            );
        }
    }
}

// ---- key presets ----

fn preset(name: &str) -> ClipGraph {
    presets()
        .clip(name)
        .unwrap_or_else(|| panic!("{name}"))
        .graph
        .clone()
}

/// The mean place of the lit heads of `line()`, or `None` when all are dark.
fn centre(light: &Array3<f64>, t: usize) -> Option<f64> {
    let on = lit(light, t);
    (!on.is_empty()).then(|| on.iter().sum::<usize>() as f64 / on.len() as f64)
}

#[test]
fn chase_moves_a_short_pill_along_the_line_once_per_event() {
    // Event 1 of every 2 beats: beats 2–4.
    let times: Vec<f64> = (1..20).map(|i| 2. + i as f64 * 0.1).collect();
    let light = play(&preset("Chase"), &line(), &times);
    let mut last = f64::NEG_INFINITY;
    for (t, beat) in times.iter().enumerate() {
        let on = lit(&light, t);
        assert!((1..=3).contains(&on.len()), "beat {beat}: {on:?}");
        assert!(on.windows(2).all(|w| w[1] == w[0] + 1), "one piece: {on:?}");
        let at = centre(&light, t).unwrap();
        assert!(at >= last, "the pill only moves forward");
        last = at;
    }
    assert!(centre(&light, 0).unwrap() < 2. && last > 9.);
}

#[test]
fn bounce_moves_a_pill_out_and_back_in_one_event() {
    // Event 1 of every 4 beats: beats 4–8; the shift peaks at the middle.
    let times: Vec<f64> = (1..40).map(|i| 4. + i as f64 * 0.1).collect();
    let light = play(&preset("Bounce"), &line(), &times);
    let centres: Vec<f64> = (0..times.len()).filter_map(|t| centre(&light, t)).collect();
    assert_eq!(centres.len(), times.len(), "the pill is always on the rig");
    let peak = centres
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0;
    assert!(
        centres[..=peak].windows(2).all(|w| w[1] >= w[0]),
        "{centres:?}"
    );
    assert!(
        centres[peak..].windows(2).all(|w| w[1] <= w[0]),
        "{centres:?}"
    );
    assert!(centres[peak] > centres[0] + 5. && centres[peak] > *centres.last().unwrap() + 5.);
}

#[test]
fn vu_meter_lights_each_bar_from_the_bottom_up_to_the_level() {
    let times = beats(0.25);
    let light = play(&preset("VU meter"), &bars(), &times);
    let mut counts = std::collections::BTreeSet::new();
    for t in 0..times.len() {
        let on = lit(&light, t);
        // bars() lists each bar's heads bottom to top, three per bar.
        let per_bar: Vec<Vec<usize>> = (0..4)
            .map(|bar| on.iter().filter(|n| *n / 3 == bar).map(|n| n % 3).collect())
            .collect();
        for heads in &per_bar {
            assert_eq!(
                heads,
                &(0..heads.len()).collect::<Vec<_>>(),
                "from the bottom"
            );
            assert_eq!(heads.len(), per_bar[0].len(), "one level for every bar");
        }
        counts.insert(per_bar[0].len());
    }
    assert!(counts.len() >= 2, "the level moves: {counts:?}");
}

#[test]
fn wipe_lights_a_growing_prefix_of_the_line() {
    let times: Vec<f64> = (0..40).map(|i| 4. + i as f64 * 0.1).collect();
    let light = play(&preset("Wipe"), &line(), &times);
    let mut count = 0;
    for t in 0..times.len() {
        let on = lit(&light, t);
        assert_eq!(on, (0..on.len()).collect::<Vec<_>>(), "a prefix");
        assert!(on.len() >= count);
        count = on.len();
    }
    assert!(count >= 11, "almost all by the end: {count}");
}

#[test]
fn dissolve_turns_heads_off_one_by_one_in_a_random_order() {
    let times = beats(0.25);
    let light = play(&preset("Dissolve"), &bars(), &times);
    let counts: Vec<usize> = (0..times.len()).map(|t| lit(&light, t).len()).collect();
    assert_eq!(counts[0], 12);
    assert!(counts.windows(2).all(|w| w[1] <= w[0]), "{counts:?}");
    assert_eq!(*counts.last().unwrap(), 0);
    let off = |n: usize| (0..times.len()).find(|t| light[[n, *t, DIMMER]] <= 1e-6);
    let order: Vec<Option<usize>> = (0..12).map(off).collect();
    assert!(
        !order.windows(2).all(|w| w[0] <= w[1]),
        "not in id order: {order:?}"
    );
}

#[test]
fn gradient_colors_the_rig_along_its_axis() {
    let light = play(&preset("Gradient"), &line(), &[1.]);
    let rgb = |n: usize| [light[[n, 0, 0]], light[[n, 0, 1]], light[[n, 0, 2]]];
    assert!((0..12).all(|n| rgb(n).iter().any(|c| *c > 0.)));
    let distance = |a: [f64; 3], b: [f64; 3]| (0..3).map(|c| (a[c] - b[c]).abs()).sum::<f64>();
    assert!(distance(rgb(0), rgb(11)) > distance(rgb(0), rgb(1)));
}

#[test]
fn sparkle_lights_a_changing_few() {
    let times: Vec<f64> = (0..32).map(|i| 1. + i as f64 * 0.0625).collect();
    let light = play(&preset("Sparkle"), &bars(), &times);
    let sets: Vec<Vec<usize>> = (0..times.len()).map(|t| lit(&light, t)).collect();
    assert!(sets.iter().all(|set| set.len() < 12));
    assert!(sets.iter().any(|set| !set.is_empty()));
    assert!(sets.windows(2).any(|w| w[0] != w[1]));
}

#[test]
fn nod_wave_runs_one_nod_shifted_along_the_line() {
    // Each head nods on its own clock, 0.6 of a turn behind across the
    // line: head n at beat b reads what head 0 reads 4 × 0.6 × n / 11
    // beats later.
    let times: Vec<f64> = (0..32).map(|i| 4. + i as f64 * 0.25).collect();
    let (_, turn) = run(&preset("Nod wave"), &line(), &times);
    let turn = turn.unwrap();
    const PITCH: usize = 4;
    let pitch = |n: usize, t: usize| turn[[n, t, PITCH]];
    assert!((0..times.len()).any(|t| (pitch(0, t) - pitch(11, t)).abs() > 1.));
    // Head 11's shift, 2.4 beats, is not on the sample grid; head 0 at
    // beat b + 2.4 wraps to the next event: compare with a second run.
    let later: Vec<f64> = times.iter().map(|b| b + 4. * 0.6 * 11. / 11.).collect();
    let (_, shifted) = run(&preset("Nod wave"), &line(), &later);
    let shifted = shifted.unwrap();
    for t in 0..times.len() {
        assert!(
            (pitch(11, t) - shifted[[0, t, PITCH]]).abs() < 1e-6,
            "sample {t}"
        );
    }
}

/// A 10 × 10 grid in the U–Z plane, one metre apart; head `g{u}{z}`.
fn fine_grid() -> Vec<Cell> {
    (0..10)
        .flat_map(|u| (0..10).map(move |z| cell(format!("g{u}{z}"), [u as f64, 0., z as f64])))
        .collect()
}

/// Slash, every 2 beats (spec section 0): a cut whose front sweeps across
/// the slash direction over the first 0.2 of the event (x = front − place,
/// read against −direction, under a rising step: a head is lit once the
/// front has reached it, the far corner too), a bloom that grows
/// out from a line at 68 % of the rig (a mirror there, and a space along
/// its normal that measures from it, scaled from 4 % to 74 %), a fade for
/// all, and a white-to-red color; cut × bloom × fade is one math node.
fn slash() -> ClipGraph {
    graph(json!({
        "t": {"kind": "time", "inputs": {"every": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "t"}, "shape": {"points": [[0, 1], [0.2, 0], [1, 0]]}}},
        "diag": {"kind": "space", "inputs": {"direction": [0.82, 0, -0.57], "shift": {"node": "curve1"}}},
        "cut": {"kind": "curve", "inputs": {"x": {"node": "diag"}, "shape": {"points": [[0, 0], [0, 1], [1, 1]]}}},
        "line": {"kind": "mirror", "inputs": {"normal": [0.57, 0, 0.82], "at": 0.68}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "t"}, "low": 0.04, "high": 0.74}},
        "dist": {"kind": "space", "inputs": {"heads": {"node": "line"}, "direction": [0.57, 0, 0.82], "scale": {"node": "curve2"}}},
        "bloom": {"kind": "curve", "inputs": {"x": {"node": "dist"}, "shape": {"points": [[0, 1], [0.76, 1], [1, 0]]}}},
        "fade": {"kind": "curve", "inputs": {"x": {"node": "t"}, "shape": {"points": [[0, 1, "hold"], [0.2, 1, "sine-out"], [1, 0]]}}},
        "heat": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "t"}, "gradient": {"stops": [{"t": 0, "color": [1, 1, 1]}, {"t": 0.2, "color": [1, 1, 1]}, {"t": 0.5, "color": [1, 0, 0.01]}, {"t": 1, "color": [1, 0, 0.01]}]}}},
        "math1": {"kind": "math", "inputs": {"values": [{"node": "cut"}, {"node": "bloom"}, {"node": "fade"}]}},
        "color1": {"kind": "color", "inputs": {"color": {"node": "heat"}, "brightness": {"node": "math1"}}}}))
}

#[test]
fn slash_cuts_across_blooms_outward_and_fades() {
    let cells = fine_grid();
    let times: Vec<f64> = (0..40).map(|i| i as f64 * 0.05).collect();
    let light = play(&slash(), &cells, &times);
    let counts: Vec<usize> = (0..times.len()).map(|t| lit(&light, t).len()).collect();
    // The bloom spreads while the cut is still sweeping: more heads light
    // over the first half, and all are near dark as the event ends.
    assert!(
        counts[2] < counts[8] && counts[8] < counts[20],
        "{counts:?}"
    );
    let last = times.len() - 1;
    assert!((0..cells.len()).all(|n| light[[n, last, DIMMER]] < 0.05));
    // Farther from the slash line a head starts later and takes longer to
    // rise. Head g55 sits near the line, g00 far from it.
    let head = |id: &str| cells.iter().position(|c| c.id == id).unwrap();
    let rise = |n: usize| {
        let start = (0..times.len()).find(|t| light[[n, *t, DIMMER]] > 0.02)?;
        let peak = (0..times.len())
            .max_by(|a, b| light[[n, *a, DIMMER]].total_cmp(&light[[n, *b, DIMMER]]))?;
        Some((start, peak - start))
    };
    let (near_start, near_rise) = rise(head("g55")).unwrap();
    let (far_start, far_rise) = rise(head("g90")).unwrap();
    assert!(near_start < far_start, "{near_start} then {far_start}");
    assert!(near_rise < far_rise, "{near_rise} then {far_rise}");
}

/// Checks `light` against a hand-computed lit set: head `n` (of
/// `fine_grid`) at sample `t` is lit when `margin(n, t)` is above 0 and dark
/// when it is below 0. Heads within `edge` of the boundary are skipped.
fn matches_by_hand(
    light: &Array3<f64>,
    times: &[f64],
    edge: f64,
    margin: impl Fn(usize, usize) -> f64,
) {
    let mut checked = 0;
    for t in 0..times.len() {
        for n in 0..light.dim().0 {
            let m = margin(n, t);
            if m.abs() < edge {
                continue;
            }
            assert_eq!(
                dim(light, n, t) > 1e-6,
                m > 0.,
                "head {n} at beat {}",
                times[t]
            );
            checked += 1;
        }
    }
    assert!(
        checked > light.dim().0 * times.len() / 2,
        "checked {checked}"
    );
}

#[test]
fn a_chase_along_plus_u_moves_toward_stage_right() {
    // shift −0.2 → 1 over each 2-beat event, scale 0.2, hard pill: head
    // g{u}{z} is lit while u/9 − shift is in [0, 0.2].
    let cells = fine_grid();
    let mut graph = preset("Chase");
    graph.nodes.get_mut("place").unwrap().inputs.insert(
        "direction".into(),
        luma_patterns::clip_graph::Input::Vector([1., 0., 0.]),
    );
    let times: Vec<f64> = (0..40).map(|i| 2. + i as f64 * 0.05).collect();
    let light = play(&graph, &cells, &times);
    matches_by_hand(&light, &times, 1e-6, |n, t| {
        let u = cells[n].uvz[0] / 9.;
        let shift = -0.2 + 1.2 * (times[t] - 2.) / 2.;
        let x = (u - shift) / 0.2;
        x.min(1. - x)
    });
    // The pill's mean U rises over the event.
    let mean_u = |t: usize| {
        let on = lit(&light, t);
        on.iter().map(|n| cells[*n].uvz[0]).sum::<f64>() / on.len() as f64
    };
    assert!(mean_u(2) < mean_u(20) && mean_u(20) < mean_u(38));
}

/// [`slash`] with brightness from `node` alone; nodes nothing reads go.
fn slash_brightness(node: &str) -> ClipGraph {
    let mut graph = slash();
    graph.nodes.get_mut("color1").unwrap().inputs.insert(
        "brightness".into(),
        luma_patterns::clip_graph::Input::wire(node),
    );
    loop {
        let read: std::collections::BTreeSet<String> = graph
            .nodes
            .values()
            .flat_map(|n| n.wires().map(|(_, s)| s.to_string()).collect::<Vec<_>>())
            .collect();
        let dead: Vec<String> = graph
            .nodes
            .keys()
            .filter(|id| *id != "color1" && !read.contains(*id))
            .cloned()
            .collect();
        if dead.is_empty() {
            return graph;
        }
        for id in dead {
            graph.nodes.remove(&id);
        }
    }
}

#[test]
fn the_slash_cut_sweeps_along_its_direction_and_the_bloom_widens_from_its_line() {
    let cells = fine_grid();
    let times: Vec<f64> = (0..40).map(|i| i as f64 * 0.05).collect();
    let unit = |v: [f64; 3]| {
        let l = v.iter().map(|c| c * c).sum::<f64>().sqrt();
        v.map(|c| c / l)
    };
    let along = |d: [f64; 3]| -> Vec<f64> {
        cells
            .iter()
            .map(|c| (0..3).map(|a| c.uvz[a] * d[a]).sum())
            .collect()
    };
    // Cut alone: the coordinate along (−0.82, 0, 0.57), 0 at the lowest
    // head and 1 at the highest; lit up to the front, which runs 0 → 1 over
    // the first 0.2 of the event. The front starts at +U, low Z.
    let light = play(&slash_brightness("cut"), &cells, &times);
    let d = along(unit([-0.82, 0., 0.57]));
    let (lo, hi) = d
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    matches_by_hand(&light, &times, 1e-6, |n, t| {
        let shift = (times[t] / 2. / 0.2).min(1.);
        shift - (d[n] - lo) / (hi - lo)
    });
    let head = |id: &str| cells.iter().position(|c| c.id == id).unwrap();
    let first_on = |n: usize| (0..times.len()).find(|t| dim(&light, n, *t) > 1e-6);
    assert!(first_on(head("g90")) < first_on(head("g55")));
    assert!(first_on(head("g55")).unwrap() < first_on(head("g18")).unwrap());
    // The cut ends at 0.4 beats (sample 8) with its front at 1, exactly on
    // the far corner g09: x = front − place is 0 there and the rising step
    // reads its value after the jump, so every head is lit.
    assert_eq!(times[8], 0.4);
    assert_eq!(lit(&light, 8), (0..cells.len()).collect::<Vec<_>>());
    assert_eq!(first_on(head("g09")), Some(8));
    // Bloom alone: lit within scale × extent of the line at 0.68 along
    // (0.57, 0, 0.82); scale runs 0.04 → 0.74 over the event.
    let light = play(&slash_brightness("bloom"), &cells, &times);
    let b = along(unit([0.57, 0., 0.82]));
    let (lo, hi) = b
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, c), v| (a.min(*v), c.max(*v)));
    let line = lo + 0.68 * (hi - lo);
    matches_by_hand(&light, &times, 1e-6, |n, t| {
        let scale = 0.04 + 0.7 * times[t] / 2.;
        scale - (b[n] - line).abs() / (hi - lo)
    });
}

// ---- version 2 → 3: every preset plays as it did ----

/// Light per head and time as version 2 played it: color × dimmer × alpha
/// for color, strobe × alpha for strobe, and aim direction plus weight.
fn v3_values(graph: &ClipGraph, cells: &[Cell], beats: &[f64]) -> Vec<Vec<Vec<f64>>> {
    let light = play(graph, cells, beats);
    let kind = graph.output_kind().unwrap();
    // `play` lists heads by id; the fixture lists them in rig order.
    let mut order: Vec<usize> = (0..cells.len()).collect();
    order.sort_by(|a, b| cells[*a].id.cmp(&cells[*b].id));
    let mut row_of = vec![0; cells.len()];
    for (row, n) in order.into_iter().enumerate() {
        row_of[n] = row;
    }
    (0..beats.len())
        .map(|t| {
            (0..cells.len())
                .map(|n| {
                    let v = |ch: usize| light[[row_of[n], t.min(light.dim().1 - 1), ch]];
                    match kind {
                        luma_patterns::clip_graph::Kind::Aim => vec![v(8), v(9), v(10), v(11)],
                        luma_patterns::clip_graph::Kind::Strobe => vec![v(STROBE) * v(11)],
                        _ => (0..3).map(|c| v(c) * v(DIMMER) * v(11)).collect(),
                    }
                })
                .collect()
        })
        .collect()
}

/// The version 2 frames, captured by `fixtures/capture_v2.py` at 691cd753.
fn v2_frames() -> Value {
    serde_json::from_str(include_str!("fixtures/v2_frames.json")).unwrap()
}

/// Largest and mean absolute difference between two runs.
fn difference(old: &Value, new: &[Vec<Vec<f64>>]) -> (f64, f64) {
    let (mut most, mut sum, mut count) = (0_f64, 0., 0.);
    for (t, frame) in new.iter().enumerate() {
        for (n, head) in frame.iter().enumerate() {
            for (c, value) in head.iter().enumerate() {
                let d = (value - old[t][n][c].as_f64().unwrap()).abs();
                most = most.max(d);
                sum += d;
                count += 1.;
            }
        }
    }
    (most, sum / count)
}

#[test]
fn every_preset_plays_as_version_2_did_on_a_line() {
    // On a straight line along U the approved changes since version 2 do
    // not move a head: its best fit is +U either way, and its middle head
    // sits on the centre, so radial reads as before. The other rigs differ
    // by those changes (best fit on a stage axis, radial from the middle of
    // the box over the largest distance); their own tests cover them.
    let old = v2_frames();
    let beats: Vec<f64> = serde_json::from_value(old["beats"].clone()).unwrap();
    // Rewritten so a front lights the last head (a jump reads the value
    // after it): Stepped chase, Wipe and Grow. Kaleidoscope's angle is now
    // around the middle of its folded heads, not their centroid.
    // Alternating sides: the middle head, exactly on the phase jump, now
    // goes with the right side.
    let skip = [
        "Slash",
        "Stepped chase",
        "Wipe",
        "Grow",
        "Kaleidoscope",
        "Alternating sides",
    ];
    // A head or a sample beat exactly on a brightness jump (a pill's far
    // end, the middle rank of an odd count, a step at the sample beat): it
    // reads the value after the jump, so these only lose light there.
    let ties = [
        "Random heads",
        "Random bars",
        "Sparkle",
        "Dissolve",
        "Build",
        "Chase",
        "Diagonal slash",
        "Many pills",
        "Mirror",
    ];
    let mut wrong = Vec::new();
    for (name, rigs) in old["clips"].as_object().unwrap() {
        if skip.contains(&name.as_str()) {
            continue;
        }
        let frames = &rigs["line13"];
        let cells: Vec<Cell> = serde_json::from_value(old["rigs"]["line13"].clone()).unwrap();
        let new = v3_values(&preset(name), &cells, &beats);
        let (most, mean) = difference(frames, &new);
        if ties.contains(&name.as_str()) {
            let gained = brighter(frames, &new);
            if gained > 1e-6 {
                wrong.push(format!("{name} gains {gained}"));
            }
        } else if most
            > if name == "Wrapping ripple" {
                1e-3
            } else {
                1e-6
            }
        {
            // Version 2's Wrapping ripple ran each head's clock for 0.7143
            // turns, 1 / 1.4 rounded; now it is Ripple on a wrapped space.
            wrong.push(format!("{name}: {most} (mean {mean})"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The most any value of `new` is above the same value of `old`.
fn brighter(old: &Value, new: &[Vec<Vec<f64>>]) -> f64 {
    let mut most = 0_f64;
    for (t, frame) in new.iter().enumerate() {
        for (n, head) in frame.iter().enumerate() {
            for (c, value) in head.iter().enumerate() {
                most = most.max(value - old[t][n][c].as_f64().unwrap());
            }
        }
    }
    most
}

/// Slash was a cut, a bloom and a fade on three per-head clocks (delay and
/// length in turns); it is now a shifted cut and a bloom that is a space
/// scaled out from a mirror plane. It plays close to the old one, not
/// exactly: a head's bloom starts within a tenth of the event of where it
/// did.
#[test]
fn slash_plays_close_to_its_version_2_form() {
    let old = v2_frames();
    let beats: Vec<f64> = serde_json::from_value(old["beats"].clone()).unwrap();
    for (rig, frames) in old["clips"]["Slash"].as_object().unwrap() {
        let cells: Vec<Cell> = serde_json::from_value(old["rigs"][rig].clone()).unwrap();
        let (most, mean) = difference(frames, &v3_values(&slash(), &cells, &beats));
        println!("Slash on {rig}: largest {most:.3}, mean {mean:.4}");
        assert!(mean < 0.05, "Slash on {rig}: mean {mean}, largest {most}");
    }
}

// ---- foundation checks (spec section 0) ----

/// Brightness of head `n` (in `play`'s id order) at sample `t`.
fn dim(light: &Array3<f64>, n: usize, t: usize) -> f64 {
    light[[n, t.min(light.dim().1 - 1), DIMMER]]
}

#[test]
fn presets_over_the_clip_stretch_with_it() {
    // Build, Dissolve and Grow run once over the clip from time(): on a
    // 16-beat and a 32-beat clip they show the same picture at the same
    // share of the clip.
    let fractions: Vec<f64> = (0..=64).map(|i| i as f64 / 64.).collect();
    for name in ["Build", "Dissolve", "Grow"] {
        let cells = if name == "Grow" { grid() } else { bars() };
        let at = |length: f64| {
            let beats: Vec<f64> = fractions.iter().map(|f| f * length).collect();
            run_for(&preset(name), &cells, &beats, length).0
        };
        let (short, long) = (at(16.), at(32.));
        assert_eq!(short, long, "{name}");
        // Half way, some heads are lit and some are not.
        let half = lit(&short, 32).len();
        assert!(half > 0 && half < cells.len(), "{name}: {half} lit at half");
    }
}

#[test]
fn dissolve_keeps_each_light_on_until_progress_passes_its_number() {
    // Each light's number is its shuffled rank; it is on until the clip's
    // progress passes that number, then off.
    let times: Vec<f64> = (0..64).map(|i| i as f64 * 0.25).collect();
    let cells = bars();
    let light = play(&preset("Dissolve"), &cells, &times);
    let mut offs = Vec::new();
    for n in 0..cells.len() {
        let off = (0..times.len())
            .find(|t| dim(&light, n, *t) < 1e-9)
            .unwrap_or_else(|| panic!("head {n} never goes off"));
        assert!(off > 0, "head {n} is on at the start");
        for t in 0..times.len() {
            let expected = if t < off { 1. } else { 0. };
            assert_eq!(dim(&light, n, t), expected, "head {n} at beat {}", times[t]);
        }
        offs.push(off);
    }
    let mut distinct = offs.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        offs.len(),
        "every light has its own off time: {offs:?}"
    );
}

/// A pill chase along U over `heads`: shift −0.1 → 0.5 in 2 beats, scale
/// 0.1, a hard-edged pill.
fn chase_over(heads: Value, nodes: Value) -> ClipGraph {
    chase_with(heads, nodes, json!([[0, 0], [0, 1], [1, 1], [1, 0]]))
}

/// [`chase_over`] with the pill's `shape`.
fn chase_with(heads: Value, nodes: Value, shape: Value) -> ClipGraph {
    let mut all = json!({
        "t": {"kind": "time", "inputs": {"every": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "t"}, "low": -0.1, "high": 0.5}},
        "place": {"kind": "space", "inputs": {"heads": heads, "direction": [1, 0, 0],
                                              "shift": {"node": "curve1"}, "scale": 0.1}},
        "pill": {"kind": "curve", "inputs": {"x": {"node": "place"}, "shape": {"points": shape}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "pill"}}}});
    for (id, node) in nodes.as_object().unwrap() {
        all[id] = node.clone();
    }
    graph(all)
}

/// Heads that are images of each other: every head's partners, by the
/// position map `images`, among `cells` (id order, as `play` lists them).
fn partners(cells: &[Cell], images: impl Fn([f64; 3]) -> Vec<[f64; 3]>) -> Vec<Vec<usize>> {
    let mut sorted = cells.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let find = |p: [f64; 3]| {
        sorted
            .iter()
            .position(|c| (0..3).all(|a| (c.uvz[a] - p[a]).abs() < 1e-6))
            .unwrap_or_else(|| panic!("no head at {p:?}"))
    };
    sorted
        .iter()
        .map(|c| images(c.uvz).into_iter().map(find).collect())
        .collect()
}

#[test]
fn two_stacked_mirrors_make_four_chases_in_sync() {
    let cells = fine_grid();
    let graph = chase_over(
        json!({"node": "quarters"}),
        json!({
            "sides": {"kind": "mirror", "inputs": {"normal": [1, 0, 0]}},
            "quarters": {"kind": "mirror", "inputs": {"heads": {"node": "sides"}, "normal": [0, 0, 1]}}}),
    );
    let times: Vec<f64> = (0..40).map(|i| i as f64 * 0.05).collect();
    let light = play(&graph, &cells, &times);
    let images = partners(&cells, |[u, v, z]| {
        vec![[9. - u, v, z], [u, v, 9. - z], [9. - u, v, 9. - z]]
    });
    let mut lit_sets = Vec::new();
    for t in 0..times.len() {
        for (n, others) in images.iter().enumerate() {
            for m in others {
                assert_eq!(dim(&light, n, t), dim(&light, *m, t), "beat {}", times[t]);
            }
        }
        lit_sets.push(lit(&light, t));
    }
    // A pill moves out from the centre line in each half: the lit set
    // changes and is never the whole rig.
    assert!(lit_sets.iter().any(|set| !set.is_empty()));
    assert!(lit_sets.iter().all(|set| set.len() < cells.len()));
    assert!(lit_sets.windows(2).any(|w| w[0] != w[1]));
}

#[test]
fn three_mirrors_through_the_centre_make_six_fold_symmetry() {
    // A rig made of the six images of a few seed heads under the three
    // mirror lines at 0°, 60° and 120° through (0, 5) on the U–Z wall.
    let images = |r: f64, a: f64| -> Vec<[f64; 3]> {
        [a, a + 120., a + 240., -a, 120. - a, 240. - a]
            .iter()
            .map(|d: &f64| {
                let d = d.to_radians();
                [r * d.cos(), 0., 5. + r * d.sin()]
            })
            .collect()
    };
    let seeds = [(1., 70.), (2., 85.), (3., 100.), (2.5, 65.), (1.5, 110.)];
    let cells: Vec<Cell> = seeds
        .iter()
        .flat_map(|(r, a)| images(*r, *a))
        .enumerate()
        .map(|(n, uvz)| cell(format!("k{n:02}"), uvz))
        .collect();
    let s = 3_f64.sqrt() / 2.;
    // A soft pill: images of one head fold to the same place up to
    // rounding, which a hard edge would turn into on and off.
    let graph = chase_with(
        json!({"node": "third"}),
        json!({
            "first": {"kind": "mirror", "inputs": {"normal": [0, 0, 1]}},
            "second": {"kind": "mirror", "inputs": {"heads": {"node": "first"}, "normal": [-s, 0, 0.5]}},
            "third": {"kind": "mirror", "inputs": {"heads": {"node": "second"}, "normal": [s, 0, 0.5]}}}),
        json!([[0, 0, [0.4, 0, 0.6, 1]], [0.5, 1, [0.4, 0, 0.6, 1]], [1, 0]]),
    );
    let times: Vec<f64> = (0..40).map(|i| i as f64 * 0.05).collect();
    let light = play(&graph, &cells, &times);
    // Each head's five partners: the other images of its seed.
    let partner = partners(&cells, |p| {
        let (r, a) = (p[0].hypot(p[2] - 5.), (p[2] - 5.).atan2(p[0]).to_degrees());
        images(r, a)
    });
    let mut moved = false;
    for t in 0..times.len() {
        for (n, others) in partner.iter().enumerate() {
            assert_eq!(others.len(), 6);
            for m in others {
                assert!(
                    (dim(&light, n, t) - dim(&light, *m, t)).abs() < 1e-9,
                    "beat {}: head {n} vs {m}",
                    times[t]
                );
            }
        }
        moved |= t > 0 && lit(&light, t) != lit(&light, t - 1);
    }
    assert!(moved, "the chase moves");
}

/// A disc of `rings` rings around (0, 0) on the U–Z wall, ring `i` of
/// radius `i` holding 6·i heads at half-steps off 0°: the set maps onto
/// itself under the three mirror lines at 0°, 60° and 120°. Ids sort in
/// rig order; returns the cells and each head's (ring, step).
fn disc(rings: usize) -> (Vec<Cell>, Vec<(usize, usize)>) {
    let mut cells = Vec::new();
    let mut place = Vec::new();
    for i in 1..=rings {
        let count = 6 * i;
        for k in 0..count {
            let angle = std::f64::consts::TAU * (k as f64 + 0.5) / count as f64;
            let r = i as f64;
            cells.push(cell(
                format!("d{i:02}{k:03}"),
                [r * angle.cos(), 0., r * angle.sin()],
            ));
            place.push((i, k));
        }
    }
    (cells, place)
}

#[test]
fn the_kaleidoscope_example_repeats_one_slice_six_times_on_a_disc() {
    // Three mirrors through the centre with normals (0, 0, 1),
    // (−0.866, 0, 0.5), (0.866, 0, 0.5), stacked: every head folds into the
    // 60°–120° slice. Each slice is a mirror image of its neighbour, so a
    // head at θ matches θ + 120°, θ + 240° and the mirror angles −θ,
    // 120° − θ, 240° − θ; θ + 60° is a mirror image, not a copy.
    let (cells, place) = disc(8);
    let s = 3_f64.sqrt() / 2.;
    let graph = chase_with(
        json!({"node": "third"}),
        json!({
            "first": {"kind": "mirror", "inputs": {"normal": [0, 0, 1]}},
            "second": {"kind": "mirror", "inputs": {"heads": {"node": "first"}, "normal": [-s, 0, 0.5]}},
            "third": {"kind": "mirror", "inputs": {"heads": {"node": "second"}, "normal": [s, 0, 0.5]}}}),
        json!([[0, 0, [0.4, 0, 0.6, 1]], [0.5, 1, [0.4, 0, 0.6, 1]], [1, 0]]),
    );
    let times: Vec<f64> = (0..40).map(|i| i as f64 * 0.05).collect();
    let light = play(&graph, &cells, &times);
    let index = |ring: usize, k: i64| {
        let count = 6 * ring as i64;
        let first = 3 * (ring - 1) * ring; // heads on the inner rings
        first + k.rem_euclid(count) as usize
    };
    let mut sixty_differs = false;
    let mut moved = false;
    for t in 0..times.len() {
        for (n, (ring, k)) in place.iter().enumerate() {
            let (count, k) = (6 * *ring as i64, *k as i64);
            // Steps are half-offset: angle −θ is step count − 1 − k.
            let third = count / 3;
            let images = [
                k + third,
                k + 2 * third,
                count - 1 - k,
                third + count - 1 - k,
                2 * third + count - 1 - k,
            ];
            for m in images {
                let m = index(*ring, m);
                assert!(
                    (dim(&light, n, t) - dim(&light, m, t)).abs() < 1e-9,
                    "beat {}: {} vs {}",
                    times[t],
                    cells[n].id,
                    cells[m].id
                );
            }
            let m = index(*ring, k + count / 6);
            sixty_differs |= (dim(&light, n, t) - dim(&light, m, t)).abs() > 0.1;
        }
        moved |= t > 0 && lit(&light, t) != lit(&light, t - 1);
    }
    assert!(moved, "the chase moves");
    assert!(sixty_differs, "θ + 60° is a mirror image, not a copy");
}

/// A pill 10 % of the ring long on a wrapped space along U, its shift
/// running 0 → 1 over each event of `every`/`duration`.
fn wrapping(every: f64, duration: f64) -> ClipGraph {
    wrapping_with(
        every,
        duration,
        json!([[0, 0], [0, 1], [0.1, 1], [0.1, 0], [1, 0]]),
    )
}

/// [`wrapping`] with the pill's `shape`.
fn wrapping_with(every: f64, duration: f64, shape: Value) -> ClipGraph {
    graph(json!({
        "t": {"kind": "time", "inputs": {"every": every, "duration": duration}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "t"}}},
        "ring": {"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
                 "inputs": {"direction": [1, 0, 0], "shift": {"node": "curve1"}}},
        "pill": {"kind": "curve", "inputs": {"x": {"node": "ring"}, "shape": {"points": shape}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "pill"}}}}))
}

/// Twenty heads on a line along U.
fn line20() -> Vec<Cell> {
    (0..20)
        .map(|n| cell(format!("p{n:02}"), [n as f64, 0., 3.]))
        .collect()
}

/// The lit heads of a ring of `heads` as runs of neighbours, wrapping.
fn pieces(on: &[usize], heads: usize) -> usize {
    on.iter()
        .filter(|n| !on.contains(&((**n + heads - 1) % heads)))
        .count()
}

#[test]
fn a_wrapping_chase_leaves_one_end_and_enters_the_other() {
    let times: Vec<f64> = (0..80).map(|i| 2. + i as f64 * 0.025).collect();
    let light = play(&wrapping(2., 2.), &line20(), &times);
    let mut wrapped = false;
    let mut seen = std::collections::BTreeSet::new();
    for t in 0..times.len() {
        let on = lit(&light, t);
        // A tenth of a ring of 20, closed: two or three heads.
        assert!((2..=3).contains(&on.len()), "beat {}: {on:?}", times[t]);
        assert_eq!(pieces(&on, 20), 1, "one pill: {on:?}");
        wrapped |= on.contains(&0) && on.contains(&19);
        seen.extend(on);
    }
    assert!(wrapped, "the pill crosses the ends");
    assert_eq!(seen.len(), 20, "the pill passes every head");
}

#[test]
fn four_overlapping_wrapping_chases_run_a_quarter_apart() {
    // every 0.5, duration 2: four events alive, a quarter of a turn apart.
    let times: Vec<f64> = (0..40).map(|i| 4. + i as f64 * 0.0125).collect();
    // A soft pill, so rounding at an edge cannot switch a head.
    let soft = json!([
        [0, 0, [0.4, 0, 0.6, 1]],
        [0.05, 1, [0.4, 0, 0.6, 1]],
        [0.1, 0],
        [1, 0]
    ]);
    let light = play(&wrapping_with(0.5, 2., soft), &line20(), &times);
    for t in 0..times.len() {
        let on = lit(&light, t);
        assert_eq!(pieces(&on, 20), 4, "four pills: {on:?}");
        // A quarter apart: head n + 5 is as bright as head n.
        for n in 0..20 {
            let (a, b) = (dim(&light, n, t), dim(&light, (n + 5) % 20, t));
            assert!(
                (a - b).abs() < 1e-9,
                "beat {}: head {n} {a} vs {b}",
                times[t]
            );
        }
    }
}

/// Julian's 2 → 8 pills: events speed up from every 1 to every 0.5 beats
/// while each lives 2 → 4 beats, so two pills become eight. Both ramps end
/// at 3/4 of the clip and hold: pills in flight lag duration / every while
/// it changes (about 6.4 at the end of a ramp over the whole clip), and
/// catch up to 8 over the last 4 beats. Each pill enters from beyond one
/// end (shift −0.05) and leaves past the other (shift 1), so it is dark
/// when born and when it ends.
fn two_to_eight_pills() -> ClipGraph {
    graph(json!({
        "clip": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "shape": {"points": [[0, 0], [0.75, 1], [1, 1]]}, "low": 1, "high": 0.5}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "shape": {"points": [[0, 0], [0.75, 1], [1, 1]]}, "low": 2, "high": 4}},
        "k": {"kind": "time", "inputs": {"every": {"node": "curve1"}, "duration": {"node": "curve2"}}},
        "curve3": {"kind": "curve", "inputs": {"x": {"node": "k"}, "low": -0.05, "high": 1}},
        "x": {"kind": "space", "inputs": {"direction": [1, 0, 0], "shift": {"node": "curve3"}}},
        "curve4": {"kind": "curve", "inputs": {"x": {"node": "x"}, "shape": {"points": [
            [0, 0, [0.4, 0, 0.6, 1]], [0.025, 1, [0.4, 0, 0.6, 1]], [0.05, 0], [1, 0]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve4"}}}}))
}

/// 2 → 8 pills on a ring: the same held ramps, but each pill's shift wraps
/// once around the ring over its life, and each pill fades in over the
/// first tenth of its life and out over the last, so births and deaths on
/// the ring never pop.
fn wrapping_pills() -> ClipGraph {
    graph(json!({
        "clip": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "shape": {"points": [[0, 0], [0.75, 1], [1, 1]]}, "low": 1, "high": 0.5}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "shape": {"points": [[0, 0], [0.75, 1], [1, 1]]}, "low": 2, "high": 4}},
        "k": {"kind": "time", "inputs": {"every": {"node": "curve1"}, "duration": {"node": "curve2"}}},
        "curve3": {"kind": "curve", "inputs": {"x": {"node": "k"}}},
        "x": {"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
              "inputs": {"direction": [1, 0, 0], "shift": {"node": "curve3"}}},
        "pill": {"kind": "curve", "inputs": {"x": {"node": "x"}, "shape": {"points": [
            [0, 0, [0.4, 0, 0.6, 1]], [0.025, 1, [0.4, 0, 0.6, 1]], [0.05, 0], [1, 0]]}}},
        "life": {"kind": "curve", "inputs": {"x": {"node": "k"}, "shape": {"points": [[0, 0], [0.1, 1], [0.9, 1], [1, 0]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "math1"}}},
        "math1": {"kind": "math", "inputs": {"values": [{"node": "pill"}, {"node": "life"}]}}}))
}

/// Two hundred heads on a line along U: a fine ring to count pills on.
fn line200() -> Vec<Cell> {
    (0..200)
        .map(|n| cell(format!("p{n:03}"), [n as f64, 0., 3.]))
        .collect()
}

/// The largest change of any light between neighbouring frames `step`
/// beats apart over the clip (value, beat, head), and the most pills seen
/// in each beat.
fn pills_and_largest_step(graph: &ClipGraph, step: f64) -> ((f64, f64, usize), Vec<usize>) {
    let cells = line200();
    let times: Vec<f64> = (0..(16. / step) as usize)
        .map(|i| i as f64 * step)
        .collect();
    let light = play(graph, &cells, &times);
    let mut most = (0., 0., 0);
    for t in 1..times.len() {
        for n in 0..cells.len() {
            let d = (dim(&light, n, t) - dim(&light, n, t - 1)).abs();
            if d > most.0 {
                most = (d, times[t], n);
            }
        }
    }
    let per_beat = (1. / step).round() as usize;
    let counts = (0..times.len())
        .collect::<Vec<_>>()
        .chunks(per_beat)
        .map(|beat| {
            beat.iter()
                .map(|t| pieces(&lit(&light, *t), cells.len()))
                .max()
                .unwrap()
        })
        .collect();
    (most, counts)
}

#[test]
fn two_pills_become_eight_smoothly() {
    let graph = two_to_eight_pills();
    let (coarse, counts) = pills_and_largest_step(&graph, 0.01);
    let (fine, _) = pills_and_largest_step(&graph, 0.005);
    println!("most pills in each beat: {counts:?}");
    println!("largest change between frames: {coarse:?} at step 0.01, {fine:?} at step 0.005");
    // In flight = duration / every: 2 in the first beat, 8 in the last. A
    // pill is dark for an instant at each end, so count the most seen in
    // each beat.
    assert!(counts.windows(2).all(|w| w[1] >= w[0]), "{counts:?}");
    assert!(counts[0] == 2 && *counts.last().unwrap() == 8, "{counts:?}");
    // Motion halves its step when the frames are twice as close; a light
    // that pops on or off does not.
    assert!(
        fine.0 < coarse.0 * 0.7,
        "a light jumps: {coarse:?} then {fine:?}"
    );
}

#[test]
fn two_pills_on_a_ring_become_eight_and_fade_in_and_out_without_a_jump() {
    let (coarse, counts) = pills_and_largest_step(&wrapping_pills(), 0.01);
    let (fine, _) = pills_and_largest_step(&wrapping_pills(), 0.005);
    println!("most pills in each beat: {counts:?}; largest change {coarse:?} then {fine:?}");
    assert!(counts.windows(2).all(|w| w[1] >= w[0]), "{counts:?}");
    assert!(counts[0] == 2 && *counts.last().unwrap() == 8, "{counts:?}");
    assert!(
        fine.0 < coarse.0 * 0.7,
        "a light jumps: {coarse:?} then {fine:?}"
    );
}

/// Zoom out: a wrapped space tiles, so a scale running 0.5 → 0.125 over the
/// clip turns 2 copies of a soft pill into 8, while a shift that slows down
/// drifts them. Every head moves smoothly: the pill is 0 at x 0 and x 1, so
/// the seams of the tiling never show.
fn zoom_out() -> ClipGraph {
    graph(json!({
        "clip": {"kind": "time"},
        "zoom": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "low": 0.5, "high": 0.125}},
        "drift": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "shape": {"points": [[0, 0, "ease-out"], [1, 1]]}}},
        "x": {"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
              "inputs": {"direction": [1, 0, 0], "scale": {"node": "zoom"}, "shift": {"node": "drift"}}},
        "pill": {"kind": "curve", "inputs": {"x": {"node": "x"}, "shape": {"points": [
            [0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "pill"}}}}))
}

#[test]
fn zoom_out_tiles_two_pills_into_eight_without_a_jump() {
    let (coarse, counts) = pills_and_largest_step(&zoom_out(), 0.01);
    let (fine, _) = pills_and_largest_step(&zoom_out(), 0.005);
    println!("most pills in each beat: {counts:?}; largest change {coarse:?} then {fine:?}");
    assert!(counts.windows(2).all(|w| w[1] >= w[0]), "{counts:?}");
    assert!(counts[0] == 2 && *counts.last().unwrap() == 8, "{counts:?}");
    assert!(
        fine.0 < coarse.0 * 0.7,
        "a light jumps: {coarse:?} then {fine:?}"
    );
}

// ---- 2026-09-30 audit: shader rules ----

#[test]
fn a_front_lights_the_last_head_before_it_ends() {
    // A jump reads the value after it, so a front lights a head once it
    // has reached it. Wipe and Grow reach the last head before their end;
    // a meter at its loudest lights every head.
    let times: Vec<f64> = (0..40).map(|i| 4. + i as f64 * 0.1).collect();
    let wipe = play(&preset("Wipe"), &line(), &times);
    assert_eq!(lit(&wipe, 0), vec![0], "the first head at the start");
    assert_eq!(lit(&wipe, times.len() - 1).len(), line().len());
    let fractions: Vec<f64> = (0..64).map(|i| i as f64 / 64. * 16.).collect();
    let grow = play(&preset("Grow"), &grid(), &fractions);
    assert!(lit(&grow, 0).is_empty(), "nothing at the start");
    assert_eq!(lit(&grow, 63).len(), grid().len(), "all before the end");
}

#[test]
fn stepped_chase_lights_each_head_in_exactly_one_block() {
    // Four blocks on a ring of cells, a quarter each: over one event every
    // head is lit in exactly one of the four steps, for any head count.
    for cells in [line(), line20(), bars()] {
        let times = [0.5, 1.5, 2.5, 3.5];
        let light = play(&preset("Stepped chase"), &cells, &times);
        for n in 0..cells.len() {
            let steps = (0..4).filter(|t| light[[n, *t, DIMMER]] > 1e-6).count();
            assert_eq!(steps, 1, "head {n} of {}", cells.len());
        }
    }
}

#[test]
fn alternating_sides_switch_cleanly() {
    // Half way through each event one side goes off as the other comes
    // on: never both, never neither, and the middle head of an odd count
    // goes with one side.
    let cells = line();
    let times: Vec<f64> = (0..32).map(|i| i as f64 / 16.).collect();
    let light = play(&preset("Alternating sides"), &cells, &times);
    for t in 0..times.len() {
        let on = lit(&light, t);
        assert!(!on.is_empty() && on.len() < cells.len(), "at {}", times[t]);
        let left = on.iter().all(|n| *n < 6);
        let right = on.iter().all(|n| *n >= 6);
        assert!(left || right, "one side at {}: {on:?}", times[t]);
    }
    let odd = line13();
    let light = play(&preset("Alternating sides"), &odd, &[0.25, 1.25]);
    let (a, b) = (lit(&light, 0), lit(&light, 1));
    assert_eq!(a.len() + b.len(), odd.len());
    assert!(a.iter().all(|n| !b.contains(n)));
}

/// Thirteen heads along U, the middle one on the centre.
fn line13() -> Vec<Cell> {
    (0..13)
        .map(|i| cell(format!("m{i:02}"), [i as f64, 0., 3.]))
        .collect()
}

#[test]
fn a_pill_of_width_w_lights_heads_in_w_and_shares_none_at_its_ends() {
    // Chase's pill is [shift, shift + 0.2): with heads every 1/19, four
    // heads (0.2 × 19 = 3.8, so 3 or 4) light at any moment.
    let cells = line20();
    let times: Vec<f64> = (0..40).map(|i| 1. + i as f64 * 0.01).collect();
    let light = play(&preset("Chase"), &cells, &times);
    for t in 0..times.len() {
        let on = lit(&light, t);
        assert!(
            (3..=4).contains(&on.len()),
            "{} lit at {}",
            on.len(),
            times[t]
        );
    }
    // Random heads lights half: on an odd count the middle rank sits on
    // the jump and reads the value after it, so the smaller half.
    let light = play(&preset("Random heads"), &line13(), &[0.5]);
    assert_eq!(lit(&light, 0).len(), 6);
}

#[test]
fn audio_reads_one_level_over_the_whole_track() {
    // The band is normalised over the track, not over the clip: clips at
    // different places and of different lengths read the same level at
    // the same beat.
    let graph = graph(json!({
        "a": {"kind": "audio", "inputs": {"low_hz": 40, "high_hz": 100}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "a"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let level = |start: f64, duration: f64, beats: &[f64]| {
        let prepared = PreparedGraph::new(
            &standard_library(),
            &graph,
            Frame {
                features: None,
                cells: &line(),
                beat: start,
                clip_start: start,
                clip_duration: duration,
                seed: 7,
            },
        )
        .unwrap()
        .with_features(Arc::new(Track))
        .unwrap();
        let out = prepared.evaluate_batch(beats).unwrap();
        let light = out[OUTPUT].lighting().unwrap().values().clone();
        beats
            .iter()
            .enumerate()
            .map(|(t, _)| light[[0, t, DIMMER]])
            .collect::<Vec<_>>()
    };
    let beats = [5., 6.5, 7.25];
    let short = level(4., 4., &beats);
    let long = level(0., 32., &beats);
    assert_eq!(short, long);
    // Track's band is 1 + sin(1.7 beat) over 0–2.
    for (b, v) in beats.iter().zip(&short) {
        assert!((v - (1. + (b * 1.7).sin()) / 2.).abs() < 1e-9);
    }
}

#[test]
fn a_bezier_handle_may_overshoot_and_outputs_clamp() {
    // CSS cubic-bezier lets y run outside 0–1: the curve passes above its
    // end value between the points, and the light clamps at 1.
    let graph = graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"},
            "shape": {"points": [[0, 0, [0.3, 1.8, 0.6, 1.4]], [1, 1]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    luma_patterns::clip_graph::check(&graph).expect("an overshooting handle passes");
    let at = graph
        .coordinate_at_heads("space1", frame(&line(), 1.))
        .unwrap();
    assert!(at.iter().all(Option::is_some));
    let shape = match &graph.nodes["curve1"].inputs["shape"] {
        luma_patterns::clip_graph::Input::Points(points) => points.clone(),
        _ => unreachable!(),
    };
    assert!(shape.sample(0.5) > 1.2, "the shape passes above 1");
    // The middle head (x 5/11) is clamped to 1 at the output, where a
    // straight line would give it 0.45.
    let light = play(&graph, &line(), &[1.]);
    assert_eq!(light[[5, 0, DIMMER]], 1.);
}
