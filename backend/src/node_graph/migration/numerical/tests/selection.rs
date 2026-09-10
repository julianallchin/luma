use super::*;

#[test]
fn migrated_count_inputs_keep_sharing_renaming_and_saved_clip_overrides() {
    use crate::models::node_graph::{Edge, PatternArgDef};
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../../fixtures/selection-v1.json")).unwrap();
    let fixture = fixtures.iter().find(|f| f.cells.len() == 5).unwrap();
    let mut graph = fixture
        .cases
        .iter()
        .find(|c| c.name.ends_with("snare-2-true-random"))
        .unwrap()
        .graph
        .clone();
    let mut copy = graph
        .nodes
        .iter()
        .find(|n| n.id == "random")
        .unwrap()
        .clone();
    copy.id = "copy".into();
    graph.nodes.push(copy);
    graph.nodes.push(NodeInstance {
        id: "pattern_args".into(),
        type_id: "pattern_args".into(),
        params: Default::default(),
        position_x: None,
        position_y: None,
    });
    graph.args.push(PatternArgDef {
        id: "heads".into(),
        name: "Selected heads".into(),
        arg_type: PatternArgType::Scalar,
        default_value: serde_json::json!({"value":2.}),
    });
    for id in ["random", "copy"] {
        graph.edges.push(Edge {
            id: format!("count-{id}"),
            from_node: "pattern_args".into(),
            from_port: "heads".into(),
            to_node: id.into(),
            to_port: "count".into(),
        });
    }
    let mut trigger = graph
        .edges
        .iter()
        .find(|e| e.to_node == "random" && e.to_port == "events_in")
        .unwrap()
        .clone();
    trigger.id = "copy-trigger".into();
    trigger.to_node = "copy".into();
    graph.edges.push(trigger);
    let mut view = graph.nodes.iter().find(|n| n.id == "view").unwrap().clone();
    view.id = "copy-view".into();
    graph.nodes.push(view);
    graph.edges.push(Edge {
        id: "copy-view".into(),
        from_node: "copy".into(),
        from_port: "out".into(),
        to_node: "copy-view".into(),
        to_port: "in".into(),
    });
    let mut score = convert(&graph, "Held selections").unwrap();
    let base = p::standard_library();
    let library = score.library(&base).unwrap();
    score
        .definitions
        .get_mut(ROOT)
        .unwrap()
        .edit(
            &library,
            p::GraphEdit::RenameInput {
                key: "heads".into(),
                name: "Crowd size".into(),
            },
        )
        .unwrap();
    score.clips.insert(
        "clip".into(),
        p::Clip {
            graph: ROOT.into(),
            start: fixture.clip_start,
            duration: fixture.clip_duration,
            seed: 0,
            selection_seed: None,
            selection: p::Selection::all(),
            z_index: 0,
            blend_mode: p::BlendMode::Replace,
            inputs: BTreeMap::from([("heads".into(), Value::Number(3.))]),
        },
    );
    let score: p::Score = serde_json::from_str(&serde_json::to_string(&score).unwrap()).unwrap();
    score.validate(&base).unwrap();
    assert_eq!(score.definitions[ROOT].inputs["heads"].name, "Crowd size");
    let Body::Graph(body) = &score.definitions[ROOT].body else {
        panic!()
    };
    for id in ["random", "copy"] {
        assert_eq!(
            body.nodes[id].inputs["count"],
            B::Input {
                input: "heads".into()
            }
        );
    }
    let clock = fixture.grid.timeline().unwrap();
    let features = std::sync::Arc::new(ReferenceFeatures {
        clock: clock.clone(),
        timing: std::sync::Arc::new(fixture.grid.timing().unwrap()),
        onsets: fixture.onsets.clone(),
        harmony: vec![],
        audio: BTreeMap::new(),
    });
    let library = score.library(&base).unwrap();
    for (inputs, expected) in [
        (BTreeMap::new(), 2.),
        (score.clips["clip"].inputs.clone(), 3.),
    ] {
        let program = p::PreparedGraph::new(
            &library,
            ROOT,
            &inputs,
            p::Frame {
                cells: &fixture.cells,
                features: None,
                beat: 0.,
                clip_start: fixture.clip_start,
                clip_duration: fixture.clip_duration,
                seed: 0,
            },
        )
        .unwrap()
        .with_features(features.clone())
        .unwrap();
        let output = program
            .evaluate_batch(&[clock.beat_at(1.6).unwrap()])
            .unwrap();
        for port in ["view/view", "view/copy-view"] {
            assert_eq!(output[port].signal().unwrap().values().sum(), expected);
        }
    }
}

