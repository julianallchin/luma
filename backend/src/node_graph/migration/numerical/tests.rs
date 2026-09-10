use super::*;
mod audio_filters;
mod mixed;
mod palettes;
mod reductions;
mod selection;
mod spectral;
mod viewers;

#[test]
fn original_soft_voronoi_graphs_match_frozen_reference() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/voronoi-v1.json")).unwrap();
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 380);
    for fixture in &fixtures {
        compare_reference(fixture);
    }
}

#[test]
fn exposed_voronoi_palettes_preserve_transparent_clip_overrides() {
    use crate::models::node_graph::PatternArgDef;
    let mut fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/voronoi-v1.json")).unwrap();
    let fixture = fixtures.last_mut().unwrap();
    fixture.cases.retain(|case| case.name == "alpha/default");
    let graph = &mut fixture.cases[0].graph;
    let palette = graph.nodes.iter().find(|n| n.id == "palette").unwrap();
    let supplied = parse_json(palette.params["value"].clone());
    graph.nodes.retain(|n| n.id != "palette");
    graph.nodes.push(NodeInstance {
        id: "pattern_args".into(),
        type_id: "pattern_args".into(),
        params: Default::default(),
        position_x: None,
        position_y: None,
    });
    for edge in &mut graph.edges {
        if edge.from_node == "palette" {
            edge.from_node = "pattern_args".into();
            edge.from_port = "colors".into();
        }
    }
    graph.args.push(PatternArgDef {
        id: "colors".into(),
        name: "Region colors".into(),
        arg_type: PatternArgType::Palette,
        default_value: serde_json::json!({"colors":["#ffffff"]}),
    });
    let color = argument_value(&PatternArgType::Palette, ValueType::Gradient, &supplied).unwrap();
    let Value::Gradient(gradient) = &color else {
        panic!()
    };
    assert!((gradient.stops[0].alpha - 0.2).abs() < 1e-6);
    compare_reference_with_inputs(fixture, &BTreeMap::from([("colors".into(), color)]));
}
use crate::models::node_graph::{BeatGrid, Signal};
use crate::models::universe::UniverseState;
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    times: Vec<f32>,
    grid: BeatGrid,
    cells: Vec<p::Cell>,
    clip_start: f64,
    clip_duration: f64,
    #[serde(default)]
    harmony: Option<Vec<(f32, f32, Option<u8>)>>,
    #[serde(default)]
    audio: BTreeMap<p::AudioSource, RecordedAudio>,
    #[serde(default)]
    onsets: BTreeMap<String, Vec<f32>>,
    cases: Vec<Case>,
}
#[derive(Clone, Debug, Deserialize)]
struct RecordedAudio {
    samples: Vec<f32>,
    sample_rate: u32,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    graph: Graph,
    frames: Vec<UniverseState>,
    views: BTreeMap<String, Signal>,
}

#[test]
fn original_numerical_graphs_match_frozen_reference() {
    let fixture: Reference =
        serde_json::from_str(include_str!("../fixtures/numerical-v1.json")).unwrap();
    assert_eq!(fixture.cases.len(), 62);
    compare_reference(&fixture);
}

#[test]
fn original_movement_graphs_match_frozen_reference() {
    let fixture: Reference =
        serde_json::from_str(include_str!("../fixtures/movement-v1.json")).unwrap();
    assert_eq!(fixture.cases.len(), 37);
    compare_reference(&fixture);
}

#[test]
fn original_spatial_graphs_match_frozen_reference() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/spatial-v1.json")).unwrap();
    assert_eq!(fixtures.len(), 6);
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 546);
    for fixture in &fixtures {
        compare_reference(fixture);
    }
}

#[test]
fn original_harmony_and_falloff_graphs_match_frozen_reference() {
    let fixture: Reference =
        serde_json::from_str(include_str!("../fixtures/harmony-v1.json")).unwrap();
    assert_eq!(fixture.cases.len(), 110);
    compare_reference(&fixture);
}

#[test]
fn original_frequency_and_stem_graphs_match_frozen_reference() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/audio-v1.json")).unwrap();
    assert_eq!(fixtures.len(), 6);
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 160);
    for fixture in &fixtures {
        compare_reference(fixture);
    }
}

