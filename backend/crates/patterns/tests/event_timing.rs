use luma_patterns::*;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
struct Analysis(Arc<TrackTiming>);
impl FeatureSource for Analysis {
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        Err(Error("no drums".into()))
    }
    fn sample(&self, request: &FeatureRequest, _: f64) -> Result<FeatureSample> {
        if request == &FeatureRequest::Timing {
            Ok(FeatureSample::Timing(self.0.clone()))
        } else {
            Err(Error("timing only".into()))
        }
    }
}
fn analysis() -> Arc<Analysis> {
    let times = vec![-1., 0., 0.5, 1.25, 2.25];
    Arc::new(Analysis(Arc::new(
        TrackTiming::new(
            BeatTimeline::new(times.clone(), 0.5).unwrap(),
            EventTimes::new(times).unwrap(),
            EventTimes::new(vec![0., 2.25]).unwrap(),
            120.,
        )
        .unwrap(),
    )))
}
fn frame() -> Frame<'static> {
    Frame {
        beat: 0.,
        clip_start: 0.,
        clip_duration: 8.,
        cells: &[],
        seed: 0,
        features: None,
    }
}
fn recorded(source: &Analysis, seconds: &[f32]) -> Value {
    Value::Events(Events::Beats {
        times: EventTimes::new(
            seconds
                .iter()
                .map(|s| source.0.clock.beat_at(f64::from(*s)).unwrap())
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    })
}
fn program(id: &str, args: BTreeMap<String, Value>, source: Arc<Analysis>) -> PreparedGraph {
    PreparedGraph::new(&standard_library(), id, &args, frame())
        .unwrap()
        .with_features(source)
        .unwrap()
}
#[test]
fn track_time_keeps_clip_metadata_fixed_while_seconds_follow_variable_tempo() {
    let source = analysis();
    let mut graph = Graph::default();
    graph.nodes.insert(
        "time".into(),
        Node {
            definition: "core/track_time".into(),
            inputs: BTreeMap::new(),
            position: None,
        },
    );
    let library = standard_library();
    for key in ["clip_start", "clip_duration", "bpm"] {
        let (_, rate) = library
            .binding_type(
                &BTreeMap::new(),
                &graph,
                &Binding::Connection {
                    node: "time".into(),
                    output: key.into(),
                },
            )
            .unwrap();
        assert_eq!(rate, Rate::Fixed);
    }
    let program = program("core/track_time", BTreeMap::new(), source.clone());
    let beats = [0., 2., -1., 2.];
    let output = program.evaluate_batch(&beats).unwrap();
    assert_eq!(
        output["seconds"].signal().unwrap().values().dim(),
        (1, 4, 1)
    );
    for (i, beat) in beats.into_iter().enumerate() {
        assert_eq!(
            output["seconds"].signal().unwrap().values()[[0, i, 0]],
            source.0.clock.seconds_at(beat).unwrap()
        );
    }
    for (key, value) in [
        ("clip_start", 0.5),
        (
            "clip_duration",
            source.0.clock.seconds_at(8.).unwrap() - 0.5,
        ),
        ("bpm", 120.),
    ] {
        let signal = output[key].signal().unwrap();
        assert_eq!(signal.values().dim(), (1, 1, 1));
        assert_eq!(signal.values()[[0, 0, 0]], value);
    }
}
#[test]
fn event_preparation_uses_seconds_across_tempo_changes_and_retains_greedy_spacing() {
    let source = analysis();
    let events = recorded(&source, &[0., 0.01, 0.024, 0.026, 0.04, 1., 2.]);
    let thin = program(
        "core/thin_events",
        BTreeMap::from([
            ("events".into(), events),
            ("minimum".into(), Value::Seconds(0.025)),
        ]),
        source.clone(),
    );
    assert_eq!(thin.dynamic_step_count(), 0);
    let events = thin.evaluate(100.).unwrap()["events"].clone();
    assert_eq!(events, recorded(&source, &[0., 0.026, 1., 2.]));
    let gap = program(
        "core/event_spacing",
        BTreeMap::from([
            ("events".into(), events),
            ("minimum".into(), Value::Seconds(0.03)),
        ]),
        source,
    );
    assert_eq!(gap.dynamic_step_count(), 0);
    assert!(
        (gap.evaluate(0.).unwrap()["spacing"].scalar_value().unwrap() - f64::from(1_f32 - 0.026))
            .abs()
            < 1e-9
    );
}
#[test]
fn recent_events_are_indexed_at_each_requested_time_including_boundaries_and_empty_queries() {
    let source = analysis();
    let event_seconds = [0_f32, 0.5, 0.525, 0.55, 2.];
    let seconds = [3., -0.1, 0.5, 0.54, 0.5];
    let mut library = standard_library();
    let mut graph = Graph::default();
    graph
        .nodes
        .insert("clock".into(), node("core/track_time", []));
    graph.nodes.insert(
        "events".into(),
        node(
            "core/event_window",
            [
                ("events", recorded(&source, &event_seconds).into()),
                ("time", wire("clock", "seconds")),
                ("count", Value::Number(2.).into()),
            ],
        ),
    );
    for key in ["times", "present", "index", "weights"] {
        graph.outputs.insert(key.into(), wire("events", key));
    }
    library.definitions.insert(
        "test".into(),
        Definition {
            name: "Event window".into(),
            inputs: BTreeMap::new(),
            outputs: library.definitions["core/event_window"].outputs.clone(),
            body: Body::Graph(graph),
        },
    );
    let prepared = PreparedGraph::new(&library, "test", &BTreeMap::new(), frame())
        .unwrap()
        .with_features(source.clone())
        .unwrap();
    let beats: Vec<_> = seconds
        .iter()
        .map(|seconds| source.0.clock.beat_at(*seconds).unwrap())
        .collect();
    let result = prepared.evaluate_batch(&beats).unwrap();
    let times = result["times"].signal().unwrap().values();
    let present = result["present"].signal().unwrap().values();
    assert_eq!(
        result["present"].signal().unwrap(),
        result["weights"].signal().unwrap()
    );
    let expected = [[2., 0.55], [0., 0.], [0.5, 0.], [0.525, 0.5], [0.5, 0.]];
    for (t, pair) in expected.iter().enumerate() {
        for c in 0..2 {
            assert_eq!(times[[0, t, c]], f64::from(pair[c] as f32));
            assert_eq!(present[[0, t, c]], if t == 1 { 0. } else { 1. });
        }
    }
    let empty = prepared.evaluate_batch(&[]).unwrap();
    assert_eq!(empty["times"].signal().unwrap().values().dim(), (1, 0, 2));
    for (index, beat) in beats.into_iter().enumerate() {
        assert_eq!(
            prepared.evaluate(beat).unwrap()["times"],
            prepared.evaluate_batch(&[beat]).unwrap()["times"]
                .sample(0)
                .unwrap()
        );
        assert_eq!(
            prepared.evaluate(beat).unwrap()["times"],
            result["times"].sample(index).unwrap()
        );
    }
}
fn node(definition: &str, inputs: impl IntoIterator<Item = (&'static str, Binding)>) -> Node {
    Node {
        definition: definition.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        position: None,
    }
}
fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
#[test]
fn computed_constants_can_configure_events_but_time_varying_wires_cannot() {
    let mut library = standard_library();
    let mut graph = Graph::default();
    graph.nodes.insert(
        "rate".into(),
        node(
            "core/add",
            [
                ("a", Value::Number(1.).into()),
                ("b", Value::Number(1.).into()),
            ],
        ),
    );
    graph.nodes.insert(
        "events".into(),
        node("core/grid_events", [("subdivision", wire("rate", "value"))]),
    );
    graph
        .outputs
        .insert("events".into(), wire("events", "events"));
    assert_eq!(
        library
            .binding_type(&BTreeMap::new(), &graph, &wire("events", "events"))
            .unwrap(),
        (ValueType::Events, Rate::Fixed)
    );
    let mut definition = Definition {
        name: "Computed rhythm".into(),
        inputs: BTreeMap::new(),
        outputs: BTreeMap::from([(
            "events".into(),
            Output {
                value_type: ValueType::Events,
                rate: Rate::Fixed,
            },
        )]),
        body: Body::Graph(graph.clone()),
    };
    library
        .definitions
        .insert("test".into(), definition.clone());
    library.validate("test").unwrap();
    let p = PreparedGraph::new(&library, "test", &BTreeMap::new(), frame())
        .unwrap()
        .with_features(analysis())
        .unwrap();
    assert_eq!(p.dynamic_step_count(), 0);
    graph.nodes.insert("clock".into(), node("clip_time", []));
    graph
        .nodes
        .get_mut("rate")
        .unwrap()
        .inputs
        .insert("a".into(), wire("clock", "progress"));
    definition.body = Body::Graph(graph);
    library.definitions.insert("test".into(), definition);
    assert!(library
        .validate("test")
        .unwrap_err()
        .0
        .contains("frame-varying"));
}

#[test]
fn recent_event_weights_address_each_head_and_each_requested_event() {
    let source = analysis();
    let events = Events::Beats {
        times: EventTimes::new(vec![0., 2., 4.]).unwrap(),
    }
    .targeted(
        EventTargets::weights(
            vec!["b".into(), "a".into()],
            ndarray::array![[0., 0.5, 1.], [1., 0., 0.25]],
        )
        .unwrap(),
    )
    .unwrap();
    let cells: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| Cell {
            id: id.into(),
            group: "all".into(),
            uvz: [0.; 3],
            world: [0.; 3],
        })
        .collect();
    let mut library = standard_library();
    let mut graph = Graph::default();
    graph
        .nodes
        .insert("time".into(), node("core/track_time", []));
    graph.nodes.insert(
        "events".into(),
        node(
            "core/event_window",
            [
                ("events", Value::Events(events).into()),
                ("time", wire("time", "seconds")),
                ("count", Value::Number(3.).into()),
            ],
        ),
    );
    graph
        .outputs
        .insert("weights".into(), wire("events", "weights"));
    library.definitions.insert(
        "test".into(),
        Definition {
            name: "Target query".into(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([(
                "weights".into(),
                library.definitions["core/event_window"].outputs["weights"].clone(),
            )]),
            body: Body::Graph(graph),
        },
    );
    let program = PreparedGraph::new(
        &library,
        "test",
        &BTreeMap::new(),
        Frame {
            cells: &cells,
            ..frame()
        },
    )
    .unwrap()
    .with_features(source)
    .unwrap();
    let times = [3., -1., 6., 0., 3.];
    let batch = program.evaluate_batch(&times).unwrap();
    let signal = batch["weights"].signal().unwrap();
    assert_eq!(signal.values().dim(), (2, 5, 3));
    assert_eq!(signal.fixtures().unwrap(), ["a", "b"]);
    let expected = [
        [
            [0., 1., 0.],
            [0., 0., 0.],
            [0.25, 0., 1.],
            [1., 0., 0.],
            [0., 1., 0.],
        ],
        [
            [0.5, 0., 0.],
            [0., 0., 0.],
            [1., 0.5, 0.],
            [0., 0., 0.],
            [0.5, 0., 0.],
        ],
    ];
    for (n, expected) in expected.iter().enumerate() {
        for (t, time) in times.iter().enumerate() {
            let single = program.evaluate_batch(&[*time]).unwrap();
            for (c, expected) in expected[t].iter().enumerate() {
                assert_eq!(signal.values()[[n, t, c]], *expected);
                assert_eq!(
                    signal.values()[[n, t, c]],
                    single["weights"].signal().unwrap().values()[[n, 0, c]]
                );
            }
        }
    }
    assert_eq!(
        program.evaluate_batch(&[]).unwrap()["weights"]
            .signal()
            .unwrap()
            .values()
            .dim(),
        (2, 0, 3)
    );
}
#[test]
fn seconds_are_distinct_from_beats_and_roundtrip_as_authored_values() {
    let value = Value::Seconds(-0.125);
    assert_eq!(
        serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(),
        value
    );
    assert!(!ValueType::Beats.accepts(value.value_type()));
    assert_eq!(
        value.value_type().to_string(),
        "Signal (seconds, one channel)"
    );
    let source = analysis();
    let events = Value::Events(Events::Beats {
        times: EventTimes::new(vec![1e200, 2e200]).unwrap(),
    });
    let prepared = PreparedGraph::new(
        &standard_library(),
        "core/event_spacing",
        &BTreeMap::from([("events".into(), events)]),
        frame(),
    )
    .unwrap();
    assert!(prepared
        .with_features(source)
        .unwrap_err()
        .0
        .contains("seconds precision"));
}

#[test]
fn a_constant_graph_output_can_be_animated_by_later_edits() {
    let library = standard_library();
    let mut graph = Graph::default();
    graph.nodes.insert("sum".into(), node("core/add", []));
    let mut definition = Definition {
        name: "Editable output".into(),
        inputs: BTreeMap::new(),
        outputs: BTreeMap::new(),
        body: Body::Graph(graph),
    };
    definition
        .edit(
            &library,
            GraphEdit::Output {
                key: "value".into(),
                binding: wire("sum", "value"),
            },
        )
        .unwrap();
    assert_eq!(definition.outputs["value"].rate, Rate::Frame);
    definition
        .edit(
            &library,
            GraphEdit::Add {
                id: "clock".into(),
                definition: "clip_time".into(),
            },
        )
        .unwrap();
    definition
        .edit(
            &library,
            GraphEdit::Bind {
                node: "sum".into(),
                input: "a".into(),
                binding: Some(wire("clock", "progress")),
            },
        )
        .unwrap();
    let Body::Graph(graph) = &definition.body else {
        unreachable!()
    };
    assert_eq!(
        library
            .binding_type(&definition.inputs, graph, &graph.outputs["value"])
            .unwrap()
            .1,
        Rate::Frame
    );
}
