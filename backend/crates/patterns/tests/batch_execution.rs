use luma_patterns::*;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn cells() -> Vec<Cell> {
    // Identity order deliberately differs from spatial/selection order.
    (0..16)
        .rev()
        .map(|i| Cell {
            id: format!("head-{i}"),
            group: "bars".into(),
            world: [i as f64, (i % 3) as f64, (i % 5) as f64],
            uvz: [i as f64, (i % 3) as f64, (i % 5) as f64],
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 2.0,
        clip_start: 2.0,
        clip_duration: 8.0,
        seed: 427,
    }
}

#[derive(Debug)]
struct EventAnalysis {
    times: Vec<f64>,
    reads: AtomicUsize,
}
impl FeatureSource for EventAnalysis {
    fn sample(&self, _: &FeatureRequest, _: f64) -> Result<FeatureSample> {
        Err(Error("event-only analysis".into()))
    }
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        EventTimes::new(self.times.clone())
    }
}

#[test]
fn binding_analysis_prepares_immutable_events_and_rebinding_replaces_them() {
    let library = standard_library();
    let source = |times| {
        Arc::new(EventAnalysis {
            times,
            reads: AtomicUsize::new(0),
        })
    };
    let first = source(vec![1., 2.]);
    let second = source(vec![3., 4.]);
    let prepared =
        PreparedGraph::new(&library, "drum_trigger", &BTreeMap::new(), frame(&[])).unwrap();
    assert_eq!(prepared.dynamic_step_count(), 1);
    let prepared = prepared.with_features(first.clone()).unwrap();
    assert_eq!(prepared.dynamic_step_count(), 0);
    let expected = |times| {
        Value::Events(Events::Beats {
            times: EventTimes::new(times).unwrap(),
        })
    };
    for beat in [0., 100., -10., 0.] {
        assert_eq!(
            prepared.evaluate(beat).unwrap()["trigger"],
            expected(vec![1., 2.])
        );
    }
    assert_eq!(first.reads.load(Ordering::SeqCst), 1);
    let rebound = prepared.clone().with_features(second.clone()).unwrap();
    assert_eq!(
        rebound.evaluate(0.).unwrap()["trigger"],
        expected(vec![3., 4.])
    );
    assert_eq!(
        prepared.evaluate(0.).unwrap()["trigger"],
        expected(vec![1., 2.])
    );
    assert_eq!(second.reads.load(Ordering::SeqCst), 1);
    let mut borrowed = frame(&[]);
    borrowed.features = Some(second.as_ref());
    assert_eq!(
        library
            .evaluate("drum_trigger", &BTreeMap::new(), borrowed)
            .unwrap()["trigger"],
        expected(vec![3., 4.])
    );
    assert_eq!(second.reads.load(Ordering::SeqCst), 2);
}

#[derive(Debug, Default)]
struct Analysis {
    event_reads: AtomicUsize,
}
impl FeatureSource for Analysis {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
        Ok(match request {
            FeatureRequest::Timing => FeatureSample::Timing(Arc::new(
                TrackTiming::new(
                    BeatTimeline::new(vec![0., 0.5, 1., 1.5, 2.], 0.).unwrap(),
                    EventTimes::new(vec![0., 0.5, 1., 1.5, 2.]).unwrap(),
                    EventTimes::new(vec![0., 2.]).unwrap(),
                    120.,
                )
                .unwrap(),
            )),
            FeatureRequest::Spectrum { .. } => FeatureSample::Spectrum {
                bins: vec![beat.abs(), 0.5 + 0.4 * beat.sin(), 2.],
                bin_hz: 20.,
            },
            FeatureRequest::Band { .. } => FeatureSample::Energy(0.5 + 0.4 * beat.sin()),
            FeatureRequest::Harmony => {
                FeatureSample::PitchClass(Some(beat.floor().rem_euclid(12.0) as u8))
            }
            FeatureRequest::Onsets(_) => FeatureSample::Onset(if beat >= 0.0 {
                Some((beat.floor(), beat.floor() as u64))
            } else {
                None
            }),
        })
    }
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        self.event_reads.fetch_add(1, Ordering::SeqCst);
        EventTimes::new(vec![0.0, 1.0, 2.0, 2.5, 4.0, 6.0, 8.0, 10.0])
    }
}