#[test]
fn original_noise_and_wander_graphs_match_frozen_reference() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/noise-v1.json")).unwrap();
    assert_eq!(fixtures.len(), 3);
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 306);
    for fixture in &fixtures {
        compare_reference(fixture);
    }
}

#[test]
fn original_event_and_envelope_graphs_match_reference_except_zero_duration_flashes() {
    let mut fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/events-v1.json")).unwrap();
    assert_eq!(fixtures.len(), 4);
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 960);
    let mut removed_flashes = 0;
    for fixture in &mut fixtures {
        // The old evaluator held sustain for an extra 100 microseconds even
        // when all four durations were zero. A finite zero-length response is
        // now silent. Keep the frozen evidence intact and adjust only that
        // explicitly identified behavior for playback/compositor comparison.
        for case in &mut fixture.cases {
            let zero_length = case.graph.nodes.iter().any(|node| {
                matches!(node.type_id.as_str(), "adsr" | "beat_envelope")
                    && ["attack", "decay", "sustain", "release"].iter().all(|id| {
                        node.params.get(*id).and_then(serde_json::Value::as_f64) == Some(0.)
                    })
            });
            if zero_length {
                for view in case.views.values_mut() {
                    for value in &mut view.data {
                        assert!(*value == 0. || (*value - 0.7).abs() < 1e-6);
                        removed_flashes += usize::from(*value != 0.);
                        *value = 0.;
                    }
                }
                for frame in &mut case.frames {
                    for value in frame.primitives.values_mut() {
                        value.dimmer = 0.;
                    }
                }
            }
        }
        compare_reference(fixture);
    }
    assert_eq!(removed_flashes, 144);
    // Hand-authored Booleans now mean the same thing as numeric UI checkboxes;
    // the old traced-event path incorrectly ignored Boolean true.
    let fixture = &mut fixtures[1];
    fixture
        .cases
        .retain(|case| case.name == "adsr-beat-0-false-3-1-1-0.2");
    assert_eq!(fixture.cases.len(), 1);
    fixture.cases[0]
        .graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "trigger")
        .unwrap()
        .params
        .insert("only_downbeats".into(), serde_json::json!(true));
    compare_reference(fixture);
}

#[test]
fn retired_event_shapes_play_through_envelope_in_historical_graphs() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/events-v1.json")).unwrap();
    assert!(crate::node_graph::nodes::get_node_types()
        .iter()
        .all(|n| !matches!(n.id.as_str(), "adsr" | "beat_envelope")));
    let mut checked = 0;
    for fixture in fixtures {
        for case in fixture
            .cases
            .iter()
            .filter(|c| {
                c.graph.nodes.iter().any(|n| {
                    matches!(n.type_id.as_str(), "adsr" | "beat_envelope")
                        && ["attack", "decay", "sustain", "release"]
                            .iter()
                            .all(|key| !n.params.contains_key(*key))
                })
            })
            .step_by(7)
        {
            checked += 1;
            let plan = crate::eval::compile::compile_pattern(
                &case.graph,
                &Default::default(),
                crate::eval::ResidentContext {
                    beat_grid: Some(fixture.grid.clone()),
                    positions: fixture
                        .cells
                        .iter()
                        .map(|c| c.world.map(|v| v as f32))
                        .collect(),
                    drum_onsets: fixture.onsets.clone().into_iter().collect(),
                    span: (
                        fixture
                            .grid
                            .timeline()
                            .unwrap()
                            .seconds_at(fixture.clip_start)
                            .unwrap() as f32,
                        fixture
                            .grid
                            .timeline()
                            .unwrap()
                            .seconds_at(fixture.clip_start + fixture.clip_duration)
                            .unwrap() as f32,
                    ),
                    ..Default::default()
                },
                fixture.cells.iter().map(|c| c.id.clone()).collect(),
            )
            .unwrap();
            assert!(plan.program.is_some());
            let mut arena = crate::eval::Arena::default();
            let frames = crate::eval::try_eval(&plan, &fixture.times, &mut arena).unwrap();
            assert_eq!(frames.len(), case.frames.len());
            for (actual, expected) in frames.iter().zip(&case.frames) {
                assert_eq!(actual.primitives.len(), expected.primitives.len());
                for (id, actual) in &actual.primitives {
                    assert!(
                        (actual.dimmer - expected.primitives[id].dimmer).abs() < 3e-5,
                        "{}",
                        case.name
                    );
                }
            }
        }
    }
    assert!(checked >= 20, "only checked {checked} saved envelopes");
}

