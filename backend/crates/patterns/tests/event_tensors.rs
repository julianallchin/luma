mod support;
use luma_patterns::*;
use ndarray::{array, Array3};
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
fn chase(times: &[f64], event_times: &[f64], travel: f64) -> Signal {
    chase_signal(
        &mapping(),
        times,
        &events(event_times),
        0.0,
        ChaseShape {
            travel,
            start: 0.0,
            end: 1.0,
            path: &Envelope::linear(vec![[0., 0.], [1., 1.]]),
            width: 0.2,
            shape: &Envelope::soft_edges(0.0),
            boundary: Boundary::Clip,
        },
    )
    .unwrap()
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
            batch.values().index_axis(ndarray::Axis(1), index),
            single.values().index_axis(ndarray::Axis(1), 0)
        );
    }
    assert_eq!(chase(&[], &[1., 2.], 2.).values().dim(), (5, 0, 1));
    assert!(chase(&times, &[], 2.).values().iter().all(|v| *v == 0.));
}

#[test]
fn max_preserves_brightness_and_shape_when_events_coincide() {
    let dim = Envelope::linear(vec![[0., 0.6], [1., 0.6]]);
    let result = chase_signal(
        &mapping(),
        &[1.3],
        &events(&[1., 1.2]),
        0.0,
        ChaseShape {
            travel: 2.,
            start: 0.,
            end: 1.,
            path: &Envelope::linear(vec![[0., 0.], [1., 1.]]),
            width: 1.,
            shape: &dim,
            boundary: Boundary::Wrap,
        },
    )
    .unwrap();
    assert!(result.values().iter().all(|v| *v == 0.6));
    let pulse = pulse_signal(&[1.3, 3.2], &events(&[1., 1.2]), 0., 2., &dim).unwrap();
    assert_eq!(pulse.values(), &array![[[0.6], [0.0]]]);
}

#[test]
fn travel_curve_and_spatial_wrapping_are_independent_of_event_spacing() {
    let result = chase_signal(
        &mapping(),
        &[1.5],
        &events(&[1.]),
        0.,
        ChaseShape {
            travel: 1.,
            start: 0.,
            end: 1.,
            path: &Envelope::linear(vec![[0., 0.], [0.5, 0.25], [1., 1.]]),
            width: 0.2,
            shape: &Envelope::soft_edges(0.),
            boundary: Boundary::Clip,
        },
    )
    .unwrap();
    assert_eq!(result.values()[[1, 0, 0]], 1.);
    assert_eq!(result.values()[[2, 0, 0]], 0.);
    let result = chase_signal(
        &mapping(),
        &[1.0],
        &events(&[1.]),
        0.,
        ChaseShape {
            travel: 1.,
            start: 0.,
            end: 1.,
            path: &Envelope::linear(vec![[0., 0.], [1., 1.]]),
            width: 0.2,
            shape: &Envelope::soft_edges(0.),
            boundary: Boundary::Wrap,
        },
    )
    .unwrap();
    assert_eq!(result.values()[[0, 0, 0]], 1.);
    assert_eq!(result.values()[[4, 0, 0]], 1.);
}

