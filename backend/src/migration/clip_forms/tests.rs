//! Conversions of small graphs shaped like the stored ones. Where the old
//! graph needs no track analysis, both clips render on eight heads in a row
//! and must match.
use super::*;
use luma_patterns::{
    AudioLevel, BlendMode, Cell, Definition, Frame, Key, Library, PreparedGraph, Segment, Selection,
};
use serde_json::{json, Value as Json};

const START: f64 = 8.0;
const DURATION: f64 = 16.0;

fn value(kind: &str, value: Json) -> Json {
    json!({"source": "value", "value": {"type": kind, "value": value}})
}
fn wire(node: &str, output: &str) -> Json {
    json!({"source": "connection", "node": node, "output": output})
}
fn input(name: &str) -> Json {
    json!({"source": "input", "input": name})
}
fn beats(v: f64) -> Json {
    value("beats", json!(v))
}
fn curve(points: Json) -> Json {
    value("envelope", json!({ "points": points }))
}

/// A clip graph with one `color` input, whose output node is `output`.
fn definition(nodes: Json) -> Definition {
    serde_json::from_value(json!({
        "name": "Look",
        "inputs": {"color": {
            "name": "Color",
            "description": "Color",
            "value_type": {"signal": {"unit": "proportion", "channels": "rgb"}},
            "rate": "frame",
            "default": {"type": "color", "value": [1.0, 1.0, 1.0]},
        }},
        "outputs": {"lighting": {"value_type": "lighting", "rate": "frame"}},
        "body": {"kind": "graph", "body": {
            "nodes": nodes,
            "outputs": {"lighting": wire("output", "lighting")},
        }},
    }))
    .expect("definition")
}

/// `color × mask` into the output.
fn colored(mut nodes: Json, mask: Json) -> Json {
    nodes["tint"] =
        json!({"definition": "core/multiply", "inputs": {"a": input("color"), "b": mask}});
    nodes["output"] = json!({"definition": "output", "inputs": {"color": wire("tint", "value")}});
    nodes
}

fn score(nodes: Json) -> Score {
    let mut score = Score::default();
    score.definitions.insert("look".into(), definition(nodes));
    score.clips.insert(
        "clip".into(),
        Clip {
            graph: "look".into(),
            start: START,
            duration: DURATION,
            seed: 3,
            selection_seed: None,
            selection: Selection::all(),
            z_index: 0,
            blend_mode: BlendMode::Replace,
            inputs: BTreeMap::from([("color".into(), Value::Color([1.0, 0.5, 0.25]))]),
        },
    );
    score
}

fn run(score: &Score, host: &Host) -> Result<Converted, String> {
    match convert(score, "clip", &json!({"expression": "all"}), host)? {
        Proposal::Form(converted) => Ok(*converted),
        Proposal::Delete { reason } => Err(format!("delete: {reason}")),
    }
}

/// The one layer under the converted clip.
fn under(converted: &Converted) -> &Clip {
    match converted.layers.as_slice() {
        [layer] if !layer.above => &layer.clip,
        other => panic!("{other:?}"),
    }
}

/// Eight heads in a row along stage X.
fn cells() -> Vec<Cell> {
    (0..8)
        .map(|n| Cell {
            id: format!("bar{n}:0"),
            group: "bar".into(),
            world: [f64::from(n), 0.0, 0.0],
            uvz: [f64::from(n), 0.0, 0.0],
        })
        .collect()
}

/// Emitted light per head, dark outside the clip.
fn light(library: &Library, clip: &Clip, beat: f64) -> Vec<[f64; 3]> {
    let cells = cells();
    if beat < clip.start || beat >= clip.start + clip.duration {
        return vec![[0.0; 3]; cells.len()];
    }
    let prepared = PreparedGraph::new(
        library,
        &clip.graph,
        &clip.inputs,
        Frame {
            cells: &cells,
            features: None,
            beat: clip.start,
            clip_start: clip.start,
            clip_duration: clip.duration,
            seed: clip.seed,
        },
    )
    .expect("prepared");
    let result = prepared.evaluate(beat).expect("evaluated");
    let Value::Lighting(lighting) = &result["lighting"] else {
        panic!("no lighting")
    };
    cells.iter().map(|cell| lighting[&cell.id].rgb()).collect()
}