#[test]
fn default_graphs_keep_all_time_samples_through_arithmetic_color_and_output() {
    let library = standard_library();
    let cells = cells();
    let times = [9.9, 2.0, 4.5, -0.2, 6.3, 2.0, 3.2];
    let mut checked = 0;
    for (id, definition) in &library.definitions {
        if definition
            .inputs
            .values()
            .any(|input| input.default.is_none())
        {
            continue;
        }
        let program = PreparedGraph::new(&library, id, &BTreeMap::new(), frame(&cells))
            .unwrap_or_else(|e| panic!("prepare {id}: {e}"))
            .with_features(Arc::new(Analysis::default()))
            .unwrap();
        let batch = program
            .evaluate_batch(&times)
            .unwrap_or_else(|e| panic!("batch {id}: {e}"));
        for (t, beat) in times.iter().enumerate() {
            let single = program
                .evaluate(*beat)
                .unwrap_or_else(|e| panic!("single {id}: {e}"));
            for (key, value) in &batch {
                assert_eq!(
                    value.sample(t).unwrap(),
                    single[key],
                    "{id}.{key} at {beat}"
                );
            }
        }
        program
            .evaluate_batch(&[])
            .unwrap_or_else(|e| panic!("empty {id}: {e}"));
        checked += 1;
    }
    assert!(checked >= 25, "only checked {checked} default graphs");
}

