use luma_patterns::*;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
struct Analysis;
impl FeatureSource for Analysis {
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        EventTimes::new((0..12).map(f64::from).collect::<Vec<_>>())
    }
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
        Ok(match request {
            FeatureRequest::Timing => return Err(Error("test source has no timing".into())),
            FeatureRequest::Spectrum { .. } => {
                return Err(Error("test source has no spectrum".into()))
            }
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
}
#[derive(serde::Deserialize)]
struct Baseline {
    score: Score,
    cells: Vec<Cell>,
    times: Vec<f64>,
    samples: BTreeMap<String, Vec<Value>>,
}

#[test]
fn v2_builtins_migrate_against_independent_original_engine_samples() {
    let baseline: Baseline =
        serde_json::from_str(include_str!("fixtures/v2-samples.json")).unwrap();
    let original = serde_json::to_string(&baseline.score).unwrap();
    let upgraded = migration::upgrade(&baseline.score).unwrap();
    assert_eq!(serde_json::to_string(&baseline.score).unwrap(), original);
    assert_eq!(migration::upgrade(&upgraded).unwrap(), upgraded);
    assert_eq!(upgraded.clips.len(), 36);
    let library = upgraded.library(&standard_library()).unwrap();
    let original_library = baseline.score.library(&migration::v2_library()).unwrap();
    for (id, clip) in &upgraded.clips {
        assert_eq!(
            library.display_name(&clip.graph),
            original_library.display_name(&baseline.score.clips[id].graph)
        );
        let mut expected_clip = baseline.score.clips[id].clone();
        expected_clip.graph = clip.graph.clone();
        assert_eq!(*clip, expected_clip, "only a graph reference may change");
        let program = PreparedGraph::new(
            &library,
            &clip.graph,
            &clip.inputs,
            Frame {
                features: None,
                cells: &baseline.cells,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )
        .unwrap()
        .with_features(Arc::new(Analysis))
        .unwrap();
        let batch = program.evaluate_batch(&baseline.times).unwrap();
        for (time, expected) in baseline.samples[id].iter().enumerate() {
            let Value::Lighting(expected) = expected else {
                panic!()
            };
            let actual = batch["lighting"].lighting().unwrap().sample(time).unwrap();
            let label = format!("{id} at {}", baseline.times[time]);
            if original_library.display_name(&baseline.score.clips[id].graph) == "Dissolve Flash" {
                assert_dissolve_order(&actual, expected, &label);
            } else {
                assert_outputs(&actual, expected, &label);
            }
        }
    }
    for (id, definition) in &upgraded.definitions {
        assert!(
            definition
                .inputs
                .values()
                .all(|p| p.value_type != ValueType::Lighting),
            "{id}"
        );
        let Body::Graph(graph) = &definition.body else {
            panic!()
        };
        for node in graph.nodes.values() {
            let definition = &library.definitions[&node.definition];
            assert!(
                !matches!(
                    definition.body,
                    Body::Primitive(
                        Primitive::TravelClock
                            | Primitive::ScalarConvert { .. }
                            | Primitive::ScalarBinary(_)
                            | Primitive::Broadcast(_)
                    )
                ),
                "{id} retained an obsolete adapter"
            );
            if definition
                .outputs
                .values()
                .any(|p| p.value_type == ValueType::Lighting)
            {
                assert_eq!(definition.body, Body::Primitive(Primitive::Output));
            }
        }
    }
}

/// Dissolve now cuts its seeded random order at whole heads instead of at each
/// head's own random threshold, and softness fades along ranks. The order is
/// the original engine's, so the lit sets are prefixes of one another.
fn assert_dissolve_order(
    actual: &BTreeMap<String, FixtureOutput>,
    expected: &BTreeMap<String, FixtureOutput>,
    label: &str,
) {
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    let lit = |values: &BTreeMap<String, FixtureOutput>| {
        values
            .iter()
            .filter(|(id, value)| {
                assert_eq!(value.writes(), expected[*id].writes(), "{label} / {id}");
                value.dimmer.is_some_and(|dimmer| dimmer > 0.0)
            })
            .map(|(id, _)| id.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let (actual, expected) = (lit(actual), lit(expected));
    assert!(
        actual.is_subset(&expected) || expected.is_subset(&actual),
        "{label}: {actual:?} and {expected:?} are not prefixes of one order"
    );
}

fn assert_outputs(
    actual: &BTreeMap<String, FixtureOutput>,
    expected: &BTreeMap<String, FixtureOutput>,
    label: &str,
) {
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (id, value) in actual {
        let expected = &expected[id];
        // Brightness now rides in the applied color: a historical dimmer-only
        // write reads back as white, and chromaticity splits differently, so
        // the product is what must agree.
        assert_eq!(
            value.writes()[1..],
            expected.writes()[1..],
            "{label} / {id}"
        );
        let channels = |v: &FixtureOutput| {
            v.rgb()
                .into_iter()
                .chain(v.position.into_iter().flatten())
                .chain(v.strobe)
                .chain(v.speed)
                .collect::<Vec<_>>()
        };
        for (a, e) in channels(value).into_iter().zip(channels(expected)) {
            assert!(
                (a - e).abs() < 2e-6,
                "{label} / {id}: {value:?} != {expected:?}"
            );
        }
    }
}

fn wire(node: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: "lighting".into(),
    }
}
fn node(id: &str, inputs: &[(&str, Binding)]) -> Node {
    Node {
        definition: id.into(),
        position: Some([120.0, 90.0]),
        inputs: inputs
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    }
}

#[test]
fn shared_bundle_helpers_specialize_without_inventing_capability_writes() {
    let old = migration::v2_library();
    let mut score = serde_json::from_value::<Score>(
        serde_json::json!({"version":2,"definitions":{},"clips":{}}),
    )
    .unwrap();
    let mut helper = old.definitions["add_lighting"].instance("add_lighting");
    helper.name = "Layer accent".into();
    score.definitions.insert("layer".into(), helper);
    let mut tint = old.definitions["wash"].inputs["color"].clone();
    tint.name = "Snare color".into();
    let definition = Definition {
        name: "Authored scene".into(),
        inputs: BTreeMap::from([("tint".into(), tint)]),
        outputs: old.definitions["wash"].outputs.clone(),
        body: Body::Graph(Graph {
            input_nodes: BTreeMap::from([(
                "tint".into(),
                InputNode {
                    name: "Snare color".into(),
                    position: Some([4.0, 8.0]),
                },
            )]),
            nodes: BTreeMap::from([
                (
                    "black".into(),
                    node("wash", &[("color", Value::Color([0.0; 3]).into())]),
                ),
                (
                    "color".into(),
                    node(
                        "wash",
                        &[(
                            "color",
                            Binding::Input {
                                input: "tint".into(),
                            },
                        )],
                    ),
                ),
                (
                    "position".into(),
                    node("write_position", &[("pan", Value::Number(30.0).into())]),
                ),
                (
                    "first".into(),
                    node("layer", &[("a", wire("black")), ("b", wire("color"))]),
                ),
                (
                    "second".into(),
                    node("layer", &[("a", wire("first")), ("b", wire("position"))]),
                ),
            ]),
            outputs: BTreeMap::from([("lighting".into(), wire("second"))]),
        }),
    };
    score.definitions.insert("root".into(), definition);
    // Distinct calls of the same helper have different capability signatures.
    let second = Definition {
        name: "Dimmer and speed".into(),
        inputs: BTreeMap::new(),
        outputs: old.definitions["wash"].outputs.clone(),
        body: Body::Graph(Graph {
            nodes: BTreeMap::from([
                (
                    "dimmer".into(),
                    node("write_dimmer", &[("value", Value::Proportion(0.4).into())]),
                ),
                (
                    "speed".into(),
                    node("write_speed", &[("value", Value::Proportion(0.8).into())]),
                ),
                (
                    "mix".into(),
                    node("layer", &[("a", wire("dimmer")), ("b", wire("speed"))]),
                ),
            ]),
            outputs: BTreeMap::from([("lighting".into(), wire("mix"))]),
            ..Graph::default()
        }),
    };
    score.definitions.insert("other".into(), second);
    for graph in ["root", "other"] {
        score.clips.insert(
            graph.into(),
            Clip {
                graph: graph.into(),
                start: 2.0,
                duration: 4.0,
                seed: 12,
                selection_seed: Some(34),
                selection: Selection::all(),
                z_index: 5,
                blend_mode: BlendMode::Screen,
                inputs: if graph == "root" {
                    BTreeMap::from([("tint".into(), Value::Color([0.3, 0.1, 0.5]))])
                } else {
                    BTreeMap::new()
                },
            },
        );
    }
    let upgraded = migration::upgrade(&score).unwrap();
    assert_eq!(upgraded.clips, score.clips);
    assert_eq!(
        upgraded.definitions["root"].inputs["tint"].name,
        "Snare color"
    );
    let Body::Graph(root) = &upgraded.definitions["root"].body else {
        panic!()
    };
    assert_eq!(root.input_nodes["tint"].position, Some([4.0, 8.0]));
    assert_eq!(root.nodes["first"].position, Some([120.0, 90.0]));
    assert_ne!(
        root.nodes["first"].definition,
        root.nodes["second"].definition
    );
    let library = upgraded.library(&standard_library()).unwrap();
    let cells = [Cell {
        id: "head".into(),
        group: "all".into(),
        world: [0.0; 3],
        uvz: [0.0; 3],
    }];
    for (id, clip) in &upgraded.clips {
        let result = library
            .evaluate(
                &clip.graph,
                &clip.inputs,
                Frame {
                    features: None,
                    cells: &cells,
                    beat: 3.0,
                    clip_start: 2.0,
                    clip_duration: 4.0,
                    seed: 12,
                },
            )
            .unwrap();
        let Value::Lighting(result) = &result["lighting"] else {
            panic!()
        };
        let expected = if id == "root" {
            let mut value = FixtureOutput::from_rgb([0.0; 3]);
            value.composite(&FixtureOutput::from_rgb([0.3, 0.1, 0.5]), BlendMode::Add);
            value.position = Some([30.0, 0.0]);
            value
        } else {
            FixtureOutput {
                dimmer: Some(0.4),
                speed: Some(1.0),
                ..FixtureOutput::default()
            }
        };
        assert_outputs(result, &BTreeMap::from([("head".into(), expected)]), id);
    }
}

#[test]
fn migrated_repeating_effects_overlap_and_keep_original_override_keys() {
    let baseline: Baseline =
        serde_json::from_str(include_str!("fixtures/v2-samples.json")).unwrap();
    let mut upgraded = migration::upgrade(&baseline.score).unwrap();
    for (effect, pattern, length) in [
        ("chase", "beat_chase", "travel"),
        ("pulse", "beat_pulse", "duration"),
        ("dissolve_flash", "beat_dissolve", "duration"),
    ] {
        let clip = upgraded
            .clips
            .get_mut(&format!("sample-{effect}-0"))
            .unwrap();
        // Migrated clips keep their original override keys.
        clip.inputs.insert("travel".into(), Value::Beats(6.0));
        clip.inputs.insert("repeat".into(), Value::Beats(2.0));
        let clip = clip.clone();
        let mut reference_inputs = clip.inputs.clone();
        let travel = reference_inputs.remove("travel").unwrap();
        reference_inputs.insert(length.into(), travel);
        upgraded.validate(&standard_library()).unwrap();
        let lib = upgraded.library(&standard_library()).unwrap();
        let frame = Frame {
            features: None,
            cells: &baseline.cells,
            beat: clip.start,
            clip_start: clip.start,
            clip_duration: clip.duration,
            seed: clip.seed,
        };
        let program = PreparedGraph::new(&lib, &clip.graph, &clip.inputs, frame).unwrap();
        let batch = program.evaluate_batch(&[7.0, 3.0, 5.0, 7.0]).unwrap();
        let mut reference_library = standard_library();
        reference_library.definitions.insert(
            "reference".into(),
            reference_library.definitions[pattern]
                .clip_instance(pattern)
                .unwrap(),
        );
        let reference =
            PreparedGraph::new(&reference_library, "reference", &reference_inputs, frame)
                .unwrap()
                .evaluate_batch(&[7.0, 3.0, 5.0, 7.0])
                .unwrap();
        for time in 0..4 {
            let actual = batch["lighting"].lighting().unwrap().sample(time).unwrap();
            let expected = reference["lighting"]
                .lighting()
                .unwrap()
                .sample(time)
                .unwrap();
            for (id, actual) in actual {
                assert_eq!(actual.dimmer, expected[&id].dimmer, "{effect} / {id}");
                assert_eq!(actual.rgb(), expected[&id].rgb(), "{effect} / {id}");
            }
        }
    }
}

#[test]
fn migration_rejects_invalid_source_without_panicking_or_modifying_it() {
    let mut score = serde_json::from_value::<Score>(
        serde_json::json!({"version":2,"definitions":{},"clips":{}}),
    )
    .unwrap();
    score.definitions.insert(
        "injected-kernel".into(),
        standard_library().definitions["output"].clone(),
    );
    let original = score.clone();
    assert!(migration::upgrade(&score)
        .unwrap_err()
        .0
        .contains("score-local definitions"));
    assert_eq!(score, original);
}

#[test]
fn every_v2_node_can_survive_as_an_unused_authored_helper() {
    let old = migration::v2_library();
    let mut score = serde_json::from_value::<Score>(
        serde_json::json!({"version":2,"definitions":{},"clips":{}}),
    )
    .unwrap();
    score.definitions = old
        .definitions
        .iter()
        .map(|(id, definition)| (format!("saved-{id}"), definition.instance(id)))
        .collect();
    let migrated = migration::upgrade(&score).unwrap();
    migrated.validate(&standard_library()).unwrap();
    assert!(migrated.definitions.len() >= score.definitions.len());
    assert!(migrated.clips.is_empty());
}

#[test]
fn migrated_drum_pulse_preserves_a_rising_tail_across_a_later_event() {
    let baseline: Baseline =
        serde_json::from_str(include_str!("fixtures/v2-samples.json")).unwrap();
    let mut score = migration::upgrade(&baseline.score).unwrap();
    let clip = score.clips.get_mut("sample-drum_pulse-0").unwrap();
    clip.inputs.insert("duration".into(), Value::Beats(2.0));
    clip.inputs.insert(
        "shape".into(),
        Value::Envelope(Envelope::linear(vec![[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]])),
    );
    let clip = clip.clone();
    score.validate(&standard_library()).unwrap();
    let lib = score.library(&standard_library()).unwrap();
    let output = PreparedGraph::new(
        &lib,
        &clip.graph,
        &clip.inputs,
        Frame {
            features: None,
            cells: &baseline.cells,
            beat: 2.0,
            clip_start: 2.0,
            clip_duration: 8.0,
            seed: clip.seed,
        },
    )
    .unwrap()
    .with_features(Arc::new(Analysis))
    .unwrap()
    .evaluate_batch(&[3.2, 3.0])
    .unwrap();
    for (time, expected) in [0.8, 1.0].into_iter().enumerate() {
        for value in output["lighting"]
            .lighting()
            .unwrap()
            .sample(time)
            .unwrap()
            .values()
        {
            assert!(
                (value.dimmer.unwrap() - expected).abs() < 1e-12,
                "a later event erased an earlier rising tail"
            );
        }
    }
}

#[test]
fn newly_reserved_names_do_not_replace_authored_helpers() {
    let frozen = migration::v2_library();
    let mut score: Score =
        serde_json::from_value(serde_json::json!({"version":2,"definitions":{},"clips":{}}))
            .unwrap();
    score
        .definitions
        .insert("output".into(), frozen.definitions["wash"].instance("wash"));
    score.clips.insert(
        "output".into(),
        serde_json::from_value(
            serde_json::json!({"graph":"output","start":0.0,"duration":4.0,"seed":0}),
        )
        .unwrap(),
    );
    let mut helper = frozen.definitions["wash"].instance("wash");
    helper.name = "Authored drum trigger".into();
    helper.inputs.get_mut("color").unwrap().default = Some(Value::Color([0.2, 0.4, 0.6]));
    score.definitions.insert("drum_trigger".into(), helper);
    let Body::Graph(graph) = &mut score.definitions.get_mut("output").unwrap().body else {
        panic!()
    };
    graph.nodes.get_mut("effect").unwrap().definition = "drum_trigger".into();
    graph.nodes.get_mut("effect").unwrap().inputs.clear();
    let upgraded = migration::upgrade(&score).unwrap();
    assert_ne!(upgraded.clips["output"].graph, "output");
    assert!(!upgraded.definitions.contains_key("output"));
    assert!(!upgraded.definitions.contains_key("drum_trigger"));
    assert!(upgraded
        .definitions
        .values()
        .any(|d| d.name == "Authored drum trigger"));
    let cells = [Cell {
        id: "head".into(),
        group: "all".into(),
        world: [0.; 3],
        uvz: [0.; 3],
    }];
    let result = upgraded
        .prepare_clip(&standard_library(), "output", &BTreeMap::new(), &cells)
        .unwrap()
        .evaluate(1.)
        .unwrap();
    let Value::Lighting(values) = &result["lighting"] else {
        panic!()
    };
    for (actual, expected) in values["head"].rgb().into_iter().zip([0.2, 0.4, 0.6]) {
        assert!((actual - expected).abs() < 1e-12);
    }
}
