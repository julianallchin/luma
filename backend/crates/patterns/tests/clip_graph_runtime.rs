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

#[test]
fn radial_runs_from_the_nearest_head_to_the_farthest() {
    // Distances 1, 2 and 3 from the centre on each side.
    let cells: Vec<Cell> = [-3., -2., -1., 1., 2., 3.]
        .into_iter()
        .enumerate()
        .map(|(n, u)| cell(format!("h{n}"), [u, 0., 3.]))
        .collect();
    let radial = graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "radial", "wrap": "no"},
                   "inputs": {"offset": 0, "width": 0.25}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&radial, &cells, &[1.]);
    assert_eq!(lit(&light, 0), vec![2, 3], "only the nearest heads read 0");
    let far = graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "radial", "wrap": "no"},
                   "inputs": {"offset": 0.9, "width": 0.1}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    assert_eq!(
        lit(&play(&far, &cells, &[1.]), 0),
        vec![0, 5],
        "the farthest read 1"
    );
}

#[test]
fn a_wrapped_stroke_wider_than_half_the_axis_is_exact() {
    // Start 0.6, width 0.8: lit from 0.6 through 1 and on from 0 to 0.4.
    let light = play(&stroke(0.6, 0.8, "yes"), &line(), &[1.]);
    let dark: Vec<usize> = (0..12).filter(|n| !lit(&light, 0).contains(n)).collect();
    assert_eq!(dark, vec![5, 6]);
    let ramp = graph(json!({
        "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "yes"},
                   "inputs": {"direction": [1, 0, 0], "offset": 0.6, "width": 0.8}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}));
    let light = play(&ramp, &line(), &[1.]);
    // The ramp climbs from head 7 to head 11 and goes on across the wrap
    // from head 0 to head 4.
    let ramp: Vec<f64> = [7, 8, 9, 10, 11, 0, 1, 2, 3, 4]
        .iter()
        .map(|n| light[[*n, 0, DIMMER]])
        .collect();
    assert!(ramp.windows(2).all(|w| w[0] < w[1]), "{ramp:?}");
}

#[test]
fn a_wrapped_line_is_a_ring_whose_ends_do_not_meet() {
    // A stroke one spacing wide lights one head wherever it is, also just
    // below 0, where a pill enters. If the lowest and the highest head sat
    // on one place, it would light both ends there.
    let heads = line().len();
    for k in -1..heads as i32 {
        let offset = (4. * k as f64 + 1.) / (4. * heads as f64);
        let light = play(&stroke(offset, 1. / heads as f64, "yes"), &line(), &[1.]);
        assert_eq!(
            lit(&light, 0).len(),
            1,
            "offset {offset}: {:?}",
            lit(&light, 0)
        );
    }
    // Not wrapped, the ends stay at 0 and 1.
    let light = play(&stroke(-0.01, 0.02, "no"), &line(), &[1.]);
    assert_eq!(lit(&light, 0), vec![0]);
    let light = play(&stroke(0.99, 0.02, "no"), &line(), &[1.]);
    assert_eq!(lit(&light, 0), vec![heads - 1]);
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
fn a_space_node_gives_each_head_its_place_in_the_stroke() {
    let cells = line();
    let at = stroke(0., 0.5, "no")
        .coordinate_at_heads("space1", frame(&cells, 1.))
        .unwrap();
    // The low half is inside, in order along the line; the rest is outside.
    let inside: Vec<f64> = at.iter().flatten().copied().collect();
    assert_eq!(inside.len(), 6);
    assert!(at[..6].iter().all(Option::is_some));
    assert!(at[6..].iter().all(Option::is_none));
    assert!(inside.windows(2).all(|w| w[0] < w[1]));
    assert!(inside.iter().all(|x| (0. ..=1.).contains(x)));
    // Cell order is the caller's, not the id order.
    let reversed: Vec<Cell> = cells.iter().rev().cloned().collect();
    let back = stroke(0., 0.5, "no")
        .coordinate_at_heads("space1", frame(&reversed, 1.))
        .unwrap();
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
