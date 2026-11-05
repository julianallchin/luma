//! Laser clips: the checker's rules for figures, `count`, `along` and the
//! zone, and the elements a laser graph draws over two projectors.
use luma_patterns::clip_graph::ClipGraph;
use luma_patterns::laser::{element_output, Sample, ALONG_SAMPLES};
use luma_patterns::{standard_library, Cell, Frame, PreparedGraph};
use serde_json::{json, Value};

fn graph(nodes: Value) -> ClipGraph {
    serde_json::from_value(json!({"version": 3, "nodes": nodes})).expect("graph JSON")
}

/// `nodes` and a laser that draws the one figure `figure1`.
fn one_figure(mut nodes: Value) -> ClipGraph {
    nodes["laser1"] = json!({"kind": "laser", "inputs": {"figures": [{"node": "figure1"}]}});
    graph(nodes)
}

/// `count` element cells on each of two projectors, left at U −2 and right
/// at U 2, each projector's spread a little across it.
fn projectors(count: usize) -> Vec<Cell> {
    ["left", "right"]
        .into_iter()
        .zip([-2., 2.])
        .flat_map(|(name, u)| {
            (0..count).map(move |i| {
                let at = u + 0.01 * i as f64;
                Cell {
                    id: format!("{name}:{i}"),
                    group: "lasers".into(),
                    world: [at, 0., 4.],
                    uvz: [at, 0., 4.],
                }
            })
        })
        .collect()
}

/// Each element's alpha and samples at `beat` of a clip over beats 0–4,
/// elements in id order, from the laser's first figure.
fn elements(graph: &ClipGraph, cells: &[Cell], beat: f64) -> Vec<(f64, Vec<Sample>)> {
    figure(graph, cells, beat, 0)
}

/// [`elements`] of the laser's figure `f`, over every cell.
fn figure(graph: &ClipGraph, cells: &[Cell], beat: f64, f: usize) -> Vec<(f64, Vec<Sample>)> {
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
    luma_patterns::laser::Element::read(&out[&element_output(f)], &ids, 1).expect("elements")[0]
        .clone()
}

fn xs(out: &[(f64, Vec<Sample>)]) -> Vec<f64> {
    out.iter().map(|(_, samples)| samples[0].at[0]).collect()
}

/// With no x, beams sit at their home places: a row across the canvas as
/// wide as `size`, the same on every projector.
#[test]
fn beams_with_no_x_spread_across_the_canvas_on_every_projector() {
    let graph =
        one_figure(json!({"figure1": {"kind": "figure", "inputs": {"count": 5, "size": 1}}}));
    let out = elements(&graph, &projectors(5), 1.);
    assert_eq!(xs(&out), [-1., -0.5, 0., 0.5, 1., -1., -0.5, 0., 0.5, 1.]);
    assert!(out
        .iter()
        .all(|(alpha, s)| *alpha == 1. && s.len() == 1 && s[0].at[1] == 0.));
    // A beam is a point: white by default, no figure to scale.
    assert!(out
        .iter()
        .all(|(_, s)| s[0].rgb == [1.; 3] && s[0].frame == [[0.; 2]; 2]));
}

/// The old fan is x over the elements' order on each projector.
#[test]
fn x_over_space_spreads_each_projectors_elements() {
    let graph = one_figure(json!({
        "split1": {"kind": "split"},
        "order": {"kind": "space", "inputs": {"heads": {"node": "split1"}}, "settings": {"kind": "order"}},
        "spread": {"kind": "curve", "inputs": {"x": {"node": "order"}, "low": -1, "high": 1}},
        "figure1": {"kind": "figure", "inputs": {"count": 4, "x": {"node": "spread"}}},
    }));
    let out = xs(&elements(&graph, &projectors(4), 1.));
    let (left, right) = out.split_at(4);
    assert_eq!(left, right);
    assert!(left.windows(2).all(|pair| pair[0] < pair[1]), "{left:?}");
    assert!(left[0] > -1. && left[3] < 1. && (left[0] + left[3]).abs() < 1e-12);
}

