//! Product acceptance graphs: the user-facing composition is the computation.
use luma_patterns::*;
use ndarray::{Array3, Axis};
use std::collections::BTreeMap;

fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn cells() -> Vec<Cell> {
    (0..5)
        .rev()
        .map(|n| Cell {
            id: format!("head-{n}"),
            group: "bars".into(),
            world: [0., 0., n as f64],
            uvz: [0., 0., n as f64],
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 8.,
        seed: 427,
    }
}
fn vector(values: &[f64], unit: Unit) -> Binding {
    Value::Signal(Signal::vector(values.to_vec(), unit).unwrap()).into()
}
fn starts(values: &[f64]) -> Binding {
    Value::Events(Events::Beats {
        times: EventTimes::new(values.to_vec()).unwrap(),
    })
    .into()
}
#[derive(Default)]
struct Patch(Graph);
impl Patch {
    fn add<const N: usize>(&mut self, id: &str, definition: &str, inputs: [(&str, Binding); N]) {
        self.0.nodes.insert(
            id.into(),
            Node {
                position: None,
                definition: definition.into(),
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
    }
    fn prepare(mut self, outputs: &[(&str, Binding)], cells: &[Cell]) -> PreparedGraph {
        let mut library = standard_library();
        self.0.outputs = outputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let specs = outputs
            .iter()
            .map(|(k, v)| {
                let (value_type, _) = library.binding_type(&BTreeMap::new(), &self.0, v).unwrap();
                (
                    k.to_string(),
                    Output {
                        value_type,
                        rate: Rate::Frame,
                    },
                )
            })
            .collect();
        let definition = Definition {
            name: "Foundation example".into(),
            inputs: BTreeMap::new(),
            outputs: specs,
            body: Body::Graph(self.0),
        };
        // The same canonical document shape saved by the editor must roundtrip.
        let definition =
            serde_json::from_str(&serde_json::to_string(&definition).unwrap()).unwrap();
        library.definitions.insert("example".into(), definition);
        PreparedGraph::new(&library, "example", &BTreeMap::new(), frame(cells)).unwrap()
    }
    fn motion_output(&mut self, pose: Binding) {
        self.add(
            "pan",
            "core/channel",
            [("value", pose.clone()), ("index", Value::Number(0.).into())],
        );
        self.add(
            "tilt",
            "core/channel",
            [("value", pose), ("index", Value::Number(1.).into())],
        );
        self.add(
            "output",
            "output",
            [
                ("pan", wire("pan", "value")),
                ("tilt", wire("tilt", "value")),
            ],
        );
    }
    fn phase(&mut self, spread: f64) -> Binding {
        self.add("time", "clip_time", []);
        self.add(
            "phase",
            "core/divide",
            [
                ("a", wire("time", "elapsed")),
                ("b", Value::Beats(4.).into()),
            ],
        );
        self.add("mapping", "mapped_position", []);
        self.add(
            "spread",
            "core/multiply",
            [
                ("a", wire("mapping", "value")),
                ("b", Value::Number(spread).into()),
            ],
        );
        self.add(
            "offset",
            "core/add",
            [
                ("a", wire("spread", "value")),
                ("b", wire("phase", "value")),
            ],
        );
        wire("offset", "value")
    }
}

#[test]
fn chase_journeys_keep_independent_lifetimes_through_the_output() {
    let cells = cells();
    for travel in [0.5, 1., 2.] {
        let mut graph = Patch::default();
        graph.add("mapping", "resolve_mapping", []);
        graph.add(
            "chase",
            "chase",
            [
                ("mapping", wire("mapping", "coordinates")),
                ("trigger", starts(&[0., 1.])),
                ("travel", Value::Beats(travel).into()),
                ("width", Value::Proportion(0.2).into()),
                ("shape", Value::Envelope(Envelope::soft_edges(0.)).into()),
            ],
        );
        graph.add(
            "tint",
            "core/multiply",
            [
                ("a", wire("chase", "mask")),
                ("b", Value::Color([1.; 3]).into()),
            ],
        );
        graph.add("output", "output", [("color", wire("tint", "value"))]);
        let program = graph.prepare(&[("lighting", wire("output", "lighting"))], &cells);
        let samples = program.evaluate_batch(&[1., 1.5, 0.75, 3.]).unwrap();
        let lighting = samples["lighting"].lighting().unwrap();
        assert_eq!(lighting.sample(0).unwrap()["head-0"].dimmer, Some(1.));
        assert!(lighting
            .sample(3)
            .unwrap()
            .values()
            .all(|o| o.dimmer == Some(0.)));
        if travel == 0.5 {
            assert!(lighting
                .sample(2)
                .unwrap()
                .values()
                .all(|o| o.dimmer == Some(0.)));
        }
        if travel == 2. {
            let sample = lighting.sample(1).unwrap();
            assert_eq!(sample["head-1"].dimmer, Some(1.));
            assert_eq!(sample["head-3"].dimmer, Some(1.));
        }
    }
}

#[test]
fn triangle_and_ramp_chase_fields_directly_remap_to_pan_tilt_vectors() {
    let cells = cells();
    for (shape, amounts) in [
        (Envelope::soft_edges(1.), [1., 0.25, 0.25, 0., 1.]),
        (
            Envelope::linear(vec![[0., 0.], [1., 1.]]),
            [0.5, 0.875, 0.125, 0., 0.5],
        ),
    ] {
        let mut graph = Patch::default();
        graph.add("mapping", "resolve_mapping", []);
        graph.add(
            "chase",
            "chase",
            [
                ("mapping", wire("mapping", "coordinates")),
                ("trigger", starts(&[0.])),
                ("travel", Value::Beats(4.).into()),
                ("width", Value::Proportion(1.).into()),
                ("shape", Value::Envelope(shape).into()),
            ],
        );
        graph.add(
            "scale",
            "core/multiply",
            [
                ("a", wire("chase", "mask")),
                ("b", vector(&[40., -20.], Unit::Degrees)),
            ],
        );
        graph.add(
            "pose",
            "core/add",
            [
                ("a", wire("scale", "value")),
                ("b", vector(&[10., 20.], Unit::Degrees)),
            ],
        );
        graph.motion_output(wire("pose", "value"));
        let program = graph.prepare(&[("lighting", wire("output", "lighting"))], &cells);
        let result = program.evaluate_batch(&[2., 0.5, 3.5, 4., 2.]).unwrap();
        let lighting = result["lighting"].lighting().unwrap();
        for (t, amount) in amounts.into_iter().enumerate() {
            assert_eq!(
                lighting.sample(t).unwrap()["head-2"].position,
                Some([10. + 40. * amount, 20. - 20. * amount])
            );
        }
    }
}

#[test]
fn shimmer_is_random_head_events_under_an_event_envelope() {
    let library = standard_library();
    let Body::Graph(graph) = &library.definitions["shimmer"].body else {
        panic!()
    };
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.nodes["heads"].definition, "random_head_events");
    assert_eq!(graph.nodes["envelope"].definition, "event_envelope");
    let cells = cells();
    let program = PreparedGraph::new(
        &library,
        "beat_shimmer",
        &BTreeMap::from([
            ("proportion".into(), Value::Proportion(0.4)),
            ("repeat".into(), Value::Beats(1.)),
            ("duration".into(), Value::Beats(2.)),
        ]),
        frame(&cells),
    )
    .unwrap();
    let times = [1.25, 0.25, 2.25, 1.25];
    let batch = program.evaluate_batch(&times).unwrap();
    let signal = batch["color"].signal().unwrap();
    assert_eq!(signal.values().dim(), (5, 4, 3));
    assert!(signal.values().iter().any(|v| *v > 0. && *v < 1.));
    assert!(
        signal
            .values()
            .index_axis(Axis(1), 0)
            .iter()
            .filter(|v| **v > 0.)
            .count()
            > 2,
        "previous selections keep fading"
    );
    for (t, beat) in times.into_iter().enumerate() {
        let single = program.evaluate_batch(&[beat]).unwrap();
        assert_eq!(
            signal.values().index_axis(Axis(1), t),
            single["color"]
                .signal()
                .unwrap()
                .values()
                .index_axis(Axis(1), 0)
        );
    }
}

#[test]
fn phase_offsets_and_vector_transforms_make_a_wave_of_circling_movers() {
    let cells = cells();
    let mut graph = Patch::default();
    let phase = graph.phase(0.5);
    graph.add("circle", "circle", [("phase", phase)]);
    graph.add(
        "scale",
        "core/multiply",
        [
            ("a", wire("circle", "value")),
            ("b", vector(&[20., 10.], Unit::Degrees)),
        ],
    );
    graph.add(
        "pose",
        "core/add",
        [
            ("a", wire("scale", "value")),
            ("b", vector(&[90., 45.], Unit::Degrees)),
        ],
    );
    graph.motion_output(wire("pose", "value"));
    let program = graph.prepare(&[("lighting", wire("output", "lighting"))], &cells);
    let times = [0., 1., 4., 1.];
    let samples = program.evaluate_batch(&times).unwrap();
    for (t, time) in times.into_iter().enumerate() {
        for h in 0..5 {
            let phase = time / 4. + h as f64 / 4. * 0.5;
            let expected = [
                90. + 20. * (phase * std::f64::consts::TAU).sin(),
                45. + 10. * (phase * std::f64::consts::TAU).cos(),
            ];
            let actual = samples["lighting"].lighting().unwrap().sample(t).unwrap()
                [&format!("head-{h}")]
                .position
                .unwrap();
            assert!(actual
                .into_iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 1e-10));
        }
    }
}

#[test]
fn a_wrapped_palette_ribbon_composes_with_independent_intensity() {
    let cells = cells();
    let gradient = Gradient {
        stops: vec![
            ColorStop {
                alpha: 1.,
                t: 0.,
                color: [1., 0., 0.],
            },
            ColorStop {
                alpha: 1.,
                t: 0.5,
                color: [0., 0., 1.],
            },
            ColorStop {
                alpha: 1.,
                t: 1.,
                color: [1., 0., 0.],
            },
        ],
    };
    let mut graph = Patch::default();
    let phase = graph.phase(1.);
    graph.add("wrap", "core/fraction", [("value", phase)]);
    graph.add(
        "palette",
        "sample_gradient",
        [
            ("position", wire("wrap", "value")),
            ("gradient", Value::Gradient(gradient.clone()).into()),
        ],
    );
    graph.add(
        "beat",
        "beat_trigger",
        [("repeat", Value::Beats(1.).into())],
    );
    graph.add(
        "pulse",
        "pulse",
        [
            ("trigger", wire("beat", "trigger")),
            ("duration", Value::Beats(1.).into()),
        ],
    );
    graph.add(
        "lit",
        "core/multiply",
        [
            ("a", wire("palette", "color")),
            ("b", wire("pulse", "mask")),
        ],
    );
    graph.add("output", "output", [("color", wire("lit", "value"))]);
    let program = graph.prepare(&[("lighting", wire("output", "lighting"))], &cells);
    for time in [0., 0.5, 1.5, -0.5, 0.] {
        let output = program.evaluate_batch(&[time]).unwrap();
        let output = output["lighting"].lighting().unwrap().sample(0).unwrap();
        for h in 0..5 {
            let expected = gradient
                .sample((h as f64 / 4. + time.max(0.) / 4.).rem_euclid(1.))
                .map(|v| v * (1. - time.rem_euclid(1.)));
            let actual = output[&format!("head-{h}")].rgb();
            assert!(
                actual
                    .into_iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 1e-8),
                "head {h}, time {time}: {actual:?} != {expected:?}"
            );
        }
    }
}