#[test]
fn tensor_samples_preserve_existing_pill_edges_curves_and_wrapping() {
    let library = standard_library();
    let mut mapping = mapping();
    let cells: Vec<_> = mapping
        .coordinates
        .iter()
        .map(|coordinate| Cell {
            id: coordinate.cell.clone(),
            group: "all".into(),
            world: [0., 0., coordinate.position],
            uvz: [0., 0., coordinate.position],
        })
        .collect();
    let frame = Frame {
        cells: &cells,
        features: None,
        beat: 1.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    };
    for closed in [false, true] {
        for coordinate in &mut mapping.coordinates {
            coordinate.closed = closed;
        }
        for boundary in [Boundary::Clip, Boundary::Wrap, Boundary::Natural] {
            for width in [0., 0.1, 0.5, 1.] {
                for center in [-0.5, 0., 0.173, 0.5, 1., 1.5] {
                    for shape in [
                        Envelope::soft_edges(0.),
                        Envelope::soft_edges(0.1),
                        Envelope::linear(vec![[0., 0.2], [1., 1.]]),
                    ] {
                        let reference = library
                            .evaluate(
                                "pill",
                                &BTreeMap::from([
                                    ("mapping".into(), Value::Coordinates(mapping.clone())),
                                    ("position".into(), Value::Position(center)),
                                    ("width".into(), Value::Proportion(width)),
                                    ("shape".into(), Value::Envelope(shape.clone())),
                                    ("boundary".into(), Value::Boundary(boundary)),
                                ]),
                                frame,
                            )
                            .unwrap();
                        let reference = &support::field(&reference["mask"]);
                        let tensor = chase_signal(
                            &mapping,
                            &[1.],
                            &events(&[1.]),
                            0.,
                            ChaseShape {
                                travel: 1.,
                                start: center,
                                end: center,
                                path: &Envelope::linear(vec![[0., 0.], [1., 1.]]),
                                width,
                                shape: &shape,
                                boundary,
                            },
                        )
                        .unwrap();
                        for (index, coordinate) in mapping.coordinates.iter().enumerate() {
                            assert!((tensor.values()[[index, 0, 0]] - reference[&coordinate.cell]).abs() < 1e-10,
                                "center={center}, width={width}, boundary={boundary:?}, fixture={index}");
                        }
                    }
                }
            }
        }
    }
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
        pulse_signal(&times, &periodic, 2., 2.5, &shape).unwrap(),
        pulse_signal(&times, &explicit, 2., 2.5, &shape).unwrap()
    );
    let flat = Envelope::soft_edges(0.0);
    assert_eq!(
        pulse_signal(&[2.75, 3.75], &periodic, 2.25, 0.1, &flat)
            .unwrap()
            .values(),
        &array![[[1.0], [1.0]]]
    );
    let grid = Events::Periodic {
        repeat: 1.,
        grid_aligned: true,
        delay: 0.5,
    };
    assert_eq!(
        pulse_signal(&[2.5, 3.5], &grid, 2.25, 0.1, &flat)
            .unwrap()
            .values(),
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
    let mut graph = library.definitions["chase"].clip_instance("chase").unwrap();
    graph.inputs.remove("trigger");
    let Body::Graph(body) = &mut graph.body else {
        panic!()
    };
    body.nodes.insert(
        "snare".into(),
        Node {
            position: None,
            definition: "drum_trigger".into(),
            inputs: BTreeMap::from([("drum".into(), Value::Drum(Drum::Snare).into())]),
        },
    );
    body.nodes.get_mut("effect").unwrap().inputs.insert(
        "trigger".into(),
        Binding::Connection {
            node: "snare".into(),
            output: "trigger".into(),
        },
    );
    library.definitions.insert("snare_chase".into(), graph);
    let cells: Vec<_> = (0..5)
        .map(|i| Cell {
            id: i.to_string(),
            group: "all".into(),
            world: [0., 0., i as f64],
            uvz: [0., 0., i as f64],
        })
        .collect();
    let frame = Frame {
        cells: &cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 8.,
        seed: 0,
    };
    let input = BTreeMap::from([
        ("travel".into(), Value::Beats(2.)),
        ("width".into(), Value::Proportion(0.2)),
        ("shape".into(), Value::Envelope(Envelope::soft_edges(0.))),
    ]);
    let prepared = PreparedGraph::new(&library, "snare_chase", &input, frame).unwrap();
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
    for duration in [0., -1., f64::INFINITY] {
        assert!(pulse_signal(&[1.], &events(&[1.]), 0., duration, &shape).is_err());
    }
    assert!(Events::Periodic {
        repeat: 0.,
        grid_aligned: false,
        delay: 0.
    }
    .validate()
    .is_err());
    assert!(pulse_signal(&[f64::NAN], &events(&[1.]), 0., 1., &shape).is_err());
    assert!(pulse_signal(&[], &events(&[1.]), f64::NAN, 1., &shape).is_err());
    // A positive travel/repeat ratio may underflow; the event at this exact
    // sample still contributes its first instant.
    let sparse = Events::Periodic {
        repeat: f64::MAX,
        grid_aligned: true,
        delay: 0.0,
    };
    assert_eq!(
        pulse_signal(&[0.0], &sparse, 0.0, f64::from_bits(1), &shape)
            .unwrap()
            .values()[[0, 0, 0]],
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
    let shape = ChaseShape {
        travel: 2.5,
        start: 0.0,
        end: 1.0,
        path: &Envelope::linear(vec![[0.0, 0.0], [1.0, 1.0]]),
        width: 0.1,
        shape: &Envelope::soft_edges(0.1),
        boundary: Boundary::Wrap,
    };
    let explicit = chase_signal(&mapping, &times, &events, 0.0, shape).unwrap();
    assert_eq!(explicit.values().dim(), (512, 4, 1));
    assert_eq!(
        explicit,
        chase_signal(&mapping, &times, &periodic, 0.0, shape).unwrap()
    );
    for (t, beat) in times.iter().enumerate() {
        assert_eq!(
            explicit.values().index_axis(ndarray::Axis(1), t),
            chase_signal(&mapping, &[*beat], &events, 0.0, shape)
                .unwrap()
                .values()
                .index_axis(ndarray::Axis(1), 0)
        );
    }
}

#[test]
fn overlapping_dissolves_match_independent_threshold_fields_and_replay_after_seeking() {
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
        features: None,
        cells: &cells,
        beat: 0.0,
        clip_start: 0.0,
        clip_duration: 16.0,
        seed: 427,
    };
    let times = [4.2, 0.2, 6.0, 3.0, 4.5, 10.0];
    let event_times = [0.0, 1.0, 2.0, 3.0];
    let shape = Envelope::linear(vec![[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]]);
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
    for reseed in [false, true] {
        for refresh in [false, true] {
            let args = BTreeMap::from([
                ("trigger".into(), Value::Events(events(&event_times))),
                ("travel".into(), Value::Beats(3.2)),
                ("shape".into(), Value::Envelope(shape.clone())),
                ("softness".into(), Value::Signal(softness.clone())),
                ("reseed".into(), Value::Boolean(reseed)),
                ("refresh".into(), Value::Boolean(refresh)),
                ("refresh_every".into(), Value::Beats(0.5)),
            ]);
            let batch = PreparedGraph::new(&library, "core/dissolve_events", &args, frame)
                .unwrap()
                .evaluate_batch(&times)
                .unwrap();
            let actual = batch["mask"].signal().unwrap();
            for (t, time) in times.iter().enumerate() {
                let mut expected: BTreeMap<_, f64> =
                    cells.iter().map(|cell| (cell.id.clone(), 0.0)).collect();
                // Independent test oracle: the established numerical threshold
                // graph, sampled separately for each event, then Max. Production
                // evaluates the event tensor once and never replays this graph.
                for (index, at) in event_times.iter().enumerate() {
                    let age = (time - at) / 3.2;
                    if !(0.0..1.0).contains(&age) {
                        continue;
                    }
                    let sample = library
                        .evaluate(
                            "dissolve_mask",
                            &BTreeMap::from([
                                ("cycle".into(), Value::Number(index as f64)),
                                ("coverage".into(), Value::Proportion(shape.sample(age))),
                                ("softness".into(), Value::Signal(softness.clone())),
                                ("reseed".into(), Value::Boolean(reseed)),
                                ("refresh".into(), Value::Boolean(refresh)),
                                ("refresh_every".into(), Value::Beats(0.5)),
                            ]),
                            Frame {
                                beat: *time,
                                ..frame
                            },
                        )
                        .unwrap();
                    let mask = &support::field(&sample["mask"]);
                    for (id, value) in mask {
                        expected.insert(id.clone(), expected[id].max(*value));
                    }
                }
                for (n, id) in actual.fixtures().unwrap().iter().enumerate() {
                    assert!(
                        (actual.values()[[n, t, 0]] - expected[id]).abs() < 1e-12,
                        "{id} at {time}, reseed={reseed}, refresh={refresh}"
                    );
                }
                let mut single_args = args.clone();
                single_args.insert("softness".into(), Value::Signal(softness.clone()));
                let single =
                    PreparedGraph::new(&library, "core/dissolve_events", &single_args, frame)
                        .unwrap()
                        .evaluate_batch(&[*time])
                        .unwrap();
                assert_eq!(
                    actual.values().index_axis(ndarray::Axis(1), t),
                    single["mask"]
                        .signal()
                        .unwrap()
                        .values()
                        .index_axis(ndarray::Axis(1), 0)
                );
            }
        }
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
        "core/chase_events",
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
        Frame {
            features: None,
            cells: &cells,
            beat: 0.0,
            clip_start: 0.0,
            clip_duration: 8.0,
            seed: 0,
        },
    )
    .unwrap()
    .evaluate_batch(&times)
    .unwrap();
    let result = result["mask"].signal().unwrap();
    for (index, id) in ids.iter().enumerate() {
        let expected = chase_signal(
            &mapping,
            &times,
            &triggers,
            0.0,
            ChaseShape {
                travel: 2.5,
                start: start[index],
                end: end[index],
                width: width[index],
                path: &path,
                shape: &shape,
                boundary: Boundary::Natural,
            },
        )
        .unwrap();
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
            result.values().index_axis(ndarray::Axis(0), actual_row),
            expected.values().index_axis(ndarray::Axis(0), expected_row),
            "wrong controls for {id}"
        );
    }
}