#[test]
fn old_stage_settings_become_an_editable_envelope_without_stage_controls() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/events-v1.json")).unwrap();
    let case = fixtures[1]
        .cases
        .iter()
        .find(|c| c.name == "adsr-beat-2-false-3-2-0-0.2")
        .unwrap();
    let score = convert(&case.graph, "Envelope").unwrap();
    let definition = &score.definitions["node/effect"];
    assert_eq!(definition.name, "Envelope");
    let Some(Value::Envelope(shape)) = &definition.inputs["shape"].default else {
        panic!()
    };
    shape.validate().unwrap();
    for key in [
        "attack",
        "decay",
        "sustain",
        "release",
        "sustain_level",
        "attack_curve",
        "decay_curve",
    ] {
        assert!(!definition.inputs.contains_key(key));
    }
    let Body::Graph(graph) = &definition.body else {
        panic!()
    };
    assert_eq!(
        graph
            .nodes
            .values()
            .filter(|n| n.definition == "envelope")
            .count(),
        1
    );
    assert!(graph
        .nodes
        .values()
        .all(|n| !matches!(n.definition.as_str(), "envelope_points" | "core/power")));
}

#[test]
fn exposed_event_controls_keep_names_and_feed_computed_fixed_overrides() {
    use crate::models::node_graph::{Edge, PatternArgDef};
    let mut fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../fixtures/events-v1.json")).unwrap();
    let fixture = &mut fixtures[1];
    fixture
        .cases
        .retain(|case| case.name == "adsr-beat-0-false-3-2-0-0.2");
    assert_eq!(fixture.cases.len(), 1);
    let graph = &mut fixture.cases[0].graph;
    let mut add_node = |id: &str, kind: &str, params: serde_json::Value| {
        graph.nodes.push(NodeInstance {
            id: id.into(),
            type_id: kind.into(),
            params: serde_json::from_value(params).unwrap(),
            position_x: None,
            position_y: None,
        })
    };
    add_node("args", "pattern_args", serde_json::json!({}));
    add_node("constant", "scalar", serde_json::json!({"value":1.5}));
    add_node("rate_sum", "math", serde_json::json!({"operation":"add"}));
    graph.args.push(PatternArgDef {
        id: "rate".into(),
        name: "Extra pulses".into(),
        arg_type: PatternArgType::Scalar,
        default_value: serde_json::json!(99.),
    });
    for (from, port, to, input) in [
        ("args", "rate", "rate_sum", "a"),
        ("constant", "out", "rate_sum", "b"),
        ("rate_sum", "out", "trigger", "subdivision"),
    ] {
        graph.edges.push(Edge {
            id: format!("{from}-{to}"),
            from_node: from.into(),
            from_port: port.into(),
            to_node: to.into(),
            to_port: input.into(),
        });
    }
    let score = convert(graph, "Exposed event timing").unwrap();
    assert_eq!(score.definitions[ROOT].inputs["rate"].name, "Extra pulses");
    assert_eq!(score.definitions[ROOT].inputs["rate"].rate, Rate::Fixed);
    compare_reference_with_inputs(
        fixture,
        &BTreeMap::from([("rate".into(), Value::Number(0.5))]),
    );
}