#[test]
fn held_random_selection_preserves_counts_and_boundaries_with_stateless_draws() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../../fixtures/selection-v1.json")).unwrap();
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 672);
    let mut changed_draws = 0;
    for fixture in &fixtures {
        let clock = fixture.grid.timeline().unwrap();
        let beats = fixture
            .times
            .iter()
            .map(|t| clock.beat_at(f64::from(*t)).unwrap())
            .collect::<Vec<_>>();
        let features = std::sync::Arc::new(ReferenceFeatures {
            clock: clock.clone(),
            timing: std::sync::Arc::new(fixture.grid.timing().unwrap()),
            onsets: fixture.onsets.clone(),
            harmony: vec![],
            audio: BTreeMap::new(),
        });
        for case in &fixture.cases {
            assert!(supports(&case.graph));
            let score =
                convert(&case.graph, &case.name).unwrap_or_else(|e| panic!("{}: {e}", case.name));
            let library = score.library(&p::standard_library()).unwrap();
            let prepare = |cells: &[p::Cell]| {
                p::PreparedGraph::new(
                    &library,
                    ROOT,
                    &BTreeMap::new(),
                    p::Frame {
                        cells,
                        features: None,
                        beat: fixture.clip_start,
                        clip_start: fixture.clip_start,
                        clip_duration: fixture.clip_duration,
                        seed: 0,
                    },
                )
                .unwrap_or_else(|e| panic!("{}: {e}", case.name))
                .with_features(features.clone())
                .unwrap()
            };
            let program = prepare(&fixture.cells);
            let batch = program.evaluate_batch(&beats).unwrap();
            let signal = batch["view/view"].signal().unwrap();
            let original = &case.views["view"];
            assert_eq!(signal.values().dim(), (original.n, original.t, original.c));
            assert!(signal.values().iter().all(|v| *v == 0. || *v == 1.));
            for (t, beat) in beats.iter().enumerate() {
                let expected_count: f32 = (0..original.n)
                    .map(|n| original.data[n * original.t + t])
                    .sum();
                let count: f64 = (0..original.n).map(|n| signal.values()[[n, t, 0]]).sum();
                assert_eq!(
                    count,
                    f64::from(expected_count),
                    "{} at {}",
                    case.name,
                    fixture.times[t]
                );
                let single = program.evaluate_batch(&[*beat]).unwrap();
                assert_eq!(
                    batch["lighting"].lighting().unwrap().sample(t).unwrap(),
                    single["lighting"].lighting().unwrap().sample(0).unwrap()
                );
                for n in 0..original.n {
                    changed_draws += usize::from(
                        signal.values()[[n, t, 0]] != f64::from(original.data[n * original.t + t]),
                    );
                }
            }
            if case.name.contains("snare") && !fixture.cells.is_empty() {
                let seconds = [1.5, 1.6, 2.5, 2.6, 3.5];
                let beats = seconds.map(|s| clock.beat_at(s).unwrap());
                let query = program.evaluate_batch(&beats).unwrap();
                let mask = query["view/view"].signal().unwrap();
                for n in 0..fixture.cells.len() {
                    assert_eq!(mask.values()[[n, 0, 0]], mask.values()[[n, 1, 0]]);
                    assert_eq!(mask.values()[[n, 2, 0]], mask.values()[[n, 3, 0]]);
                }
                let selector = case
                    .graph
                    .nodes
                    .iter()
                    .find(|n| n.type_id == "random_select_mask")
                    .unwrap();
                if selector.params["avoid_repeat"] == serde_json::json!(true) {
                    let count = selector.params["count"].as_f64().unwrap().round().max(0.) as usize;
                    let count = count.min(fixture.cells.len());
                    for (a, b) in [(0, 2), (2, 4)] {
                        let overlap = (0..fixture.cells.len())
                            .filter(|n| {
                                mask.values()[[*n, a, 0]] > 0. && mask.values()[[*n, b, 0]] > 0.
                            })
                            .count();
                        assert_eq!(
                            overlap,
                            (2 * count).saturating_sub(fixture.cells.len()),
                            "{}",
                            case.name
                        );
                    }
                }
            }
            let mut reversed = fixture.cells.clone();
            reversed.reverse();
            let reversed = prepare(&reversed).evaluate_batch(&beats).unwrap();
            for t in 0..beats.len() {
                assert_eq!(
                    batch["lighting"].lighting().unwrap().sample(t).unwrap(),
                    reversed["lighting"].lighting().unwrap().sample(t).unwrap()
                );
            }
        }
    }
    // Head identities intentionally change: draws use stable head IDs and a
    // shuffled cycle replaces the old recursive avoid-repeat chain.
    assert!(changed_draws > 100);
}