fn wired(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn node(definition: &str, inputs: BTreeMap<String, Binding>) -> Node {
    Node {
        position: None,
        definition: definition.into(),
        inputs,
    }
}

#[test]
fn nested_outputs_only_prepare_their_connected_analysis_and_reductions() {
    #[derive(Debug, Default)]
    struct SnareOnly(AtomicUsize);
    impl FeatureSource for SnareOnly {
        fn sample(&self, _: &FeatureRequest, _: f64) -> Result<FeatureSample> {
            Err(Error("audio analysis is unavailable".into()))
        }
        fn onsets(&self, drum: Drum) -> Result<EventTimes> {
            if drum != Drum::Snare {
                return Err(Error("only snare was analyzed".into()));
            }
            self.0.fetch_add(1, Ordering::SeqCst);
            EventTimes::new(vec![1., 2.])
        }
    }
    fn define(library: &mut Library, id: &str, graph: Graph) {
        let outputs = graph
            .outputs
            .iter()
            .map(|(name, binding)| {
                let (value_type, rate) = library
                    .binding_type(&BTreeMap::new(), &graph, binding)
                    .unwrap();
                (name.clone(), Output { value_type, rate })
            })
            .collect();
        library.definitions.insert(
            id.into(),
            Definition {
                name: id.into(),
                inputs: BTreeMap::new(),
                outputs,
                body: Body::Graph(graph),
            },
        );
    }
    let mut library = standard_library();
    let mut graph = Graph::default();
    for (name, drum) in [("snare", Drum::Snare), ("kick", Drum::Kick)] {
        graph.nodes.insert(
            name.into(),
            node(
                "drum_trigger",
                BTreeMap::from([("drum".into(), Value::Drum(drum).into())]),
            ),
        );
        graph.outputs.insert(name.into(), wired(name, "trigger"));
    }
    graph
        .nodes
        .insert("audio".into(), node("band_energy", BTreeMap::new()));
    graph.nodes.insert(
        "range".into(),
        node(
            "clip_range",
            BTreeMap::from([("value".into(), wired("audio", "value"))]),
        ),
    );
    graph
        .outputs
        .insert("peak".into(), wired("range", "maximum"));
    graph
        .outputs
        .insert("constant".into(), Value::Number(0.5).into());
    define(&mut library, "sources", graph);
    let mut wrapper = Graph::default();
    wrapper
        .nodes
        .insert("sources".into(), node("sources", BTreeMap::new()));
    for key in ["snare", "kick", "peak", "constant"] {
        wrapper.outputs.insert(key.into(), wired("sources", key));
    }
    define(&mut library, "wrapper", wrapper);
    for key in ["snare", "constant", "kick", "peak"] {
        let mut root = Graph::default();
        root.nodes
            .insert("source".into(), node("wrapper", BTreeMap::new()));
        root.outputs.insert("value".into(), wired("source", key));
        define(&mut library, "root", root);
        let prepared = PreparedGraph::new(&library, "root", &BTreeMap::new(), frame(&[])).unwrap();
        if key == "constant" {
            assert!(prepared.feature_requests().is_empty());
            assert_eq!(prepared.dynamic_step_count(), 0);
            assert_eq!(
                prepared.evaluate(100.).unwrap()["value"],
                Value::Number(0.5)
            );
            continue;
        }
        assert_eq!(prepared.feature_requests().len(), 1);
        let features = Arc::new(SnareOnly::default());
        if key != "snare" {
            // Connecting an unavailable branch still reports its real error.
            assert!(prepared.with_features(features).is_err());
            continue;
        }
        assert_eq!(
            prepared.feature_requests(),
            &[FeatureRequest::Onsets(Drum::Snare)]
        );
        let prepared = prepared.with_features(features.clone()).unwrap();
        for beat in [2., 1., -1., 2.] {
            assert_eq!(
                prepared.evaluate(beat).unwrap()["value"],
                Value::Events(Events::Beats {
                    times: EventTimes::new(vec![1., 2.]).unwrap(),
                })
            );
        }
        assert_eq!(features.0.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn a_chase_batch_consumes_events_once_and_retains_overlapping_journeys() {
    let mut library = standard_library();
    let chase = &library.definitions["chase"];
    let definition = Definition {
        name: "Snare chase".into(),
        inputs: BTreeMap::from([
            ("travel".into(), chase.inputs["travel"].clone()),
            ("width".into(), chase.inputs["width"].clone()),
        ]),
        outputs: library.definitions["output"].outputs.clone(),
        body: Body::Graph(Graph {
            input_nodes: BTreeMap::new(),
            nodes: BTreeMap::from([
                (
                    "snare".into(),
                    node(
                        "drum_trigger",
                        BTreeMap::from([("drum".into(), Value::Drum(Drum::Snare).into())]),
                    ),
                ),
                ("mapping".into(), node("resolve_mapping", BTreeMap::new())),
                (
                    "effect".into(),
                    node(
                        "chase",
                        BTreeMap::from([
                            ("trigger".into(), wired("snare", "trigger")),
                            ("mapping".into(), wired("mapping", "coordinates")),
                            (
                                "travel".into(),
                                Binding::Input {
                                    input: "travel".into(),
                                },
                            ),
                            (
                                "width".into(),
                                Binding::Input {
                                    input: "width".into(),
                                },
                            ),
                        ]),
                    ),
                ),
                (
                    "tint".into(),
                    node(
                        "core/multiply",
                        BTreeMap::from([
                            ("a".into(), wired("effect", "mask")),
                            ("b".into(), Value::Color([1.0; 3]).into()),
                        ]),
                    ),
                ),
                (
                    "output".into(),
                    node(
                        "output",
                        BTreeMap::from([("color".into(), wired("tint", "value"))]),
                    ),
                ),
            ]),
            outputs: BTreeMap::from([("lighting".into(), wired("output", "lighting"))]),
        }),
    };
    library.definitions.insert("test".into(), definition);
    let cells = cells();
    let analysis = Arc::new(Analysis::default());
    let program = PreparedGraph::new(
        &library,
        "test",
        &BTreeMap::from([
            ("travel".into(), Value::Beats(3.0)),
            ("width".into(), Value::Proportion(0.3)),
        ]),
        frame(&cells),
    )
    .unwrap()
    .with_features(analysis.clone())
    .unwrap();
    analysis.event_reads.store(0, Ordering::SeqCst);
    let times = [3.1, 2.1, 4.9, 1.2];
    let batch = program.evaluate_batch(&times).unwrap();
    assert_eq!(
        analysis.event_reads.load(Ordering::SeqCst),
        0,
        "prepared event data is shared without reading analysis during playback"
    );
    let output = batch["lighting"].lighting().unwrap();
    assert_eq!(output.values().dim(), (16, 4, 8));
    assert_eq!(output.writes()[..2], [true, true]);
    for (t, beat) in times.iter().enumerate() {
        assert_eq!(
            batch["lighting"].sample(t).unwrap(),
            program.evaluate(*beat).unwrap()["lighting"]
        );
    }
}

#[test]
fn animated_chase_width_and_envelope_are_sampled_on_the_time_axis() {
    let mut library = standard_library();
    let mut definition = library.definitions["beat_chase"]
        .clip_instance("beat_chase")
        .unwrap();
    definition.inputs.remove("width");
    definition.inputs.remove("shape");
    let Body::Graph(graph) = &mut definition.body else {
        panic!()
    };
    graph
        .nodes
        .insert("clock".into(), node("clip_time", BTreeMap::new()));
    graph.nodes.insert(
        "edges".into(),
        node(
            "soft_edges",
            BTreeMap::from([("softness".into(), wired("clock", "progress"))]),
        ),
    );
    graph.nodes.get_mut("chase").unwrap().inputs.extend([
        ("width".into(), wired("clock", "progress")),
        ("shape".into(), wired("edges", "shape")),
    ]);
    library.definitions.insert("test".into(), definition);
    let cells = cells();
    let program = PreparedGraph::new(
        &library,
        "test",
        &BTreeMap::from([(
            "mapping".into(),
            Value::Mapping(MappingSpec {
                source: MappingSource::U,
                mirror: None,
                per_group: false,
                reverse: false,
            }),
        )]),
        frame(&cells),
    )
    .unwrap();
    let times = [9.5, 2.0, 2.5, 7.0, 4.0];
    let batch = program.evaluate_batch(&times).unwrap();
    for (t, beat) in times.iter().enumerate() {
        assert_eq!(
            batch["lighting"].sample(t).unwrap(),
            program.evaluate(*beat).unwrap()["lighting"]
        );
    }
    // The stroke has left by 9.5 and is a wide band mid-passage at 7.0.
    assert_ne!(
        batch["lighting"].sample(0).unwrap(),
        batch["lighting"].sample(3).unwrap()
    );
}
