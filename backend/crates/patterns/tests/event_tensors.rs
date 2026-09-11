//! Chase, Pulse and Dissolve are graphs over one event-ages tensor. Events
//! within the window become channels; Max reduces them. No kernel replays a
//! graph per event, and seeking never depends on playback history.
mod support;
use luma_patterns::*;
use ndarray::{array, Array3, Axis};
use std::{collections::BTreeMap, sync::Arc};
#[allow(unused_imports)]
use support::EvaluateEffect;

fn mapping() -> Mapping {
    Mapping::linear((0..5).map(|i| (i.to_string(), i as f64 / 4.0)), false).unwrap()
}
fn events(times: &[f64]) -> Events {
    Events::Beats {
        times: EventTimes::new(times.to_vec()).unwrap(),
    }
}
fn cells(mapping: &Mapping) -> Vec<Cell> {
    mapping
        .coordinates
        .iter()
        .map(|c| Cell {
            id: c.cell.clone(),
            group: "all".into(),
            world: [0., 0., c.position],
            uvz: [0., 0., c.position],
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
        seed: 0,
    }
}
struct Chase<'a> {
    travel: f64,
    start: f64,
    end: f64,
    path: Envelope,
    width: f64,
    shape: Envelope,
    boundary: Boundary,
    mapping: &'a Mapping,
}
impl Default for Chase<'static> {
    fn default() -> Self {
        static MAPPING: std::sync::OnceLock<Mapping> = std::sync::OnceLock::new();
        Self {
            travel: 2.,
            start: 0.,
            end: 1.,
            path: Envelope::linear(vec![[0., 0.], [1., 1.]]),
            width: 0.2,
            shape: Envelope::soft_edges(0.),
            boundary: Boundary::Clip,
            mapping: MAPPING.get_or_init(mapping),
        }
    }
}
impl Chase<'_> {
    fn signal(&self, times: &[f64], events: &Events) -> Signal {
        let cells = cells(self.mapping);
        PreparedGraph::new(
            &standard_library(),
            "chase",
            &BTreeMap::from([
                ("mapping".into(), Value::Coordinates(self.mapping.clone())),
                ("trigger".into(), Value::Events(events.clone())),
                ("travel".into(), Value::Beats(self.travel)),
                ("start".into(), Value::Position(self.start)),
                ("end".into(), Value::Position(self.end)),
                ("path".into(), Value::Envelope(self.path.clone())),
                ("width".into(), Value::Proportion(self.width)),
                ("shape".into(), Value::Envelope(self.shape.clone())),
                ("boundary".into(), Value::Boundary(self.boundary)),
            ]),
            frame(&cells),
        )
        .unwrap()
        .evaluate_batch(times)
        .unwrap()["mask"]
            .signal()
            .unwrap()
            .clone()
    }
}
fn chase(times: &[f64], event_times: &[f64], travel: f64) -> Signal {
    Chase {
        travel,
        ..Chase::default()
    }
    .signal(times, &events(event_times))
}
fn pulse(
    times: &[f64],
    events: &Events,
    clip_start: f64,
    duration: f64,
    shape: &Envelope,
) -> Signal {
    PreparedGraph::new(
        &standard_library(),
        "pulse",
        &BTreeMap::from([
            ("trigger".into(), Value::Events(events.clone())),
            ("duration".into(), Value::Beats(duration)),
            ("shape".into(), Value::Envelope(shape.clone())),
        ]),
        Frame {
            clip_start,
            beat: clip_start,
            ..frame(&[])
        },
    )
    .unwrap()
    .evaluate_batch(times)
    .unwrap()["mask"]
        .signal()
        .unwrap()
        .clone()
}

#[test]
fn independent_journeys_are_a_fixture_time_event_broadcast_and_max() {
    let signal = chase(&[2.5], &[1.0, 2.0], 2.0);
    assert_eq!(signal.values().dim(), (5, 1, 1));
    assert_eq!(
        signal.values().iter().copied().collect::<Vec<_>>(),
        [0., 1., 0., 1., 0.]
    );
    assert_eq!(signal.unit(), Unit::Proportion);
    assert_eq!(signal.fixtures().unwrap(), ["0", "1", "2", "3", "4"]);
}