/// The largest difference over the old clip, sampled every 1/32 beat.
/// Layers over the clip must be white and multiply.
fn difference(score: &Score, converted: &Converted) -> f64 {
    let old = &score.clips["clip"];
    let before = score.library(&standard_library()).unwrap();
    let after = standard_library();
    (0..(DURATION * 32.0) as usize)
        .map(|i| START + i as f64 / 32.0 + 0.003)
        .flat_map(|beat| {
            let a = light(&before, old, beat);
            let mut b = light(&after, &converted.clip, beat);
            for layer in &converted.layers {
                assert!(layer.above && layer.clip.blend_mode == BlendMode::Multiply);
                for (head, top) in b.iter_mut().zip(light(&after, &layer.clip, beat)) {
                    assert!(top.iter().all(|v| (v - top[0]).abs() < 1e-9), "white");
                    head.iter_mut().for_each(|c| *c *= top[0]);
                }
            }
            a.into_iter()
                .zip(b)
                .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
                .collect::<Vec<_>>()
        })
        .fold(0.0, f64::max)
}

fn beat_trigger(repeat: f64, grid_aligned: bool, delay: f64) -> Json {
    json!({"definition": "beat_trigger", "inputs": {
        "repeat": beats(repeat),
        "grid_aligned": value("boolean", json!(grid_aligned)),
        "delay": beats(delay),
    }})
}

fn chase(trigger: &str, mapping: Json, shape: Json) -> Json {
    json!({
        "mapping": {"definition": "resolve_mapping", "inputs": {"mapping": value("mapping", mapping)}},
        "chase": {"definition": "chase", "inputs": {
            "mapping": wire("mapping", "coordinates"),
            "trigger": wire(trigger, "trigger"),
            "travel": beats(1.5),
            "width": value("number", json!(0.25)),
            "shape": shape,
            "path": curve(json!([[0.0, 0.0], [1.0, 1.0]])),
            "start": value("position", json!(0.0)),
            "end": value("position", json!(1.0)),
            "boundary": value("boundary", json!("natural")),
        }},
    })
}

#[test]
fn a_reversed_beat_chase_becomes_a_backward_chase() {
    let mut nodes = chase(
        "events",
        json!({"source": {"kind": "u"}, "per_group": false, "reverse": true}),
        curve(json!([[0.0, 0.0], [0.9, 1.0], [1.0, 0.0]])),
    );
    nodes["events"] = beat_trigger(2.0, false, 0.0);
    let score = score(colored(nodes, wire("chase", "mask")));
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "color.chase@1");
    assert_eq!(converted.look, "chase");
    let Value::Mapping(axis) = &converted.clip.inputs["axis"] else {
        panic!()
    };
    assert!(!axis.reverse, "the axis has no reverse");
    let Value::Envelope(path) = &converted.clip.inputs["path"] else {
        panic!()
    };
    assert!(
        path.points[0][1] > path.points[1][1],
        "the path runs backward"
    );
    assert!(difference(&score, &converted) < 2e-6);
}

#[test]
fn a_pulse_on_the_track_grid_starts_on_its_first_event() {
    let mut nodes = json!({
        "events": beat_trigger(1.0, true, 0.5),
        "pulse": {"definition": "pulse", "inputs": {
            "trigger": wire("events", "trigger"),
            "duration": beats(0.25),
            "shape": curve(json!([[0.0, 1.0], [1.0, 0.0]])),
        }},
    });
    nodes = colored(nodes, wire("pulse", "mask"));
    let score = score(nodes);
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "color.sparkle@1");
    assert_eq!(converted.clip.start, START + 0.5);
    assert_eq!(converted.clip.duration, DURATION - 0.5);
    assert_eq!(converted.clip.inputs["every"], Value::Beats(1.0));
    assert_eq!(converted.clip.inputs["coverage"], Value::Proportion(1.0));
    assert!(matches!(converted.clip.inputs["brightness"], Value::Hit(_)));
    assert!(difference(&score, &converted) < 2e-6);
}