#[test]
fn an_unconnected_pitch_palette_is_black_and_keeps_its_editable_inputs() {
    let mut fixture: Reference =
        serde_json::from_str(include_str!("../fixtures/harmony-v1.json")).unwrap();
    let mut graph = fixture.cases.remove(80).graph;
    graph.edges.retain(|e| e.to_port != "chroma");
    let mut score = convert(&graph, "Empty palette").unwrap();
    let helper = &score.definitions["node/palette"];
    assert!(helper.inputs.contains_key("chroma"));
    assert!(helper.inputs.contains_key("stops"));
    let library = score.library(&p::standard_library()).unwrap();
    let features = std::sync::Arc::new(ReferenceFeatures {
        timing: std::sync::Arc::new(fixture.grid.timing().unwrap()),
        onsets: Default::default(),
        audio: Default::default(),
        clock: fixture.grid.timeline().unwrap(),
        harmony: fixture.harmony.unwrap(),
    });
    let prepared = p::PreparedGraph::new(
        &library,
        ROOT,
        &BTreeMap::new(),
        p::Frame {
            cells: &fixture.cells,
            features: None,
            beat: 4.,
            clip_start: 4.,
            clip_duration: 8.,
            seed: 0,
        },
    )
    .unwrap()
    .with_features(features.clone())
    .unwrap();
    let output = prepared.evaluate_batch(&[4., 5.]).unwrap();
    assert!(output["view/view"]
        .signal()
        .unwrap()
        .values()
        .iter()
        .all(|v| *v == 0.));
    let Body::Graph(graph) = &mut score.definitions.get_mut(ROOT).unwrap().body else {
        panic!()
    };
    graph
        .nodes
        .get_mut("palette")
        .unwrap()
        .inputs
        .insert("chroma".into(), wire("harmony", "signal"));
    let library = score.library(&p::standard_library()).unwrap();
    let reconnected = p::PreparedGraph::new(
        &library,
        ROOT,
        &BTreeMap::new(),
        p::Frame {
            cells: &fixture.cells,
            features: None,
            beat: 4.,
            clip_start: 4.,
            clip_duration: 8.,
            seed: 0,
        },
    )
    .unwrap()
    .with_features(features)
    .unwrap()
    .evaluate_batch(&[4.])
    .unwrap();
    assert_eq!(
        reconnected["view/view"]
            .signal()
            .unwrap()
            .values()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![1., 0., 0.]
    );
}

