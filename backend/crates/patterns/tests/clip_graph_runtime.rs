//! Clip graphs played: every preset lights a stand-in rig, and the stroke,
//! shuffle, curve, overlap, mirror, turning line and noise behave as the
//! spec says.
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
}

fn graph(nodes: Value) -> ClipGraph {
    serde_json::from_value(json!({"version": 1, "nodes": nodes})).expect("graph JSON")
}

/// The lighting of `graph` over `cells` at `beats`, for a clip over beats
/// 0–16: (head, time, channel), heads in id order.
fn play(graph: &ClipGraph, cells: &[Cell], beats: &[f64]) -> Array3<f64> {
    run(graph, cells, beats).0
}

fn run(graph: &ClipGraph, cells: &[Cell], beats: &[f64]) -> (Array3<f64>, Option<Array3<f64>>) {
    let prepared = PreparedGraph::new(
        &standard_library(),
        graph,
        Frame {
            features: None,
            cells,
            beat: 0.,
            clip_start: 0.,
            clip_duration: 16.,
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

fn stroke(offset: f64, width: f64, wrap: &str) -> ClipGraph {
    graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "line", "wrap": wrap},
                   "inputs": {"direction": [1, 0, 0], "offset": offset, "width": width}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}))
}

#[test]
fn a_stroke_lights_its_share_of_the_line() {
    let light = play(&stroke(0., 0.5, "no"), &line(), &[1.]);
    assert_eq!(lit(&light, 0), (0..6).collect::<Vec<_>>());
    // Past the end of the axis: only a wrapping stroke comes back at 0.
    let wrapped = play(&stroke(0.9, 0.3, "yes"), &line(), &[1.]);
    assert!(lit(&wrapped, 0).contains(&0));
    let clipped = play(&stroke(0.9, 0.3, "no"), &line(), &[1.]);
    assert!(!lit(&clipped, 0).contains(&0));
    assert!(lit(&clipped, 0).contains(&11));
}

#[test]
fn a_shuffle_lights_the_same_set_within_an_event_and_a_new_one_after() {
    let graph = graph(json!({
        "clock1": {"kind": "clock", "inputs": {"every": 1}},
        "shuffle1": {"kind": "shuffle", "inputs": {"clock": {"node": "clock1"}}},
        "space1": {"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                   "inputs": {"heads": {"node": "shuffle1"}, "offset": 0, "width": 0.5}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
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
fn outside_a_stroke_a_curve_gives_low_and_a_color_curve_black() {
    let number = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0], "offset": 0, "width": 0.5}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}, "low": 0.2, "high": 1}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&number, &line(), &[1.]);
    assert!((light[[0, 0, DIMMER]] - 1.).abs() < 1e-9);
    assert!((light[[11, 0, DIMMER]] - 0.2).abs() < 1e-9);
    let color = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0], "offset": 0, "width": 0.5}},
        "curve1": {"kind": "curve", "settings": {"kind": "color"},
                   "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]},
                              "gradient": {"stops": [{"t": 0, "color": [1, 1, 1]}, {"t": 1, "color": [1, 1, 1]}]}}},
        "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}}}));
    let light = play(&color, &line(), &[1.]);
    assert!(light[[0, 0, DIMMER]] > 0.9);
    assert_eq!(light[[11, 0, DIMMER]], 0.);
}

/// Red while an event is young, blue once it is old, both at full peak.
fn red_then_blue(clock: &str) -> Value {
    json!({"kind": "curve", "settings": {"kind": "color"},
           "inputs": {"x": {"node": clock}, "shape": {"points": [[0, 1, "hold"], [0.5, 0], [1, 0]]},
                      "gradient": {"stops": [{"t": 0, "color": [1, 0, 0]}, {"t": 1, "color": [0, 0, 1]}]}}})
}

#[test]
fn overlapping_events_show_the_biggest_effect_and_ties_the_newest() {
    // At beat 1.5 event 0 is 0.75 old and event 1 is 0.25 old.
    let brighter = graph(json!({
        "clock1": {"kind": "clock", "inputs": {"every": 1, "duration": 2}},
        "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&brighter, &line(), &[1.5]);
    assert!((light[[0, 0, DIMMER]] - 0.75).abs() < 1e-9);
    let tie = graph(json!({
        "clock1": {"kind": "clock", "inputs": {"every": 1, "duration": 2}},
        "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
        "curve1": red_then_blue("time1"),
        "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}}}));
    let light = play(&tie, &line(), &[1.5]);
    // Event 0, 0.75 old, is red; event 1, 0.25 old, is blue. Equal peaks:
    // the newer one, blue, shows.
    assert!(light[[0, 0, 2]] > 0.99 && light[[0, 0, 0]] < 0.01);
}

#[test]
fn two_clocks_on_color_and_brightness_are_independent() {
    let graph = graph(json!({
        "clock1": {"kind": "clock", "inputs": {"every": 1}},
        "clock2": {"kind": "clock", "inputs": {"every": 3}},
        "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
        "time2": {"kind": "time", "inputs": {"clock": {"node": "clock2"}}},
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