#[test]
fn a_color_fade_becomes_color_over_time() {
    let gradient = json!({"stops": [
        {"t": 0.0, "color": [1.0, 0.0, 0.0]},
        {"t": 1.0, "color": [0.0, 0.2, 1.0]},
    ]});
    let nodes = json!({
        "time": {"definition": "clip_time", "inputs": {}},
        "fade": {"definition": "sample_gradient", "inputs": {
            "gradient": value("gradient", gradient.clone()),
            "position": wire("time", "progress"),
        }},
        "mask": {"definition": "uniform_mask", "inputs": {"coverage": value("proportion", json!(1.0))}},
        "tint": {"definition": "core/multiply", "inputs": {"a": wire("fade", "color"), "b": wire("mask", "mask")}},
        "output": {"definition": "output", "inputs": {"color": wire("tint", "value")}},
    });
    let score = score(nodes);
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "color.time@1");
    assert_eq!(converted.clip.inputs["every"], Value::Beats(0.0));
    assert_eq!(
        converted.clip.inputs["colors"],
        serde_json::from_value::<Value>(json!({"type": "gradient", "value": gradient})).unwrap()
    );
    assert!(converted.notes.is_empty(), "{:?}", converted.notes);
    assert!(difference(&score, &converted) < 2e-6);
}

#[test]
fn alternating_sides_becomes_a_two_step_chase() {
    let nodes = json!({
        "clock": {"definition": "rhythm", "inputs": {
            "delay": beats(0.0),
            "grid_aligned": value("boolean", json!(true)),
            "repeat": beats(2.0),
        }},
        "elapsed": {"definition": "core/divide", "inputs": {"a": wire("clock", "elapsed"), "b": value("number", json!(1.0))}},
        "period": {"definition": "core/divide", "inputs": {"a": beats(2.0), "b": value("number", json!(1.0))}},
        "phase": {"definition": "core/divide", "inputs": {"a": wire("elapsed", "value"), "b": wire("period", "value")}},
        "mapping": {"definition": "mapped_position", "inputs": {"mapping": value("mapping",
            json!({"source": {"kind": "u"}, "per_group": false, "reverse": false}))}},
        "fixture_side": {"definition": "core/greater", "inputs": {"a": wire("mapping", "value"), "b": value("number", json!(0.4999999999)), "tolerance": value("number", json!(0.0))}},
        "beat_side": {"definition": "core/greater", "inputs": {"a": wire("phase", "value"), "b": value("number", json!(0.4999999999)), "tolerance": value("number", json!(0.0))}},
        "offset": {"definition": "core/subtract", "inputs": {"a": wire("fixture_side", "mask"), "b": wire("beat_side", "mask")}},
        "alternate": {"definition": "core/absolute", "inputs": {"value": wire("offset", "value")}},
        "mask": {"definition": "core/clamp_coverage", "inputs": {"value": wire("alternate", "value")}},
    });
    let score = score(colored(nodes, wire("mask", "mask")));
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "color.chase@1");
    assert_eq!(converted.clip.inputs["every"], Value::Beats(2.0));
    assert_eq!(converted.clip.inputs["travel"], Value::Beats(2.0));
    assert_eq!(converted.clip.inputs["width"], Value::Number(0.5));
    assert!(difference(&score, &converted) < 2e-6);
}

#[test]
fn a_drum_chase_stamps_the_hits_inside_the_clip() {
    let mut nodes = chase(
        "snare",
        json!({"source": {"kind": "u"}, "per_group": false, "reverse": false}),
        curve(json!([[0.0, 1.0], [1.0, 1.0]])),
    );
    nodes["snare"] =
        json!({"definition": "drum_trigger", "inputs": {"drum": value("drum", json!("snare"))}});
    let score = score(colored(nodes, wire("chase", "mask")));
    let host = Host {
        onsets: BTreeMap::from([(Drum::Snare, vec![2.0, 9.0, 12.5, 23.0, 30.0])]),
        ..Host::default()
    };
    let converted = run(&score, &host).unwrap();
    assert_eq!(converted.clip.graph, "color.chase@1");
    assert_eq!(converted.clip.start, START);
    let Value::Events(Events::Beats { times }) = &converted.clip.inputs["every"] else {
        panic!("{:?}", converted.clip.inputs["every"])
    };
    assert_eq!(times.as_slice(), &[1.0, 4.5, 15.0]);

    // A hit a hair before the clip that is still lit moves the start back.
    let host = Host {
        onsets: BTreeMap::from([(Drum::Snare, vec![7.97, 9.0])]),
        ..Host::default()
    };
    let converted = run(&score, &host).unwrap();
    assert_eq!(converted.clip.start, 7.97);
    let Value::Events(Events::Beats { times }) = &converted.clip.inputs["every"] else {
        panic!()
    };
    assert_eq!(times.as_slice().len(), 2);
    assert_eq!(times.as_slice()[0], 0.0);

    // So does a hit a whole beat before it, as long as its stroke is lit.
    let host = Host {
        onsets: BTreeMap::from([(Drum::Snare, vec![7.0, 9.0])]),
        ..Host::default()
    };
    let converted = run(&score, &host).unwrap();
    assert_eq!(
        (converted.clip.start, converted.clip.duration),
        (7.0, DURATION + 1.0)
    );
    assert!(converted
        .notes
        .iter()
        .any(|n| n == "start moved back 1.0000 beats onto an event still lit at the old start"));
}