#[test]
fn an_exposed_pitch_palette_preserves_its_name_sharing_and_clip_override() {
    use crate::models::node_graph::{Edge, PatternArgDef};
    let mut fixture: Reference =
        serde_json::from_str(include_str!("../fixtures/harmony-v1.json")).unwrap();
    // A three-color original palette: compare its original output after moving
    // the same stops to a named graph Input and sharing it between two nodes.
    let mut case = fixture.cases.remove(83);
    let raw = serde_json::from_str::<serde_json::Value>(
        case.graph
            .nodes
            .iter()
            .find(|n| n.id == "palette")
            .unwrap()
            .params["fallback_palette"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    case.graph.nodes.push(NodeInstance {
        id: "args".into(),
        type_id: "pattern_args".into(),
        params: Default::default(),
        position_x: None,
        position_y: None,
    });
    case.graph.args.push(PatternArgDef {
        id: "colors".into(),
        name: "Song colors".into(),
        arg_type: PatternArgType::Palette,
        default_value: raw,
    });
    let mut second = case
        .graph
        .nodes
        .iter()
        .find(|n| n.id == "palette")
        .unwrap()
        .clone();
    second.id = "second".into();
    case.graph.nodes.push(second);
    for target in ["palette", "second"] {
        case.graph.edges.push(Edge {
            id: format!("expose-{target}"),
            from_node: "args".into(),
            from_port: "colors".into(),
            to_node: target.into(),
            to_port: "stops".into(),
        });
    }
    case.graph.edges.push(Edge {
        id: "second-chroma".into(),
        from_node: "harmony".into(),
        from_port: "signal".into(),
        to_node: "second".into(),
        to_port: "chroma".into(),
    });
    let score = convert(&case.graph, "Shared palette").unwrap();
    let root = &score.definitions[ROOT];
    assert_eq!(root.inputs["colors"].name, "Song colors");
    let Body::Graph(graph) = &root.body else {
        panic!()
    };
    for target in ["palette", "second"] {
        assert_eq!(
            graph.nodes[target].inputs["stops"],
            B::Input {
                input: "colors".into()
            }
        );
    }
    fixture.cases = vec![case];
    compare_reference(&fixture);
    let library = score.library(&p::standard_library()).unwrap();
    let replacement = argument_value(
        &PatternArgType::Palette,
        ValueType::Gradient,
        &serde_json::json!({"colors":["#00ff00"]}),
    )
    .unwrap();
    let gradient_replacement = argument_value(
        &PatternArgType::Gradient,
        ValueType::Gradient,
        &serde_json::json!({"stops":[{"t":0.4,"color":"#00ff00"}]}),
    )
    .unwrap();
    let (Value::Gradient(palette), Value::Gradient(gradient)) =
        (&replacement, &gradient_replacement)
    else {
        panic!()
    };
    assert_eq!(gradient.stops[0].t, 0.4);
    for t in [-1., 0., 0.5, 1., 2.] {
        assert_eq!(palette.sample(t), gradient.sample(t));
        assert_eq!(palette.sample_alpha(t), gradient.sample_alpha(t));
    }
    let prepared = p::PreparedGraph::new(
        &library,
        ROOT,
        &BTreeMap::from([("colors".into(), replacement)]),
        p::Frame {
            cells: &fixture.cells,
            features: None,
            beat: 4.,
            clip_start: 4.,
            clip_duration: 8.,
            seed: 0,
        },
    )
    .unwrap()
    .with_features(std::sync::Arc::new(ReferenceFeatures {
        timing: std::sync::Arc::new(fixture.grid.timing().unwrap()),
        onsets: Default::default(),
        clock: fixture.grid.timeline().unwrap(),
        harmony: fixture.harmony.clone().unwrap(),
        audio: Default::default(),
    }))
    .unwrap();
    let output = prepared.evaluate_batch(&[4., 5.]).unwrap();
    let values = output["view/view"]
        .signal()
        .unwrap()
        .values()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    for (value, expected) in values.iter().zip([0., 1., 0., 0., 1., 0.]) {
        assert!((*value - expected).abs() < 3e-5, "{values:?}");
    }
}

#[derive(Debug)]
struct ReferenceFeatures {
    clock: p::BeatTimeline,
    timing: std::sync::Arc<p::TrackTiming>,
    onsets: BTreeMap<String, Vec<f32>>,
    harmony: Vec<(f32, f32, Option<u8>)>,
    audio: BTreeMap<p::AudioSource, RecordedAudio>,
}
impl p::FeatureSource for ReferenceFeatures {
    fn onsets(&self, drum: p::Drum) -> p::Result<p::EventTimes> {
        let times = self
            .onsets
            .get(drum.name())
            .ok_or_else(|| p::Error("reference has no drum analysis".into()))?;
        p::EventTimes::new(
            times
                .iter()
                .map(|t| self.clock.beat_at(f64::from(*t)))
                .collect::<p::Result<Vec<_>>>()?,
        )
    }
    fn sample(&self, request: &p::FeatureRequest, beat: f64) -> p::Result<p::FeatureSample> {
        if request == &p::FeatureRequest::Timing {
            return Ok(p::FeatureSample::Timing(self.timing.clone()));
        }
        if let p::FeatureRequest::Spectrum { source, hold_edges } = request {
            let audio = self
                .audio
                .get(&source.source())
                .ok_or_else(|| p::Error(format!("{} reference audio absent", source.name())))?;
            let (bins, bin_hz) = crate::audio::spectrum::sample(
                &audio.samples,
                audio.sample_rate,
                self.clock.seconds_at(beat)?,
                *hold_edges,
            )
            .map_err(p::Error)?;
            return Ok(p::FeatureSample::Spectrum { bins, bin_hz });
        }
        if request != &p::FeatureRequest::Harmony {
            return Err(p::Error("reference has no requested audio analysis".into()));
        }
        let seconds = self.clock.seconds_at(beat)?;
        Ok(p::FeatureSample::PitchClass(
            self.harmony
                .iter()
                .rev()
                .find(|(start, end, _)| seconds >= f64::from(*start) && seconds < f64::from(*end))
                .and_then(|(_, _, pitch)| *pitch),
        ))
    }
}

fn compare_reference(fixture: &Reference) {
    compare_reference_with_inputs(fixture, &BTreeMap::new());
}

fn compare_reference_with_inputs(fixture: &Reference, inputs: &BTreeMap<String, Value>) {
    let times = &fixture.times;
    let cells = &fixture.cells;
    let ids: Vec<_> = cells.iter().map(|c| c.id.clone()).collect();
    let clock = fixture.grid.timeline().unwrap();
    let features = Some({
        std::sync::Arc::new(ReferenceFeatures {
            timing: std::sync::Arc::new(fixture.grid.timing().unwrap()),
            onsets: fixture.onsets.clone(),
            clock: clock.clone(),
            harmony: fixture.harmony.clone().unwrap_or_default(),
            audio: fixture.audio.clone(),
        })
    });
    for case in &fixture.cases {
        let (name, graph, expected, views) = (&case.name, &case.graph, &case.frames, &case.views);
        let score = convert(graph, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        let library = score.library(&p::standard_library()).unwrap();
        let frame = p::Frame {
            cells,
            features: None,
            beat: fixture.clip_start,
            clip_start: fixture.clip_start,
            clip_duration: fixture.clip_duration,
            seed: 0,
        };
        let prepared = p::PreparedGraph::new(&library, ROOT, inputs, frame)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let prepared = if let Some(features) = &features {
            prepared
                .with_features(features.clone())
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        } else {
            prepared
        };
        let clip = p::Clip {
            graph: ROOT.into(),
            start: fixture.clip_start,
            duration: fixture.clip_duration,
            seed: 0,
            selection_seed: None,
            selection: p::Selection::all(),
            z_index: 0,
            blend_mode: p::BlendMode::Replace,
            inputs: inputs.clone(),
        };
        let plan = crate::eval::lighting::compile_clip(
            &clip,
            clock.clone(),
            cells.clone(),
            prepared.clone(),
            "lighting",
        )
        .unwrap();
        let host = crate::eval::try_eval(&plan, times, &mut crate::eval::Arena::default()).unwrap();
        // The public saved-pattern entry point must reach the same canonical
        // engine as score playback, including resident analysis and overrides.
        let audio = |recorded: &RecordedAudio| crate::eval::ResidentAudio {
            samples: std::sync::Arc::new(recorded.samples.clone()),
            sample_rate: recorded.sample_rate,
        };
        let args = inputs.iter().map(|(id, value)| {
            let wire = if graph.args.iter().any(|arg| arg.id == *id && arg.arg_type == PatternArgType::Palette) {
                let Value::Gradient(gradient) = value else { panic!("palette override needs a gradient"); };
                serde_json::json!({"colors": gradient.stops.iter().map(|stop| serde_json::json!({
                    "r": stop.color[0]*255., "g": stop.color[1]*255., "b": stop.color[2]*255., "a": stop.alpha
                })).collect::<Vec<_>>()})
            } else { crate::node_graph::lighting::wire_value(value) };
            (id.clone(), wire)
        }).collect();
        let imported = crate::eval::compile::compile_pattern(
            graph,
            &args,
            crate::eval::ResidentContext {
                seed: 0,
                positions: cells.iter().map(|c| c.world.map(|v| v as f32)).collect(),
                beat_grid: Some(fixture.grid.clone()),
                span: plan.ctx.span,
                drum_onsets: fixture.onsets.clone().into_iter().collect(),
                chord_sections: fixture.harmony.clone().unwrap_or_default(),
                audio: fixture.audio.get(&p::AudioSource::Mix).map(audio),
                stems: fixture
                    .audio
                    .iter()
                    .filter(|(source, _)| **source != p::AudioSource::Mix)
                    .map(|(source, samples)| (source.name().into(), audio(samples)))
                    .collect(),
            },
            ids.clone(),
        )
        .unwrap_or_else(|e| panic!("{name}: public import: {e:?}"));
        let imported_frames =
            crate::eval::try_eval(&imported, times, &mut crate::eval::Arena::default())
                .unwrap_or_else(|e| panic!("{name}: imported playback: {e}"));
        for (a, b) in imported_frames.iter().zip(&host) {
            for (id, a) in &a.primitives {
                let b = &b.primitives[id];
                for (x, y) in a
                    .color
                    .into_iter()
                    .chain(a.position)
                    .chain([a.dimmer, a.strobe, a.speed])
                    .zip(
                        b.color
                            .into_iter()
                            .chain(b.position)
                            .chain([b.dimmer, b.strobe, b.speed]),
                    )
                {
                    assert!(
                        (x - y).abs() < 3e-4,
                        "{name}: import/score mismatch {x} vs {y}"
                    );
                }
            }
        }

        let beats: Vec<_> = times
            .iter()
            .map(|t| clock.beat_at(*t as f64).unwrap())
            .collect();
        let batch = prepared
            .evaluate_batch(&beats)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let actual = batch["lighting"].lighting().unwrap();
        // Compare the numerical graph before output clamping as well as
        // final fixture capabilities, so saturation cannot hide a mismatch.
        for (id, view) in views {
            let signal = batch[&format!("view/{id}")].signal().unwrap();
            let values = signal.values();
            assert_eq!(values.dim().2, view.c, "{name}: channel count");
            for n in 0..view.n {
                let row = signal
                    .fixtures()
                    .map(|domain| {
                        domain
                            .iter()
                            .position(|id| id == &ids[n])
                            .expect("reference fixture in the result domain")
                    })
                    .unwrap_or(0);
                for t in 0..view.t {
                    for c in 0..view.c {
                        let a = values[[row, t.min(values.dim().1 - 1), c]];
                        let e = view.data[(n * view.t + t) * view.c + c] as f64;
                        assert!(
                            (a - e).abs()
                                < if fixture.audio.is_empty() {
                                    3e-5
                                } else {
                                    (e.abs() * 5e-5).max(2e-8)
                                },
                            "{name}, raw [{n},{t},{c}]: {a} != {e}"
                        );
                    }
                }
            }
        }
        for (t, expected) in expected.iter().enumerate() {
            let sample = actual.sample(t).unwrap();
            let sought = prepared.evaluate_batch(&[beats[t]]).unwrap();
            assert_eq!(
                sought["lighting"].lighting().unwrap().sample(0).unwrap(),
                sample,
                "{name}: batch changed a seek result"
            );
            for id in &ids {
                let a = &sample[id];
                let e = &expected.primitives[id];
                for (channel, (a, e)) in a
                    .color
                    .unwrap_or([1.; 3])
                    .into_iter()
                    .chain([
                        a.dimmer.unwrap_or(0.),
                        a.strobe.unwrap_or(0.),
                        a.speed.unwrap_or(1.),
                        a.position.unwrap_or([0.; 2])[0],
                        a.position.unwrap_or([0.; 2])[1],
                    ])
                    .zip(
                        e.color
                            .into_iter()
                            .map(|v| v.clamp(0., 1.))
                            // The output bundle retains positive headroom;
                            // the compositor bounds it after master intensity.
                            .chain([
                                e.dimmer.max(0.),
                                e.strobe,
                                e.speed,
                                e.position[0],
                                e.position[1],
                            ])
                            .map(f64::from),
                    )
                    .enumerate()
                {
                    assert!(
                        (a - e).abs() < 3e-5,
                        "{name}, time {t}, channel {channel}: {a} != {e}"
                    );
                }
            }
            compare_master_response(name, &host[t], expected, &plan.outputs);
        }
    }
}

fn compare_master_response(
    name: &str,
    actual: &UniverseState,
    expected: &UniverseState,
    bindings: &crate::eval::OutputBinding,
) {
    use crate::eval::composite::{blank_frame, composite_frame};
    for master in [0., 0.25, 0.7, 1.] {
        for layered in [false, true] {
            let mut base = blank_frame();
            if layered {
                base.primitives = expected
                    .primitives
                    .keys()
                    .map(|id| {
                        (
                            id.clone(),
                            crate::models::universe::PrimitiveState {
                                color: [0.2, 0.4, 0.7],
                                dimmer: 0.3,
                                position: [0.; 2],
                                strobe: 0.,
                                speed: 1.,
                            },
                        )
                    })
                    .collect();
            }
            let mut old = base.clone();
            let mut new = base;
            composite_frame(
                &mut old,
                expected,
                bindings,
                p::BlendMode::Replace,
                master,
                None,
            );
            composite_frame(
                &mut new,
                actual,
                bindings,
                p::BlendMode::Replace,
                master,
                None,
            );
            for (id, value) in &new.primitives {
                let reference = &old.primitives[id];
                assert!(
                    (value.dimmer - reference.dimmer).abs() < 3e-5,
                    "{name}: master {master}, layered {layered}: {} != {}",
                    value.dimmer,
                    reference.dimmer
                );
                // The historical compositor admitted negative chromaticity.
                // The new output clamps it at the physical capability boundary.
                // For ordinary colors also check the opacity of a layer, not
                // just its dimmer value in isolation.
                if expected.primitives[id]
                    .color
                    .iter()
                    .all(|v| (0.0..=1.0).contains(v))
                {
                    for (a, b) in value.color.iter().zip(reference.color) {
                        assert!(
                            (a - b).abs() < 3e-5,
                            "{name}: master {master}, layered {layered}: color {a} != {b}"
                        );
                    }
                }
            }
        }
    }
}