#[test]
fn a_laser_writes_no_light_of_its_own() {
    let graph = one_figure(json!({"figure1": {"kind": "figure"}}));
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

/// A mirror across the stage's middle flips one side's canvas: elements
/// pushed to one side land on opposite sides of the two projectors, and a
/// line's frame turns with them.
#[test]
fn a_mirror_flips_the_canvas_of_the_folded_projectors() {
    let graph = one_figure(json!({
        "mirror1": {"kind": "mirror", "inputs": {"direction": [1, 0, 0]}},
        "figure1": {"kind": "figure", "settings": {"kind": "line"},
                   "inputs": {"heads": {"node": "mirror1"}, "x": -0.5, "rotation": 30}},
    }));
    let out = elements(&graph, &projectors(1), 1.);
    let (left, right) = (&out[0].1[0], &out[1].1[0]);
    assert!(
        (left.at[0] + right.at[0]).abs() < 1e-12,
        "{left:?} {right:?}"
    );
    assert!(left.at[0] != 0.);
    // Reflected across U, the line's end runs the other way across.
    assert!((left.frame[0][0] + right.frame[0][0]).abs() < 1e-12);
    assert!((left.frame[1][0] - right.frame[1][0]).abs() < 1e-12);
}

/// The x place follows a curve over time: the slide-in effect.
#[test]
fn a_single_beam_slides_in_and_holds() {
    let graph = one_figure(json!({
        "time1": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"},
                   "shape": {"points": [[0, 0], [0.25, 1], [1, 1]]}, "low": -1, "high": 0.3}},
        "figure1": {"kind": "figure", "inputs": {"count": 1, "x": {"node": "curve1"}}},
    }));
    let cells = projectors(1);
    let at = |beat| xs(&elements(&graph, &cells, beat))[0];
    assert!(at(0.) < -0.9, "{}", at(0.));
    assert!((at(2.) - 0.3).abs() < 1e-9 && (at(3.5) - 0.3).abs() < 1e-9);
}

/// `y` from `along` bends a line: the solver gets the line's place at
/// every sample, and the bend peaks in the middle.
#[test]
fn along_bends_a_line() {
    let graph = one_figure(json!({
        "along1": {"kind": "along"},
        "bend": {"kind": "curve", "inputs": {"x": {"node": "along1"},
                 "shape": {"points": [[0, 0], [0.5, 1], [1, 0]]}, "low": 0, "high": 0.5}},
        "figure1": {"kind": "figure", "settings": {"kind": "line"}, "inputs": {"y": {"node": "bend"}}},
    }));
    let out = elements(&graph, &projectors(1), 1.);
    let samples = &out[0].1;
    assert_eq!(samples.len(), ALONG_SAMPLES);
    let ys: Vec<f64> = samples.iter().map(|s| s.at[1]).collect();
    let peak = ys.iter().copied().fold(0., f64::max);
    assert!(ys[0].abs() < 1e-12 && ys[ALONG_SAMPLES - 1].abs() < 1e-12);
    assert!(peak > 0.45, "{ys:?}");
    // What does not read along is the same at every sample.
    assert!(samples.iter().all(|s| s.at[0] == 0. && s.rgb == [1.; 3]));
}

/// A colour gradient along a ring, and a beam, which has one place along
/// it, reads along's start.
#[test]
fn colour_from_along_is_a_gradient_and_a_beam_has_one_sample() {
    let nodes = |kind: &str| {
        one_figure(json!({
            "along1": {"kind": "along"},
            "fade": {"kind": "curve", "settings": {"kind": "color"}, "inputs": {"x": {"node": "along1"},
                     "gradient": {"stops": [{"t": 0, "color": [1, 0, 0]}, {"t": 1, "color": [0, 0, 1]}]}}},
            "figure1": {"kind": "figure", "settings": {"kind": kind}, "inputs": {"color": {"node": "fade"}}},
        }))
    };
    let ring = elements(&nodes("ring"), &projectors(1), 1.);
    let colours: Vec<[f64; 3]> = ring[0].1.iter().map(|s| s.rgb).collect();
    assert_eq!(colours.len(), ALONG_SAMPLES);
    assert!(colours[0][0] > colours[ALONG_SAMPLES - 1][0]);
    assert!(colours[0][2] < colours[ALONG_SAMPLES - 1][2]);
    let beams = elements(&nodes("beams"), &projectors(3), 1.);
    assert!(beams.iter().all(|(_, s)| s.len() == 1 && s[0].rgb[0] > 0.9));
}