#[test]
fn the_same_vector_math_transforms_sampled_paths_without_fixture_references() {
    // Samples of a geometric path parameter. This checks numerical portability;
    // a laser still needs an explicit scan-sample domain and device output.
    let samples = Signal::new(
        Array3::from_shape_vec((1, 3, 2), vec![-1., 0., 0., 1., 1., 0.]).unwrap(),
        Unit::Number,
        Channels::components(2).unwrap(),
        None,
    )
    .unwrap();
    let mut graph = Patch::default();
    graph.add(
        "scale",
        "core/multiply",
        [
            ("a", Value::Signal(samples).into()),
            ("b", vector(&[0.5, 0.25], Unit::Number)),
        ],
    );
    graph.add(
        "offset",
        "core/add",
        [
            ("a", wire("scale", "value")),
            ("b", vector(&[0.1, -0.1], Unit::Number)),
        ],
    );
    let program = graph.prepare(&[("path", wire("offset", "value"))], &[]);
    let output = program.evaluate_batch(&[0., 0.5, 1.]).unwrap();
    let signal = output["path"].signal().unwrap();
    assert!(signal.fixtures().is_none());
    let expected = [-0.4, -0.1, 0.1, 0.15, 0.6, -0.1];
    assert!(signal
        .values()
        .iter()
        .zip(expected)
        .all(|(a, b)| (a - b).abs() < 1e-12));
}