#[test]
fn equal_short_and_long_travel_have_exact_lifetimes() {
    assert_eq!(chase(&[2.0], &[1., 2.], 1.).values()[[0, 0, 0]], 1.);
    assert_eq!(chase(&[2.0], &[1., 2.], 1.).values()[[4, 0, 0]], 0.);
    assert!(chase(&[1.5, 1.75], &[1., 2.], 0.5)
        .values()
        .iter()
        .all(|v| *v == 0.));
    assert_eq!(chase(&[2.5], &[1., 2.], 2.).values()[[3, 0, 0]], 1.);
    assert!(chase(&[0.99, 4.0], &[1., 2.], 2.)
        .values()
        .iter()
        .all(|v| *v == 0.));
}

#[test]
fn time_axis_is_independent_of_event_axis_and_seek_order() {
    let times = [2.5, 1.0, 3.0, 1.5, 2.5];
    let batch = chase(&times, &[1., 2.], 2.);
    for (index, time) in times.iter().enumerate() {
        let single = chase(&[*time], &[1., 2.], 2.);
        assert_eq!(
            batch.values().index_axis(Axis(1), index),
            single.values().index_axis(Axis(1), 0)
        );
    }
    assert_eq!(chase(&[], &[1., 2.], 2.).values().dim(), (5, 0, 1));
    assert!(chase(&times, &[], 2.).values().iter().all(|v| *v == 0.));
}

#[test]
fn max_preserves_brightness_and_shape_when_events_coincide() {
    let dim = Envelope::linear(vec![[0., 0.6], [1., 0.6]]);
    let result = Chase {
        width: 1.,
        shape: dim.clone(),
        boundary: Boundary::Wrap,
        ..Chase::default()
    }
    .signal(&[1.3], &events(&[1., 1.2]));
    assert!(result.values().iter().all(|v| *v == 0.6));
    let pulse = pulse(&[1.3, 3.2], &events(&[1., 1.2]), 0., 2., &dim);
    assert_eq!(pulse.values(), &array![[[0.6], [0.0]]]);
}

#[test]
fn travel_curve_and_spatial_wrapping_are_independent_of_event_spacing() {
    let result = Chase {
        travel: 1.,
        path: Envelope::linear(vec![[0., 0.], [0.5, 0.25], [1., 1.]]),
        ..Chase::default()
    }
    .signal(&[1.5], &events(&[1.]));
    assert_eq!(result.values()[[1, 0, 0]], 1.);
    assert_eq!(result.values()[[2, 0, 0]], 0.);
    let result = Chase {
        travel: 1.,
        boundary: Boundary::Wrap,
        ..Chase::default()
    }
    .signal(&[1.0], &events(&[1.]));
    assert_eq!(result.values()[[0, 0, 0]], 1.);
    assert_eq!(result.values()[[4, 0, 0]], 1.);
}

#[test]
fn periodic_events_equal_explicit_timestamps_including_delays_and_grid_origin() {
    let times = [2.1, 3.5, 4.8, 6.0];
    let shape = Envelope::linear(vec![[0., 1.], [1., 0.]]);
    let periodic = Events::Periodic {
        repeat: 1.,
        grid_aligned: false,
        delay: 0.5,
    };
    let explicit = events(&[-0.5, 0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5]);
    assert_eq!(
        pulse(&times, &periodic, 2., 2.5, &shape),
        pulse(&times, &explicit, 2., 2.5, &shape)
    );
    let flat = Envelope::soft_edges(0.0);
    assert_eq!(
        pulse(&[2.75, 3.75], &periodic, 2.25, 0.1, &flat).values(),
        &array![[[1.0], [1.0]]]
    );
    let grid = Events::Periodic {
        repeat: 1.,
        grid_aligned: true,
        delay: 0.5,
    };
    assert_eq!(
        pulse(&[2.5, 3.5], &grid, 2.25, 0.1, &flat).values(),
        &array![[[1.0], [1.0]]]
    );
}

#[derive(Debug)]
struct Analysis;
impl FeatureSource for Analysis {
    fn sample(&self, _: &FeatureRequest, _: f64) -> Result<FeatureSample> {
        panic!("the event operation must consume the timestamp tensor, not the latest-event sample")
    }
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        EventTimes::new(vec![1., 2.])
    }
}