/// `along` is only for laser elements: anywhere else it is a clear error.
#[test]
fn along_reaches_only_a_figures_element_inputs() {
    let error = |nodes: Value| graph(nodes).check().unwrap_err().0;
    let message = error(json!({
        "along1": {"kind": "along"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "along1"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}},
    }));
    assert!(
        message.starts_with("color1.brightness: expected a value that is the same all along each laser element; got a wire that varies along it from curve1"),
        "{message}"
    );
    let message = error(json!({
        "along1": {"kind": "along"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "along1"}}},
        "figure1": {"kind": "figure"},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "figure1"}], "alpha": {"node": "curve1"}}},
    }));
    assert!(message.starts_with("laser1.alpha: expected"), "{message}");
    let message = error(json!({
        "along1": {"kind": "along"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "along1"}}},
        "space1": {"kind": "space", "inputs": {"at": {"node": "curve1"}}},
        "curve2": {"kind": "curve", "inputs": {"x": {"node": "space1"}}},
        "figure1": {"kind": "figure", "inputs": {"x": {"node": "curve2"}}},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "figure1"}]}},
    }));
    assert!(message.starts_with("space1.at: expected"), "{message}");
}

/// A shuffled stab over sixteen beams lights the share of them its curve
/// asks for, never every element: each element's light is its own.
#[test]
fn a_shuffled_stab_lights_only_its_own_elements() {
    let graph = graph(json!({
        "time1": {"kind": "time", "inputs": {"every": 1}},
        "shuffle1": {"kind": "shuffle", "inputs": {"time": {"node": "time1"}}},
        "space1": {"kind": "space", "inputs": {"heads": {"node": "shuffle1"}}, "settings": {"kind": "order"}},
        "stab": {"kind": "curve", "inputs": {"x": {"node": "space1"},
                 "shape": {"points": [[0, 1], [0.125, 1], [0.125, 0], [1, 0]]}}},
        "figure1": {"kind": "figure", "inputs": {"count": 16}},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "figure1"}], "alpha": {"node": "stab"}}},
    }));
    let cells: Vec<Cell> = projectors(16).into_iter().take(16).collect();
    let mut lit_sets = std::collections::BTreeSet::new();
    for beat in [0.1, 1.1, 2.1, 3.1] {
        let lit: Vec<usize> = elements(&graph, &cells, beat)
            .iter()
            .enumerate()
            .filter(|(_, (alpha, _))| *alpha > 0.)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(lit.len(), 2, "beat {beat}: {lit:?}");
        lit_sets.insert(lit);
    }
    assert!(lit_sets.len() > 1, "the shuffle never moved: {lit_sets:?}");
}

#[test]
fn count_and_sides_take_a_whole_number_and_no_wire() {
    let error = |nodes: Value| one_figure(nodes).check().unwrap_err().0;
    assert_eq!(
        error(json!({"figure1": {"kind": "figure", "inputs": {"count": 2.5}}})),
        "figure1.count: expected a whole number; got 2.5. Example: count=8"
    );
    assert!(error(json!({
        "time1": {"kind": "time"},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}, "low": 1, "high": 8}},
        "figure1": {"kind": "figure", "inputs": {"count": {"node": "curve1"}}},
    }))
    .starts_with("figure1.count: expected a whole number that stays for the whole clip"));
    assert!(
        error(json!({"figure1": {"kind": "figure", "inputs": {"sides": 2}}}))
            .starts_with("figure1.sides: expected")
    );
}

#[test]
fn a_zone_is_any_text() {
    graph(json!({
        "figure1": {"kind": "figure", "settings": {"kind": "polygon"}},
        "laser1": {"kind": "laser", "settings": {"zone": "a1b2"},
                   "inputs": {"figures": [{"node": "figure1"}]}},
    }))
    .check()
    .unwrap();
}