#[test]
fn a_bass_pulse_follows_a_band_of_the_mix() {
    let nodes = json!({
        "bass": {"definition": "band_mask", "inputs": {
            "source": value("audio_source", json!("bass")),
            "low_hz": value("number", json!(20.0)),
            "high_hz": value("number", json!(250.0)),
            "gain": value("number", json!(8.0)),
            "shape": curve(json!([[0.0, 0.2], [1.0, 1.0]])),
        }},
    });
    let score = score(colored(nodes, wire("bass", "mask")));
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "color.constant@1");
    assert_eq!(
        converted.clip.inputs["color"],
        Value::Color([1.0, 0.5, 0.25])
    );
    assert_eq!(
        converted.clip.inputs["alpha"],
        Value::Audio(AudioLevel {
            from_hz: 20.0,
            to_hz: 250.0,
            floor: 0.2,
            threshold: 0.0,
        })
    );
    assert!(converted.notes.iter().any(|n| n == "stem → mix (bass)"));
}

#[test]
fn a_pulse_times_a_clip_curve_moves_the_curve_to_alpha() {
    let nodes = json!({
        "events": beat_trigger(1.0, false, 0.0),
        "pulse": {"definition": "pulse", "inputs": {
            "trigger": wire("events", "trigger"),
            "duration": beats(0.5),
            "shape": curve(json!([[0.0, 1.0], [0.4, 1.0], [1.0, 0.0]])),
        }},
        "time": {"definition": "clip_time", "inputs": {}},
        "phrase": {"definition": "envelope", "inputs": {
            "progress": wire("time", "progress"),
            "shape": curve(json!([[0.0, 0.0], [0.25, 1.0], [1.0, 0.5]])),
        }},
        "level": {"definition": "scale_mask", "inputs": {"mask": wire("pulse", "mask"), "amount": wire("phrase", "value")}},
    });
    let score = score(colored(nodes, wire("level", "mask")));
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "color.sparkle@1");
    let Value::Time(alpha) = &converted.clip.inputs["alpha"] else {
        panic!("{:?}", converted.clip.inputs["alpha"])
    };
    assert_eq!(alpha.points[1], (0.25, Key::Number(1.0)));
    assert_eq!(alpha.segments, vec![Segment::Linear; 2]);
    assert!(difference(&score, &converted) < 2e-6);
}

#[test]
fn a_pulse_times_a_chase_becomes_a_chase_under_a_multiply_pulse() {
    let mut nodes = json!({
        "events": beat_trigger(1.0, false, 0.0),
        "pulse": {"definition": "pulse", "inputs": {
            "trigger": wire("events", "trigger"),
            "duration": beats(0.5),
            "shape": curve(json!([[0.0, 1.0], [1.0, 0.0]])),
        }},
        "both": {"definition": "multiply_mask", "inputs": {"a": wire("pulse", "mask"), "b": wire("chase", "mask")}},
    });
    for (key, node) in chase(
        "events",
        json!({"source": {"kind": "u"}, "per_group": false, "reverse": false}),
        curve(json!([[0.0, 1.0], [1.0, 1.0]])),
    )
    .as_object()
    .unwrap()
    {
        nodes[key] = node.clone();
    }
    let score = score(colored(nodes, wire("both", "mask")));
    let converted = run(&score, &Host::default()).unwrap();
    assert_eq!(converted.look, "chase × pulse (layered)");
    assert_eq!(converted.clip.graph, "color.chase@1");
    assert_eq!(converted.clip.blend_mode, BlendMode::Replace);
    assert_eq!(
        converted.clip.inputs["color"],
        Value::Color([1.0, 0.5, 0.25])
    );
    let [layer] = converted.layers.as_slice() else {
        panic!("{:?}", converted.layers)
    };
    assert!(layer.above);
    assert_eq!(layer.clip.graph, "color.sparkle@1");
    assert_eq!(layer.clip.blend_mode, BlendMode::Multiply);
    assert_eq!(layer.clip.inputs["color"], Value::Color([1.0; 3]));
    assert_eq!(
        (layer.clip.start, layer.clip.duration),
        (converted.clip.start, converted.clip.duration)
    );
    assert!(difference(&score, &converted) < 2e-6);
}

