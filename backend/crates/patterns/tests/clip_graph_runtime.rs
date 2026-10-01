//! Clip graphs played: every preset lights a stand-in rig, the key presets
//! light the heads they should, and shuffle, curve, per-head clocks, lists,
//! overlap, mirror, turning line and noise behave as the spec says.
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
    serde_json::from_value(json!({"version": 2, "nodes": nodes})).expect("graph JSON")
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
        "clock1": {"kind": "clock", "inputs": {"every": 1}},
        "shuffle1": {"kind": "shuffle", "inputs": {"clock": {"node": "clock1"}}},
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
    // Head n starts n / 11 of the clip late and runs for a quarter of it.
    let graph = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0]}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}, "low": 0, "high": 0.75}},
        "time1": {"kind": "time", "inputs": {"delay": {"node": "curve1"}, "length": 0.25}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "time1"},
                   "shape": {"points": [[0, 0.2], [0, 0.4], [1, 0.6], [1, 0.8]]}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}}}));
    // Beat 6 of 16 is 0.375 of the clip: head 0 is done, head 11 (delay
    // 0.75) has not started, and head 2 (delay 0.136) is on its way.
    let light = play(&graph, &line(), &[6.]);
    assert!((light[[0, 0, DIMMER]] - 0.8).abs() < 1e-9);
    assert!((light[[11, 0, DIMMER]] - 0.2).abs() < 1e-9);
    let local = (0.375 - 0.75 * 2. / 11.) / 0.25;
    assert!((light[[2, 0, DIMMER]] - (0.4 + 0.2 * local)).abs() < 1e-9);
}

#[test]
fn a_list_multiplies_its_items() {
    let graph = graph(json!({
        "space1": {"kind": "space", "inputs": {"direction": [1, 0, 0]}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "time1": {"kind": "time"},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": [{"node": "curve1"}, {"node": "curve2"}, 0.5]}}}));
    let light = play(&graph, &line(), &[4., 12.]);
    for (t, progress) in [(0, 0.25), (1, 0.75)] {
        for n in 0..12 {
            let expected = n as f64 / 11. * progress * 0.5;
            assert!((light[[n, t, DIMMER]] - expected).abs() < 1e-9);
        }
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
fn a_shift_slides_the_coordinate_and_a_length_stretches_it() {
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
        json!({"direction": [1, 0, 0], "shift": 0.25, "length": 0.5}),
        "no",
    );
    for (a, x) in plain.iter().zip(&slid) {
        assert!((x - (a - 0.25) / 0.5).abs() < 1e-9, "{plain:?} {slid:?}");
    }
    // On a ring the slid coordinate wraps before it is stretched.
    let ring = at(json!({"direction": [1, 0, 0]}), "yes");
    let wrapped = at(
        json!({"direction": [1, 0, 0], "shift": 0.9, "length": 2}),
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

/// Slash, every 2 beats: a cut that steps heads on across the slash
/// direction over the first 0.2 of the event, a bloom that fades heads in
/// from the slash line outward, slower the farther they are, and a shared
/// fade; the three multiply.
fn slash() -> ClipGraph {
    graph(json!({
        "clock1":{"kind":"clock","inputs":{"every":2}},
        "time1":{"kind":"time","inputs":{"clock":{"node":"clock1"}}},
        "space1":{"kind":"space","inputs":{"direction":[-0.8192,0,0.5736]}},
        "curve1":{"kind":"curve","inputs":{"x":{"node":"space1"},"low":0,"high":0.2}},
        "time2":{"kind":"time","inputs":{"clock":{"node":"clock1"},"delay":{"node":"curve1"}}},
        "curve2":{"kind":"curve","inputs":{"x":{"node":"time2"},"shape":{"points":[[0,0],[0,1],[1,1]]}}},
        "space2":{"kind":"space","inputs":{"direction":[0.5736,0,0.8192]}},
        "curve3":{"kind":"curve","inputs":{"x":{"node":"space2"},"shape":{"points":[[0,1],[0.68,0],[1,0.47]]},"low":0,"high":0.9}},
        "curve4":{"kind":"curve","inputs":{"x":{"node":"space2"},"shape":{"points":[[0,1],[0.68,0],[1,0.47]]},"low":0.05,"high":0.4}},
        "time3":{"kind":"time","inputs":{"clock":{"node":"clock1"},"delay":{"node":"curve3"},"length":{"node":"curve4"}}},
        "curve5":{"kind":"curve","inputs":{"x":{"node":"time3"}}},
        "curve6":{"kind":"curve","inputs":{"x":{"node":"time1"},"shape":{"points":[[0,1,"hold"],[0.2,1,"sine-out"],[1,0]]}}},
        "curve7":{"kind":"curve","settings":{"kind":"color"},"inputs":{"x":{"node":"time1"},"gradient":{"stops":[{"t":0,"color":[1,1,1]},{"t":0.2,"color":[1,1,1]},{"t":0.5,"color":[1,0,0.01]},{"t":1,"color":[1,0,0.01]}]}}},
        "color1":{"kind":"color","inputs":{"color":{"node":"curve7"},"brightness":[{"node":"curve2"},{"node":"curve5"},{"node":"curve6"}]}}}))
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