/// A sheet with rails and beams: three figures, one clip. Each figure's
/// elements are its own heads, so the rails' home row spans the canvas
/// whatever the other figures draw, and the sheet keeps its one element.
#[test]
fn figures_of_several_kinds_draw_in_one_clip() {
    let graph = graph(json!({
        "sheet": {"kind": "figure", "settings": {"kind": "line"}, "inputs": {"size": 0.8}},
        "rails": {"kind": "figure", "inputs": {"count": 2, "size": 0.8}},
        "dots": {"kind": "figure", "inputs": {"count": 4, "color": [0, 0, 1]}},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "sheet"}, {"node": "rails"}, {"node": "dots"}]}},
    }));
    let laser = graph.laser().unwrap();
    assert_eq!(laser.count(), 7);
    assert_eq!(laser.place(0), Some((0, 0)));
    assert_eq!(laser.place(2), Some((1, 1)));
    assert_eq!(laser.place(6), Some((2, 3)));
    assert_eq!(laser.place(7), None);
    // One projector: heads 0 the sheet, 1–2 the rails, 3–6 the dots.
    let cells: Vec<Cell> = projectors(7).into_iter().take(7).collect();
    let rails: Vec<f64> = xs(&figure(&graph, &cells, 1., 1))[1..3].to_vec();
    assert_eq!(rails, [-0.8, 0.8]);
    let dots = figure(&graph, &cells, 1., 2);
    let row = xs(&dots[3..7]);
    for (x, home) in row.iter().zip([-0.5, -1. / 6., 1. / 6., 0.5]) {
        assert!((x - home).abs() < 1e-12, "{row:?}");
    }
    assert!(dots[3..7].iter().all(|(_, s)| s[0].rgb == [0., 0., 1.]));
    let sheet = &figure(&graph, &cells, 1., 0)[0].1[0];
    assert_eq!(sheet.frame, [[0.8, 0.], [0., 0.8]]);
}

/// One space wired into two figures reads each figure's elements as their
/// own row: both fans run end to end.
#[test]
fn a_space_reads_each_figures_elements_apart() {
    let graph = graph(json!({
        "order": {"kind": "space", "settings": {"kind": "order"}},
        "spread": {"kind": "curve", "inputs": {"x": {"node": "order"}, "low": -1, "high": 1}},
        "wide": {"kind": "figure", "inputs": {"count": 4, "x": {"node": "spread"}}},
        "narrow": {"kind": "figure", "inputs": {"count": 2, "x": {"node": "spread"}}},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "wide"}, {"node": "narrow"}]}},
    }));
    let cells: Vec<Cell> = projectors(6).into_iter().take(6).collect();
    assert_eq!(
        xs(&figure(&graph, &cells, 1., 0)[0..4]),
        [-0.75, -0.25, 0.25, 0.75]
    );
    assert_eq!(xs(&figure(&graph, &cells, 1., 1)[4..6]), [-0.5, 0.5]);
}

#[test]
fn a_laser_takes_each_figure_once_and_a_figure_only_feeds_a_laser() {
    let error = |nodes: Value| graph(nodes).check().unwrap_err().0;
    assert_eq!(
        error(json!({"laser1": {"kind": "laser", "inputs": {"figures": []}}})),
        "laser1.figures: expected one or more figure wires; got none. Example: figures=[figure1]"
    );
    assert_eq!(
        error(json!({"laser1": {"kind": "laser"}})),
        "laser1.figures: expected a list of one or more figure wires; got nothing. Example: figures=[figure1, figure2]"
    );
    assert_eq!(
        error(json!({
            "figure1": {"kind": "figure"},
            "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "figure1"}, {"node": "figure1"}]}},
        })),
        "laser1.figures: expected each figure once; got figure1 twice. Example: figures=[figure1, figure2]"
    );
    assert!(error(json!({
        "time1": {"kind": "time"},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "time1"}]}},
    }))
    .starts_with("laser1.figures: expected a list of one or more figure wires; got a coordinate wire from time1"));
    assert!(error(json!({
        "figure1": {"kind": "figure"},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "figure1"}}},
    }))
    .starts_with("color1.brightness: expected"));
}

#[test]
fn a_lasers_figures_draw_at_most_64_elements_together() {
    let message = graph(json!({
        "a": {"kind": "figure", "inputs": {"count": 40}},
        "b": {"kind": "figure", "inputs": {"count": 30}},
        "laser1": {"kind": "laser", "inputs": {"figures": [{"node": "a"}, {"node": "b"}]}},
    }))
    .check()
    .unwrap_err()
    .0;
    assert!(
        message.starts_with("laser1.figures: expected at most 64 elements on each projector, every figure's count together; got 70"),
        "{message}"
    );
}
