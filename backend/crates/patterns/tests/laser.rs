//! Laser clips: the checker's rules for `beams` and the zone, and the beams a
//! laser graph places over two projectors.
use luma_patterns::clip_graph::ClipGraph;
use luma_patterns::laser::BEAM_OUTPUT;
use luma_patterns::{standard_library, Cell, Frame, PreparedGraph};
use serde_json::{json, Value};

fn graph(nodes: Value) -> ClipGraph {
    serde_json::from_value(json!({"version": 3, "nodes": nodes})).expect("graph JSON")
}

/// `beams` beam cells on each of two projectors, left at U −2 and right at
/// U 2.
fn projectors(beams: usize) -> Vec<Cell> {
    ["left", "right"]
        .into_iter()
        .zip([-2., 2.])
        .flat_map(|(name, u)| {
            (0..beams).map(move |i| Cell {
                id: format!("{name}:{i}"),
                group: "lasers".into(),
                world: [u, 0., 4.],
                uvz: [u, 0., 4.],
            })
        })
        .collect()
}

/// `[x, y, join, alpha]` per beam at `beat` of a clip over beats 0–4, beams
/// in id order.
fn beams(graph: &ClipGraph, cells: &[Cell], beat: f64) -> Vec<[f64; 4]> {
    graph.check().expect("a valid graph");
    let frame = Frame {
        cells,
        features: None,
        beat,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    let prepared = PreparedGraph::new(&standard_library(), graph, frame).expect("prepares");
    let out = prepared.evaluate_batch(&[beat]).expect("evaluates");
    let mut ids: Vec<String> = cells.iter().map(|c| c.id.clone()).collect();
    ids.sort();
    luma_patterns::laser::Beam::read(&out[BEAM_OUTPUT], &ids, 1).expect("beams")[0].clone()
}

#[test]
fn a_fan_spreads_its_beams_across_the_canvas_on_every_projector() {
    let graph = graph(json!({"laser1": {"kind": "laser", "inputs": {"size": 1}}}));
    let out = beams(&graph, &projectors(5), 1.);
    let xs: Vec<f64> = out.iter().map(|b| b[0]).collect();
    assert_eq!(xs, [-1., -0.5, 0., 0.5, 1., -1., -0.5, 0., 0.5, 1.]);
    assert!(out.iter().all(|b| b[1] == 0. && b[3] == 1.));
}

#[test]
fn a_laser_writes_no_light_of_its_own() {
    let graph = graph(json!({"laser1": {"kind": "laser"}}));
    let cells = projectors(2);
    let frame = Frame {
        cells: &cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    let prepared = PreparedGraph::new(&standard_library(), &graph, frame).unwrap();
    let out = prepared.evaluate_batch(&[0.]).unwrap();
    let lighting = out[luma_patterns::clip_graph::OUTPUT]
        .lighting()
        .expect("lighting");
    assert_eq!(lighting.writes(), [false; 6]);
}

/// A mirror across the stage's middle flips one side's canvas: a fan pushed
/// to one side lands on opposite sides of the two projectors.
#[test]
fn a_mirror_flips_the_canvas_of_the_folded_projectors() {
    let graph = graph(json!({
        "mirror1": {"kind": "mirror", "inputs": {"direction": [1, 0, 0]}},
        "laser1": {"kind": "laser", "inputs": {"heads": {"node": "mirror1"}, "x": -0.5, "size": 0.25}},
    }));
    let out = beams(&graph, &projectors(3), 1.);
    let (left, right) = out.split_at(3);
    for (l, r) in left.iter().zip(right.iter()) {
        assert!((l[0] + r[0]).abs() < 1e-12, "{l:?} {r:?}");
    }
    assert!(left
        .iter()
        .all(|b| b[0] != 0. && b[0].signum() == left[0][0].signum()));
}

/// The x offset follows a curve over time: the slide-in effect.
#[test]
fn a_single_beam_slides_in_and_holds() {
    let graph = graph(json!({
        "time1": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"},
                   "shape": {"points": [[0, 0], [0.25, 1], [1, 1]]}, "low": -1, "high": 0.3}},
        "laser1": {"kind": "laser", "inputs": {"beams": 1, "x": {"node": "curve1"}}},
    }));
    let cells = projectors(1);
    let start = beams(&graph, &cells, 0.)[0][0];
    let held = beams(&graph, &cells, 2.)[0][0];
    let later = beams(&graph, &cells, 3.5)[0][0];
    assert!(start < -0.9, "{start}");
    assert!((held - 0.3).abs() < 1e-9 && (later - 0.3).abs() < 1e-9);
}

#[test]
fn beams_take_a_whole_number_and_no_wire() {
    let error = |nodes: Value| graph(nodes).check().unwrap_err().0;
    assert_eq!(
        error(json!({"laser1": {"kind": "laser", "inputs": {"beams": 2.5}}})),
        "laser1.beams: expected a whole number; got 2.5. Example: beams=8"
    );
    assert!(error(json!({
        "time1": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 1, "high": 8}},
        "laser1": {"kind": "laser", "inputs": {"beams": {"node": "curve1"}}},
    }))
    .starts_with("laser1.beams: expected a whole number that stays for the whole clip"));
}

#[test]
fn a_zone_is_any_text() {
    graph(json!({"laser1": {"kind": "laser", "settings": {"zone": "a1b2", "shape": "circle"}}}))
        .check()
        .unwrap();
}