#[test]
fn layers_share_the_earliest_start_and_keep_their_events() {
    // Snare hits drive the chase; a pulse on the beat grid multiplies it.
    // The hit at 7.5 is still lit at the clip start, so both layers start
    // there; the grid pulse keeps its phase as stamps.
    let mut nodes = json!({
        "beat": beat_trigger(1.0, true, 0.0),
        "snare": {"definition": "drum_trigger", "inputs": {"drum": value("drum", json!("snare"))}},
        "pulse": {"definition": "pulse", "inputs": {
            "trigger": wire("beat", "trigger"),
            "duration": beats(0.25),
            "shape": curve(json!([[0.0, 1.0], [1.0, 1.0]])),
        }},
        "both": {"definition": "multiply_mask", "inputs": {"a": wire("pulse", "mask"), "b": wire("chase", "mask")}},
    });
    for (key, node) in chase(
        "snare",
        json!({"source": {"kind": "u"}, "per_group": false, "reverse": false}),
        curve(json!([[0.0, 1.0], [1.0, 1.0]])),
    )
    .as_object()
    .unwrap()
    {
        nodes[key] = node.clone();
    }
    let score = score(colored(nodes, wire("both", "mask")));
    let host = Host {
        onsets: BTreeMap::from([(Drum::Snare, vec![7.5, 12.0])]),
        ..Host::default()
    };
    let converted = run(&score, &host).unwrap();
    let [layer] = converted.layers.as_slice() else {
        panic!()
    };
    assert_eq!(converted.clip.start, 7.5);
    assert_eq!(layer.clip.start, 7.5);
    let Value::Events(Events::Beats { times }) = &converted.clip.inputs["every"] else {
        panic!()
    };
    assert_eq!(times.as_slice(), &[0.0, 4.5]);
    let Value::Events(Events::Beats { times }) = &layer.clip.inputs["every"] else {
        panic!("{:?}", layer.clip.inputs["every"])
    };
    assert_eq!(&times.as_slice()[..3], &[0.5, 1.5, 2.5]);
}

#[test]
fn an_aim_clip_is_deleted() {
    let mut nodes = colored(json!({}), value("proportion", json!(1.0)));
    nodes["output"]["inputs"]["pan"] = value("degrees", json!(10.0));
    let score = score(nodes);
    let proposal = convert(
        &score,
        "clip",
        &json!({"expression": "all"}),
        &Host::default(),
    );
    assert!(
        matches!(proposal, Ok(Proposal::Delete { .. })),
        "{proposal:?}"
    );
}

#[test]
fn a_colored_strobe_becomes_a_color_layer_under_a_strobe() {
    let mut nodes = colored(json!({}), value("proportion", json!(0.5)));
    nodes["output"]["inputs"]["strobe"] = value("proportion", json!(0.9));
    let converted = run(&score(nodes), &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "strobe.constant@1");
    assert_eq!(converted.clip.inputs["rate"], Value::Proportion(0.9));
    assert_eq!(converted.clip.inputs["alpha"], Value::Proportion(1.0));
    let under = under(&converted);
    assert_eq!(under.graph, "color.constant@1");
    assert_eq!(under.inputs["color"], Value::Color([1.0, 0.5, 0.25]));
    assert_eq!(under.inputs["alpha"], Value::Proportion(0.5));
    assert_eq!((under.start, under.duration), (START, DURATION));
}

