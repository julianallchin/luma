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
    // A jump at x 0 or 1 sets the value outside; the ends themselves read
    // the inside, so a pulse on [0, 1] lights both end heads.
    let closed = play(
        &over_line(json!([[0, 0], [0, 1], [1, 1], [1, 0]]), "no"),
        &line(),
        &[1.],
    );
    assert_eq!(lit(&closed, 0), (0..12).collect::<Vec<_>>());
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

#[test]
fn overlapping_events_show_the_biggest_effect_and_ties_the_newest() {
    // At beat 1.5 event 0 is 0.75 old and event 1 is 0.25 old.
    let brighter = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1, "duration": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&brighter, &line(), &[1.5]);
    assert!((light[[0, 0, DIMMER]] - 0.75).abs() < 1e-9);
    let tie = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1, "duration": 2}},
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
    assert_eq!(at, vec![1., 0.5, 0., 0., 0.5, 1.]);
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
    // On a ring the slid coordinate wraps before it is stretched.
    let ring = at(json!({"direction": [1, 0, 0]}), "yes");
    let wrapped = at(
        json!({"direction": [1, 0, 0], "shift": 0.9, "scale": 2}),
        "yes",
    );
    for (a, x) in ring.iter().zip(&wrapped) {
        assert!((x - (a - 0.9).rem_euclid(1.) / 2.).abs() < 1e-9);
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
/// the slash direction over the first 0.2 of the event, a bloom that grows
/// out from a line at 68 % of the rig (a mirror there, and a space along
/// its normal that measures from it, scaled from 4 % to 74 %), a fade for
/// all, and a white-to-red color; cut × bloom × fade is one math node.
fn slash() -> ClipGraph {
    graph(json!({
        "t": {"kind": "time", "inputs": {"every": 2}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "t"}, "shape": {"points": [[0, 0], [0.2, 1], [1, 1]]}}},
        "diag": {"kind": "space", "inputs": {"direction": [-0.82, 0, 0.57], "shift": {"node": "curve1"}}},
        "cut": {"kind": "curve", "inputs": {"x": {"node": "diag"}, "shape": {"points": [[0, 1], [0, 0], [1, 0]]}}},
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
fn matches_by_hand(light: &Array3<f64>, times: &[f64], edge: f64, margin: impl Fn(usize, usize) -> f64) {
    let mut checked = 0;
    for t in 0..times.len() {
        for n in 0..light.dim().0 {
            let m = margin(n, t);
            if m.abs() < edge {
                continue;
            }
            assert_eq!(dim(light, n, t) > 1e-6, m > 0., "head {n} at beat {}", times[t]);
            checked += 1;
        }
    }
    assert!(checked > light.dim().0 * times.len() / 2, "checked {checked}");
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
        cells.iter().map(|c| (0..3).map(|a| c.uvz[a] * d[a]).sum()).collect()
    };
    // Cut alone: the coordinate along (−0.82, 0, 0.57), 0 at the lowest
    // head and 1 at the highest; lit below the shift, which runs 0 → 1 over
    // the first 0.2 of the event. The front starts at +U, low Z.
    let light = play(&slash_brightness("cut"), &cells, &times);
    let d = along(unit([-0.82, 0., 0.57]));
    let (lo, hi) = d.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    matches_by_hand(&light, &times, 1e-6, |n, t| {
        let shift = (times[t] / 2. / 0.2).min(1.);
        shift - (d[n] - lo) / (hi - lo)
    });
    let head = |id: &str| cells.iter().position(|c| c.id == id).unwrap();
    let first_on = |n: usize| (0..times.len()).find(|t| dim(&light, n, *t) > 1e-6);
    assert!(first_on(head("g90")) < first_on(head("g55")));
    assert!(first_on(head("g55")).unwrap() < first_on(head("g18")).unwrap());
    // The far corner sits at exactly 1, where the shift ends: never cut on.
    assert_eq!(first_on(head("g09")), None);
    // Bloom alone: lit within scale × extent of the line at 0.68 along
    // (0.57, 0, 0.82); scale runs 0.04 → 0.74 over the event.
    let light = play(&slash_brightness("bloom"), &cells, &times);
    let b = along(unit([0.57, 0., 0.82]));
    let (lo, hi) = b.iter().fold((f64::MAX, f64::MIN), |(a, c), v| (a.min(*v), c.max(*v)));
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
fn every_preset_plays_as_version_2_did() {
    let old = v2_frames();
    let beats: Vec<f64> = serde_json::from_value(old["beats"].clone()).unwrap();
    // Presets whose meaning changed, with the largest difference allowed
    // on a rig: Mirror's space now measures from the mirror plane (its
    // shift and scale halved): exact when a head sits on the plane (line13),
    // half a spacing off otherwise, and on the square rigs its best-fit
    // directions disagree. Wrapping ripple is now Ripple on a wrapped
    // space, as time has no length any more: version 2's length 0.7143 was
    // 1 / 1.4 rounded. Everything else plays exactly (the fixture keeps 6
    // decimals).
    let changed: &[(&str, &str, f64)] = &[
        ("Mirror", "bars", 1.0),
        ("Mirror", "spread", 1.0),
        ("Wrapping ripple", "bars", 1e-3),
        ("Wrapping ripple", "spread", 1e-3),
        ("Wrapping ripple", "line13", 1e-3),
    ];
    let mut report = Vec::new();
    for (name, rigs) in old["clips"].as_object().unwrap() {
        if name == "Slash" {
            continue;
        }
        let graph = preset(name);
        for (rig, frames) in rigs.as_object().unwrap() {
            let cells: Vec<Cell> = serde_json::from_value(old["rigs"][rig].clone()).unwrap();
            let (most, mean) = difference(frames, &v3_values(&graph, &cells, &beats));
            report.push(format!(
                "{name} on {rig}: largest {most:.2e}, mean {mean:.2e}"
            ));
            let allowed = changed
                .iter()
                .find(|(changed, on, _)| changed == name && on == rig)
                .map_or(1e-6, |(_, _, allowed)| *allowed);
            assert!(most <= allowed, "{name} on {rig}: {most} (mean {mean})");
        }
    }
    println!("{}", report.join("\n"));
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
fn dissolve_keeps_each_light_on_until_its_own_delay() {
    // Delays run 0–16 beats over a random order; a waiting light reads the
    // curve's first value (on) until its clock starts.
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
            cells.push(cell(format!("d{i:02}{k:03}"), [r * angle.cos(), 0., r * angle.sin()]));
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

/// The first form of 2 → 8 pills: ramps over the whole clip and a shift
/// that wraps around the ring once over each pill's life.
fn wrapped_pills() -> ClipGraph {
    graph(json!({
        "clip": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "low": 1, "high": 0.5}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "clip"}, "low": 2, "high": 4}},
        "k": {"kind": "time", "inputs": {"every": {"node": "curve1"}, "duration": {"node": "curve2"}}},
        "curve3": {"kind": "curve", "inputs": {"x": {"node": "k"}}},
        "x": {"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
              "inputs": {"direction": [1, 0, 0], "shift": {"node": "curve3"}}},
        "curve4": {"kind": "curve", "inputs": {"x": {"node": "x"}, "shape": {"points": [
            [0, 0, [0.4, 0, 0.6, 1]], [0.025, 1, [0.4, 0, 0.6, 1]], [0.05, 0], [1, 0]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve4"}}}}))
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
        .map(|beat| beat.iter().map(|t| pieces(&lit(&light, *t), cells.len())).max().unwrap())
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

/// The same pills, each fading in over the first tenth of its life and
/// out over the last: births and deaths at the seam no longer pop.
#[test]
fn two_pills_become_more_without_a_jump_when_each_fades_in_and_out() {
    let mut graph = wrapped_pills();
    let extra = json!({
        "life": {"kind": "curve", "inputs": {"x": {"node": "k"}, "shape": {"points": [[0, 0], [0.1, 1], [0.9, 1], [1, 0]]}}},
        "math1": {"kind": "math", "inputs": {"values": [{"node": "curve4"}, {"node": "life"}]}}});
    for (id, node) in extra.as_object().unwrap() {
        graph
            .nodes
            .insert(id.clone(), serde_json::from_value(node.clone()).unwrap());
    }
    graph.nodes.get_mut("color1").unwrap().inputs.insert(
        "brightness".into(),
        luma_patterns::clip_graph::Input::wire("math1"),
    );
    let (coarse, counts) = pills_and_largest_step(&graph, 0.01);
    let (fine, _) = pills_and_largest_step(&graph, 0.005);
    println!("pills per beat: {counts:?}; largest change {coarse:?} then {fine:?}");
    assert!(counts.windows(2).all(|w| w[1] + 1 >= w[0]), "{counts:?}");
    assert!(*counts.last().unwrap() > counts[1] + 2, "{counts:?}");
    assert!(
        fine.0 < coarse.0 * 0.7,
        "a light jumps: {coarse:?} then {fine:?}"
    );
}