#[test]
fn drum_trigger_wires_into_the_same_chase_and_prepared_execution_matches_batch() {
    let mut library = standard_library();
    let wire = |node: &str, output: &str| Binding::Connection {
        node: node.into(),
        output: output.into(),
    };
    let node = |definition: &str, inputs: Vec<(&str, Binding)>| Node {
        position: None,
        definition: definition.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    };
    library.definitions.insert(
        "snare_chase".into(),
        Definition {
            name: "Snare chase".into(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([(
                "lighting".into(),
                Output {
                    value_type: ValueType::Lighting,
                    rate: Rate::Frame,
                },
            )]),
            body: Body::Graph(Graph {
                input_nodes: BTreeMap::new(),
                nodes: BTreeMap::from([
                    (
                        "snare".into(),
                        node(
                            "drum_trigger",
                            vec![("drum", Value::Drum(Drum::Snare).into())],
                        ),
                    ),
                    (
                        "mapping".into(),
                        node(
                            "resolve_mapping",
                            vec![(
                                "mapping",
                                Value::Mapping(MappingSpec {
                                    source: MappingSource::Z,
                                    mirror: None,
                                    per_group: false,
                                    reverse: false,
                                })
                                .into(),
                            )],
                        ),
                    ),
                    (
                        "chase".into(),
                        node(
                            "chase",
                            vec![
                                ("trigger", wire("snare", "trigger")),
                                ("mapping", wire("mapping", "coordinates")),
                                ("travel", Value::Beats(2.).into()),
                                ("width", Value::Proportion(0.2).into()),
                                ("shape", Value::Envelope(Envelope::soft_edges(0.)).into()),
                                ("boundary", Value::Boundary(Boundary::Clip).into()),
                            ],
                        ),
                    ),
                    (
                        "tint".into(),
                        node(
                            "core/multiply",
                            vec![
                                ("a", wire("chase", "mask")),
                                ("b", Value::Color([1.0; 3]).into()),
                            ],
                        ),
                    ),
                    (
                        "output".into(),
                        node("output", vec![("color", wire("tint", "value"))]),
                    ),
                ]),
                outputs: BTreeMap::from([("lighting".into(), wire("output", "lighting"))]),
            }),
        },
    );
    let cells = cells(&mapping());
    let prepared =
        PreparedGraph::new(&library, "snare_chase", &BTreeMap::new(), frame(&cells)).unwrap();
    assert_eq!(
        prepared.feature_requests(),
        [FeatureRequest::Onsets(Drum::Snare)]
    );
    let prepared = prepared.with_features(Arc::new(Analysis)).unwrap();
    for time in [2.5, 1., 4., 2.5] {
        let actual = prepared.evaluate(time).unwrap();
        let expected = chase(&[time], &[1., 2.], 2.);
        let Value::Lighting(output) = &actual["lighting"] else {
            panic!()
        };
        for (index, id) in expected.fixtures().unwrap().iter().enumerate() {
            assert_eq!(output[id].dimmer, Some(expected.values()[[index, 0, 0]]));
        }
    }
}

#[test]
fn signal_broadcasting_preserves_domains_units_and_channels() {
    let ids = Some(vec!["left".into(), "right".into()].into());
    let envelope = Signal::new(
        array![[[0.25], [0.75]]],
        Unit::Proportion,
        Channels::Value,
        None,
    )
    .unwrap();
    let colors = Signal::new(
        array![[[1., 0., 0.]], [[0., 0., 1.]]],
        Unit::Proportion,
        Channels::Rgb,
        ids,
    )
    .unwrap();
    let result = colors
        .zip(&envelope, Unit::Proportion, |a, b| a * b)
        .unwrap();
    assert_eq!(
        result.values(),
        &array![
            [[0.25, 0., 0.], [0.75, 0., 0.]],
            [[0., 0., 0.25], [0., 0., 0.75]]
        ]
    );
    assert_eq!(result.channels(), &Channels::Rgb);
    let foreign = Signal::new(
        Array3::zeros((2, 1, 3)),
        Unit::Proportion,
        Channels::Rgb,
        Some(vec!["right".into(), "left".into()].into()),
    )
    .unwrap();
    assert!(colors.zip(&foreign, Unit::Number, f64::max).is_err());
    let position = Signal::new(
        Array3::zeros((1, 1, 2)),
        Unit::Degrees,
        Channels::PanTilt,
        None,
    )
    .unwrap();
    assert!(colors.zip(&position, Unit::Number, f64::max).is_err());
    assert_eq!(
        serde_json::from_str::<Signal>(&serde_json::to_string(&result).unwrap()).unwrap(),
        result
    );
}

#[test]
fn malformed_event_tensors_and_timing_fail_explicitly() {
    assert!(EventTimes::new(vec![2., 1.]).is_err());
    assert!(EventTimes::new(vec![f64::NAN]).is_err());
    let shape = Envelope::soft_edges(0.);
    let library = standard_library();
    for duration in [0., -1., f64::INFINITY] {
        assert!(PreparedGraph::new(
            &library,
            "pulse",
            &BTreeMap::from([
                ("trigger".into(), Value::Events(events(&[1.]))),
                ("duration".into(), Value::Beats(duration)),
            ]),
            frame(&[]),
        )
        .is_err());
    }
    assert!(Events::Periodic {
        repeat: 0.,
        grid_aligned: false,
        delay: 0.
    }
    .validate()
    .is_err());
    let program = PreparedGraph::new(
        &library,
        "pulse",
        &BTreeMap::from([("trigger".into(), Value::Events(events(&[1.])))]),
        frame(&[]),
    )
    .unwrap();
    assert!(program.evaluate_batch(&[f64::NAN]).is_err());
    assert!(PreparedGraph::new(
        &library,
        "pulse",
        &BTreeMap::new(),
        Frame {
            clip_start: f64::NAN,
            ..frame(&[])
        },
    )
    .is_err());
    // A positive duration/repeat ratio may underflow; the event at this exact
    // sample still contributes its first instant.
    let sparse = Events::Periodic {
        repeat: f64::MAX,
        grid_aligned: true,
        delay: 0.0,
    };
    assert_eq!(
        pulse(&[0.0], &sparse, 0.0, f64::from_bits(1), &shape).values()[[0, 0, 0]],
        1.0
    );
}

#[test]
fn long_span_batches_gather_only_relevant_events() {
    let mapping =
        Mapping::linear((0..512).map(|i| (i.to_string(), i as f64 / 511.0)), false).unwrap();
    let events = events(&(0..40000).map(|i| i as f64).collect::<Vec<_>>());
    let periodic = Events::Periodic {
        repeat: 1.0,
        grid_aligned: true,
        delay: 0.0,
    };
    let times = [39999.5, 2.5, 20000.5, 19998.2];
    let shape = Chase {
        travel: 2.5,
        width: 0.1,
        shape: Envelope::soft_edges(0.1),
        boundary: Boundary::Wrap,
        mapping: &mapping,
        ..Chase::default()
    };
    let explicit = shape.signal(&times, &events);
    assert_eq!(explicit.values().dim(), (512, 4, 1));
    assert_eq!(explicit, shape.signal(&times, &periodic));
    for (t, beat) in times.iter().enumerate() {
        assert_eq!(
            explicit.values().index_axis(Axis(1), t),
            shape
                .signal(&[*beat], &events)
                .values()
                .index_axis(Axis(1), 0)
        );
    }
}

#[test]
fn overlapping_dissolves_match_independent_selections_and_replay_after_seeking() {
    let library = standard_library();
    let cells: Vec<_> = (0..16)
        .rev()
        .map(|i| Cell {
            id: format!("head-{i:02}"),
            group: "bars".into(),
            world: [0.0, 0.0, i as f64],
            uvz: [0.0, 0.0, i as f64],
        })
        .collect();
    let frame = Frame {
        seed: 427,
        clip_duration: 16.,
        ..frame(&cells)
    };
    let times = [4.2, 0.2, 6.0, 3.0, 4.5, 10.0];
    let event_times = [0.0, 1.0, 2.0, 3.0];
    let coverage = Envelope::linear(vec![[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]]);
    let softness = Signal::new(
        Array3::from_shape_fn((cells.len(), 1, 1), |(n, t, _)| ((n + t) % 5) as f64 / 5.0),
        Unit::Proportion,
        Channels::Value,
        Some(
            cells
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>()
                .into(),
        ),
    )
    .unwrap();
    let args = BTreeMap::from([
        ("trigger".into(), Value::Events(events(&event_times))),
        ("duration".into(), Value::Beats(3.2)),
        ("proportion".into(), Value::Envelope(coverage.clone())),
        ("softness".into(), Value::Signal(softness.clone())),
    ]);
    let batch = PreparedGraph::new(&library, "dissolve", &args, frame)
        .unwrap()
        .evaluate_batch(&times)
        .unwrap();
    let actual = batch["mask"].signal().unwrap();
    for (t, time) in times.iter().enumerate() {
        let mut expected: BTreeMap<_, f64> =
            cells.iter().map(|cell| (cell.id.clone(), 0.0)).collect();
        // Independent oracle: the selection graph sampled separately for each
        // event, then Max. Production evaluates the event tensor once.
        for (index, at) in event_times.iter().enumerate() {
            let age = (time - at) / 3.2;
            if !(0.0..1.0).contains(&age) {
                continue;
            }
            let sample = library
                .evaluate(
                    "random_selection",
                    &BTreeMap::from([
                        ("index".into(), Value::Number(index as f64)),
                        ("proportion".into(), Value::Proportion(coverage.sample(age))),
                        ("softness".into(), Value::Signal(softness.clone())),
                    ]),
                    Frame {
                        beat: *time,
                        ..frame
                    },
                )
                .unwrap();
            let mask = &support::field(&sample["selected"]);
            for (id, value) in mask {
                expected.insert(id.clone(), expected[id].max(*value));
            }
        }
        for (n, id) in actual.fixtures().unwrap().iter().enumerate() {
            assert!(
                (actual.values()[[n, t, 0]] - expected[id]).abs() < 1e-12,
                "{id} at {time}"
            );
        }
        let single = PreparedGraph::new(&library, "dissolve", &args, frame)
            .unwrap()
            .evaluate_batch(&[*time])
            .unwrap();
        assert_eq!(
            actual.values().index_axis(Axis(1), t),
            single["mask"]
                .signal()
                .unwrap()
                .values()
                .index_axis(Axis(1), 0)
        );
    }
}

#[test]
fn chase_controls_follow_fixture_identity_across_mapping_order() {
    let cells: Vec<_> = (0..7)
        .rev()
        .map(|i| Cell {
            id: format!("head-{i}"),
            group: "bars".into(),
            world: [0.0, 0.0, i as f64],
            uvz: [0.0, 0.0, i as f64],
        })
        .collect();
    let mapping = Mapping::linear(
        cells.iter().map(|cell| (cell.id.clone(), cell.uvz[2])),
        false,
    )
    .unwrap();
    let ids: Arc<[String]> = cells
        .iter()
        .map(|cell| cell.id.clone())
        .collect::<Vec<_>>()
        .into();
    let start: Vec<_> = (0..7).map(|i| i as f64 * 0.05).collect();
    let end: Vec<_> = (0..7).map(|i| 1.2 - i as f64 * 0.04).collect();
    let width: Vec<_> = (0..7).map(|i| (i % 4) as f64 * 0.1).collect();
    let signal = |values: &[f64], unit| {
        Value::Signal(
            Signal::new(
                Array3::from_shape_vec((7, 1, 1), values.to_vec()).unwrap(),
                unit,
                Channels::Value,
                Some(ids.clone()),
            )
            .unwrap(),
        )
    };
    let path = Envelope::linear(vec![[0.0, 0.0], [1.0, 1.0]]);
    let shape = Envelope::soft_edges(0.1);
    let triggers = events(&[0.0, 1.0, 2.0]);
    let times = [1.6, 0.2, 2.5, 1.6];
    let result = PreparedGraph::new(
        &standard_library(),
        "chase",
        &BTreeMap::from([
            ("mapping".into(), Value::Coordinates(mapping.clone())),
            ("trigger".into(), Value::Events(triggers.clone())),
            ("travel".into(), Value::Beats(2.5)),
            ("start".into(), signal(&start, Unit::Position)),
            ("end".into(), signal(&end, Unit::Position)),
            ("width".into(), signal(&width, Unit::Proportion)),
            ("shape".into(), Value::Envelope(shape.clone())),
            ("path".into(), Value::Envelope(path.clone())),
        ]),
        frame(&cells),
    )
    .unwrap()
    .evaluate_batch(&times)
    .unwrap();
    let result = result["mask"].signal().unwrap();
    for (index, id) in ids.iter().enumerate() {
        // Every head's controls equal a whole-selection run with that head's values.
        let expected = Chase {
            travel: 2.5,
            start: start[index],
            end: end[index],
            width: width[index],
            path: path.clone(),
            shape: shape.clone(),
            boundary: Boundary::Natural,
            mapping: &mapping,
        }
        .signal(&times, &triggers);
        let actual_row = result
            .fixtures()
            .unwrap()
            .iter()
            .position(|key| key == id)
            .unwrap();
        let expected_row = expected
            .fixtures()
            .unwrap()
            .iter()
            .position(|key| key == id)
            .unwrap();
        assert_eq!(
            result.values().index_axis(Axis(0), actual_row),
            expected.values().index_axis(Axis(0), expected_row),
            "wrong controls for {id}"
        );
    }
}