#[test]
fn a_bass_strobe_gate_becomes_an_audio_threshold() {
    let mut nodes = colored(
        json!({
            "energy": {"definition": "band_energy", "inputs": {
                "source": value("audio_source", json!("mix")),
                "low_hz": value("number", json!(20.0)),
                "high_hz": value("number", json!(60.0))}},
            "sensitivity": {"definition": "remap_field/signals", "inputs": {
                "value": wire("energy", "value"),
                "low": value("number", json!(0.01)),
                "high": value("number", json!(0.3))}},
            "gate": {"definition": "core/greater", "inputs": {
                "a": wire("sensitivity", "value"),
                "b": value("number", json!(0.36)),
                "tolerance": value("number", json!(0.0))}},
            "rate": {"definition": "core/multiply", "inputs": {
                "a": value("proportion", json!(0.9)), "b": wire("gate", "mask")}},
        }),
        wire("sensitivity", "value"),
    );
    nodes["output"]["inputs"]["strobe"] = wire("rate", "value");
    let converted = run(&score(nodes), &Host::default()).unwrap();
    assert_eq!(converted.clip.graph, "strobe.constant@1");
    assert_eq!(converted.clip.inputs["rate"], Value::Proportion(0.9));
    assert_eq!(
        converted.clip.inputs["alpha"],
        Value::Audio(AudioLevel {
            from_hz: 20.0,
            to_hz: 60.0,
            floor: 1.0,
            threshold: 0.36,
        })
    );
    let under = under(&converted);
    assert!(matches!(under.inputs["alpha"], Value::Audio(_)));
}

#[test]
fn old_mappings_become_form_axes() {
    let axis_of = |mapping: Json, host: Host| {
        let mut nodes = chase("events", mapping, curve(json!([[0.0, 1.0], [1.0, 1.0]])));
        nodes["events"] = beat_trigger(2.0, false, 0.0);
        let converted = run(&score(colored(nodes, wire("chase", "mask"))), &host).unwrap();
        let Value::Mapping(axis) = &converted.clip.inputs["axis"] else {
            panic!()
        };
        let Value::Envelope(path) = &converted.clip.inputs["path"] else {
            panic!()
        };
        (axis.clone(), path.points[0][1], converted.notes)
    };

    let (axis, _, notes) = axis_of(
        json!({"source": {"kind": "radial"}, "per_group": true, "reverse": false}),
        Host::default(),
    );
    assert_eq!(axis.source, MappingSource::Radial);
    assert!(!axis.per_group);
    assert_eq!(axis.span, Span::Group);
    assert_eq!(axis.plane, Some(AxisPlane::UpDown));
    assert!(notes
        .iter()
        .any(|n| n == "center: extent middle → centroid"));

    // A solved circle whose heads are unknown still becomes an angle.
    let circle =
        json!({"source": {"kind": "circle", "origin": 0.25}, "per_group": false, "reverse": false});
    let (axis, _, notes) = axis_of(circle.clone(), Host::default());
    assert_eq!(axis.source, MappingSource::Angle);
    assert_eq!(axis.plane, Some(AxisPlane::Auto));
    assert!(notes
        .iter()
        .any(|n| n == "solved circle → angle: its heads are unknown"));

    // On a ring the angle around the centroid reads the heads like the
    // fitted circle, turned: the path takes the turn.
    let ring: Vec<Cell> = (0..8)
        .map(|n| {
            let angle = std::f64::consts::TAU * f64::from(n) / 8.0;
            // Wide enough for the circle fit's 2.5 inlier distance, and not
            // quite flat, which the fit needs.
            let world = [
                20.0 + 10.0 * angle.cos(),
                30.0 + 10.0 * angle.sin(),
                1.0 + 0.01 * f64::from(n % 3),
            ];
            Cell {
                id: format!("ring{n}:0"),
                group: "ring".into(),
                world,
                uvz: Cell::stage_coordinates(world),
            }
        })
        .collect();
    let (axis, _, notes) = axis_of(
        circle,
        Host {
            cells: ring,
            ..Host::default()
        },
    );
    assert_eq!(axis.source, MappingSource::Angle);
    assert_eq!(axis.plane, Some(AxisPlane::Auto));
    assert!(
        notes
            .iter()
            .any(|n| n == "solved circle → angle around the best-fit plane"),
        "{notes:?}"
    );
}
