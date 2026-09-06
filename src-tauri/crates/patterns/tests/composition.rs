use luma_patterns::*;
use std::collections::BTreeMap;

fn bar() -> Mapping {
    Mapping::linear(
        (0..32).map(|n| (format!("bar/head-{n}"), f64::from(n))),
        false,
    )
    .unwrap()
}
fn cells() -> &'static [Cell] {
    static CELLS: std::sync::OnceLock<Vec<Cell>> = std::sync::OnceLock::new();
    CELLS.get_or_init(|| {
        (0..32)
            .map(|n| Cell {
                id: format!("bar/head-{n}"),
                group: "bar".into(),
                world: [0.0, 0.0, f64::from(n)],
                uvz: [0.0, 0.0, f64::from(n)],
            })
            .collect()
    })
}
fn frame(beat: f64) -> Frame<'static> {
    Frame {
        features: None,
        cells: cells(),
        beat,
        clip_start: 8.0,
        clip_duration: 4.0,
        seed: 42,
    }
}
fn inputs() -> BTreeMap<String, Value> {
    BTreeMap::from([(
        "mapping".into(),
        Value::Mapping(MappingSpec {
            source: MappingSource::Z,
            per_group: false,
            reverse: false,
        }),
    )])
}
fn light(
    lib: &Library,
    id: &str,
    values: &BTreeMap<String, Value>,
    beat: f64,
) -> BTreeMap<String, [f64; 3]> {
    let result = lib.evaluate(id, values, frame(beat)).unwrap();
    let Value::Lighting(l) = &result["lighting"] else {
        panic!()
    };
    l.iter().map(|(id, v)| (id.clone(), v.rgb())).collect()
}
#[test]
fn all_builtins_validate_and_only_lighting_graphs_are_score_candidates() {
    let lib = standard_library();
    for id in lib.definitions.keys() {
        lib.validate(id).unwrap();
    }
    assert!(lib.definitions["chase"].playable());
    assert!(lib.definitions["dissolve_flash"].playable());
    assert!(!lib.definitions["chase_mask"].playable());
    assert!(matches!(lib.definitions["chase"].body, Body::Graph(_)));
    assert!(matches!(lib.definitions["chase_mask"].body, Body::Graph(_)));
    assert!(matches!(lib.definitions["pill"].body, Body::Graph(_)));
    assert!(matches!(
        lib.definitions["dissolve_mask"].body,
        Body::Graph(_)
    ));
    assert!(matches!(
        lib.definitions["multiply_mask"].body,
        Body::Graph(_)
    ));
}
#[test]
fn chase_rest_is_dark_and_seeking_is_deterministic() {
    let lib = standard_library();
    let i = inputs();
    let mid = light(&lib, "chase", &i, 9.0);
    assert!(mid.values().any(|c| c[0] > 0.0));
    for t in [10.0, 10.5, 11.999] {
        assert!(light(&lib, "chase", &i, t).values().all(|c| c[0] == 0.0));
    }
    assert_eq!(mid, light(&lib, "chase", &i, 9.0));
    assert_eq!(mid, light(&lib, "chase", &i, 13.0));
}
#[test]
fn circle_wrap_preserves_the_two_halves_of_a_pill() {
    let m = Mapping::circle(
        (0..8).map(|n| (n.to_string(), f64::from(n) / 8.0)),
        0.0,
        false,
    )
    .unwrap();
    let wrapped = pill(
        &m,
        0.0,
        0.4,
        &Envelope {
            points: vec![[0., 1.], [1., 1.]],
        },
        Boundary::Natural,
    );
    assert_eq!(wrapped["0"], 1.0);
    assert_eq!(wrapped["1"], 1.0);
    assert_eq!(wrapped["7"], 1.0);
    let clipped = pill(
        &m,
        0.0,
        0.4,
        &Envelope {
            points: vec![[0., 1.], [1., 1.]],
        },
        Boundary::Clip,
    );
    assert_eq!(clipped["7"], 0.0);
    assert_eq!(m.coordinates[7].position, 0.875);
    assert_eq!(
        wrapped,
        pill(
            &m,
            1.0,
            0.4,
            &Envelope {
                points: vec![[0., 1.], [1., 1.]]
            },
            Boundary::Natural
        )
    );
}
#[test]
fn entry_and_exit_account_for_the_entire_stroke() {
    let m = bar();
    for width in [0.1, 0.25, 0.8] {
        assert!(pill(
            &m,
            -width / 2.0,
            width,
            &Envelope {
                points: vec![[0., 1.], [1., 1.]]
            },
            Boundary::Clip
        )
        .values()
        .all(|v| *v == 0.0));
        assert!(pill(
            &m,
            1.0 + width / 2.0,
            width,
            &Envelope {
                points: vec![[0., 1.], [1., 1.]]
            },
            Boundary::Clip
        )
        .values()
        .all(|v| *v < 1e-12));
        assert_eq!(
            pill(
                &m,
                0.0,
                width,
                &Envelope {
                    points: vec![[0., 1.], [1., 1.]]
                },
                Boundary::Clip
            )["bar/head-0"],
            1.0
        );
    }
}
#[test]
fn dissolve_operates_per_head_and_is_monotone_not_flicker() {
    let lib = standard_library();
    let evaluate = |progress, softness, cells: &[Cell]| {
        let values = BTreeMap::from([
            ("coverage".into(), Value::Proportion(1.0 - progress)),
            ("softness".into(), Value::Proportion(softness)),
        ]);
        let result = lib
            .evaluate(
                "dissolve_mask",
                &values,
                Frame {
                    features: None,
                    cells,
                    ..frame(9.0)
                },
            )
            .unwrap();
        let Value::Mask(mask) = result["mask"].clone() else {
            panic!()
        };
        mask
    };
    for softness in [0.0, 0.2, 1.0] {
        let mut previous = evaluate(0.0, softness, cells());
        assert!(previous.values().all(|v| *v == 1.0));
        for step in 1..=100 {
            let next = evaluate(f64::from(step) / 100.0, softness, cells());
            for (id, value) in &next {
                assert!(*value <= previous[id]);
            }
            previous = next;
        }
        assert!(previous.values().all(|v| *v == 0.0));
    }
    let middle = evaluate(0.5, 0.0, cells());
    assert!(middle.values().any(|v| *v == 0.0));
    assert!(middle.values().any(|v| *v == 1.0));
    let mut reordered = cells().to_vec();
    reordered.reverse();
    assert_eq!(middle, evaluate(0.5, 0.0, &reordered));
}
#[test]
fn dissolve_reseeding_changes_between_strokes_only_when_requested() {
    let lib = standard_library();
    let mut i = BTreeMap::new();
    let first = light(&lib, "dissolve_flash", &i, 9.0);
    assert_ne!(first, light(&lib, "dissolve_flash", &i, 13.0));
    i.insert("reseed".into(), Value::Boolean(false));
    assert_eq!(
        light(&lib, "dissolve_flash", &i, 9.0),
        light(&lib, "dissolve_flash", &i, 13.0)
    );
    assert!(light(&lib, "dissolve_flash", &i, 8.0)
        .values()
        .all(|c| c[0] == 1.0));
    assert!(light(&lib, "dissolve_flash", &i, 10.0)
        .values()
        .all(|c| c[0] == 0.0));
}
#[test]
fn nested_pattern_exposes_inputs_without_flattening_their_types() {
    let mut lib = standard_library();
    let chase = &lib.definitions["chase"];
    let exposed_inputs = chase.inputs.clone();
    let outputs = chase.outputs.clone();
    let node = Node {
        position: None,
        definition: "chase".into(),
        inputs: exposed_inputs
            .keys()
            .map(|k| (k.clone(), Binding::Input { input: k.clone() }))
            .collect(),
    };
    lib.definitions.insert(
        "score-local".into(),
        Definition {
            name: "My chase".into(),
            inputs: exposed_inputs,
            outputs,
            body: Body::Graph(Graph {
                nodes: BTreeMap::from([("chase".into(), node)]),
                outputs: BTreeMap::from([(
                    "lighting".into(),
                    Binding::Connection {
                        node: "chase".into(),
                        output: "lighting".into(),
                    },
                )]),
            }),
        },
    );
    assert_eq!(
        light(&lib, "score-local", &inputs(), 9.0),
        light(&lib, "chase", &inputs(), 9.0)
    );
}
#[test]
fn rejects_units_missing_targets_invalid_values_and_implicit_overlap() {
    let lib = standard_library();
    assert!(lib
        .evaluate("pill", &BTreeMap::new(), frame(9.0))
        .unwrap_err()
        .0
        .contains("mapping"));
    let mut i = inputs();
    i.insert("travel".into(), Value::Number(2.0));
    assert!(lib
        .evaluate("chase", &i, frame(9.0))
        .unwrap_err()
        .0
        .contains("expected Beats"));
    i.insert("travel".into(), Value::Beats(8.0));
    assert!(lib
        .evaluate("chase", &i, frame(9.0))
        .unwrap_err()
        .0
        .contains("overlap"));
    i.insert("travel".into(), Value::Beats(f64::NAN));
    assert!(lib.evaluate("chase", &i, frame(9.0)).is_err());
}
#[test]
fn rejects_signal_to_fixed_input_and_definition_cycles() {
    let mut lib = standard_library();
    let Body::Graph(g) = &mut lib.definitions.get_mut("chase_mask").unwrap().body else {
        panic!()
    };
    g.nodes.get_mut("motion").unwrap().inputs.insert(
        "travel".into(),
        Binding::Connection {
            node: "rhythm".into(),
            output: "elapsed".into(),
        },
    );
    assert!(lib.validate("chase").unwrap_err().0.contains("fixed input"));
    let mut lib = standard_library();
    let Body::Graph(g) = &mut lib.definitions.get_mut("chase_mask").unwrap().body else {
        panic!()
    };
    g.nodes.get_mut("pill").unwrap().definition = "chase_mask".into();
    assert!(lib.validate("chase").unwrap_err().0.contains("recursive"));
}
#[test]
fn saved_graphs_roundtrip_without_losing_rich_values() {
    let lib = standard_library();
    let encoded = serde_json::to_string(&lib).unwrap();
    let restored: Library = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        light(&lib, "chase", &inputs(), 9.0),
        light(&restored, "chase", &inputs(), 9.0)
    );
    let result = restored
        .evaluate(
            "envelope",
            &BTreeMap::from([
                (
                    "shape".into(),
                    Value::Envelope(Envelope {
                        points: vec![[0.0, 0.0], [0.25, 1.0], [1.0, 0.0]],
                    }),
                ),
                ("progress".into(), Value::Proportion(0.25)),
            ]),
            frame(9.0),
        )
        .unwrap();
    assert_eq!(result["value"], Value::Proportion(1.0));
}

#[test]
fn angled_wings_each_get_their_own_principal_progression() {
    let cells: Vec<_> = (0..2)
        .flat_map(|wing| {
            (0..5).map(move |n| {
                let t = f64::from(n);
                let sign = if wing == 0 { -1.0 } else { 1.0 };
                Cell {
                    id: format!("wing-{wing}/cell-{n}"),
                    group: format!("wing-{wing}"),
                    world: [sign * t, t, 0.0],
                    uvz: [sign * t, t, 0.0],
                }
            })
        })
        .collect();
    let m = MappingSpec {
        source: MappingSource::MajorAxis {
            toward: [0.0, 1.0, 0.0],
        },
        per_group: true,
        reverse: false,
    }
    .resolve(&cells)
    .unwrap();
    for c in &m.coordinates {
        let n = c.cell.rsplit('-').next().unwrap().parse::<f64>().unwrap();
        assert!((c.position - n / 4.0).abs() < 1e-9);
    }
    let reversed = MappingSpec {
        source: MappingSource::MajorAxis {
            toward: [0.0, 1.0, 0.0],
        },
        per_group: true,
        reverse: true,
    }
    .resolve(&cells)
    .unwrap();
    for (a, b) in m.coordinates.iter().zip(&reversed.coordinates) {
        assert!((a.position + b.position - 1.0).abs() < 1e-9);
    }
}
#[test]
fn dissolve_mask_can_modulate_chase_without_a_new_effect_kernel() {
    let mut lib = standard_library();
    let source = lib.definitions["chase_mask"].clone();
    let graph = Graph {
        nodes: BTreeMap::from([
            (
                "chase".into(),
                Node {
                    position: None,
                    definition: "chase_mask".into(),
                    inputs: source
                        .inputs
                        .keys()
                        .map(|k| (k.clone(), Binding::Input { input: k.clone() }))
                        .collect(),
                },
            ),
            (
                "dissolve".into(),
                Node {
                    position: None,
                    definition: "dissolve_mask".into(),
                    inputs: BTreeMap::from([("coverage".into(), Value::Proportion(0.5).into())]),
                },
            ),
            (
                "combine".into(),
                Node {
                    position: None,
                    definition: "multiply_mask".into(),
                    inputs: BTreeMap::from([
                        (
                            "a".into(),
                            Binding::Connection {
                                node: "chase".into(),
                                output: "mask".into(),
                            },
                        ),
                        (
                            "b".into(),
                            Binding::Connection {
                                node: "dissolve".into(),
                                output: "mask".into(),
                            },
                        ),
                    ]),
                },
            ),
            (
                "color".into(),
                Node {
                    position: None,
                    definition: "appearance".into(),
                    inputs: BTreeMap::from([(
                        "mask".into(),
                        Binding::Connection {
                            node: "combine".into(),
                            output: "mask".into(),
                        },
                    )]),
                },
            ),
        ]),
        outputs: BTreeMap::from([(
            "lighting".into(),
            Binding::Connection {
                node: "color".into(),
                output: "lighting".into(),
            },
        )]),
    };
    lib.definitions.insert(
        "dissolving-chase".into(),
        Definition {
            name: "Dissolving chase".into(),
            inputs: source.inputs,
            outputs: lib.definitions["appearance"].outputs.clone(),
            body: Body::Graph(graph),
        },
    );
    let actual = light(&lib, "dissolving-chase", &inputs(), 9.0);
    let chase = light(&lib, "chase", &inputs(), 9.0);
    let mask = lib
        .evaluate(
            "dissolve_mask",
            &BTreeMap::from([("coverage".into(), Value::Proportion(0.5))]),
            frame(9.0),
        )
        .unwrap();
    let Value::Mask(mask) = &mask["mask"] else {
        panic!()
    };
    for (id, color) in actual {
        assert_eq!(color, chase[&id].map(|v| v * mask[&id]));
    }
}

#[test]
fn score_insertion_creates_a_local_pattern_and_clip_edits_do_not_mutate_the_graph() {
    let lib = standard_library();
    let mut score = Score::default();
    score
        .insert_effect(&lib, "chase", "local-1", 8.0, 8.0)
        .unwrap();
    let original = serde_json::to_value(&score.definitions).unwrap();
    score
        .clips
        .get_mut("local-1")
        .unwrap()
        .inputs
        .insert("color".into(), Value::Color([0.0, 0.0, 1.0]));
    let saved = score.to_json(&lib).unwrap();
    let restored = Score::from_json(&lib, &saved).unwrap();
    assert_eq!(
        original,
        serde_json::to_value(&restored.definitions).unwrap()
    );
    let out = restored
        .evaluate_clip(&lib, "local-1", &inputs(), 9.0, cells())
        .unwrap();
    let Value::Lighting(l) = &out["lighting"] else {
        panic!()
    };
    assert!(l.values().any(|c| c.rgb()[2] > 0.0));
    assert!(l.values().all(|c| c.rgb()[0] == 0.0));
    assert!(restored
        .evaluate_clip(&lib, "local-1", &inputs(), 16.0, cells())
        .unwrap()
        .is_empty());
    assert!(score
        .insert_effect(&lib, "chase_mask", "mask", 8.0, 8.0)
        .is_err());
}

#[test]
fn score_persists_mapping_choices_and_rejects_resolved_cell_snapshots() {
    let lib = standard_library();
    let mut score = Score::default();
    score
        .insert_effect(&lib, "chase", "local", 0.0, 8.0)
        .unwrap();
    score.clips.get_mut("local").unwrap().inputs.insert(
        "mapping".into(),
        Value::Mapping(MappingSpec {
            source: MappingSource::Circle { origin: 0.25 },
            per_group: true,
            reverse: true,
        }),
    );
    let json = score.to_json(&lib).unwrap();
    assert!(!json.contains("bar/head"));
    Score::from_json(&lib, &json).unwrap();
    score
        .clips
        .get_mut("local")
        .unwrap()
        .inputs
        .insert("mapping".into(), Value::Coordinates(bar()));
    assert!(score
        .to_json(&lib)
        .unwrap_err()
        .0
        .contains("resolved cell values"));
}

#[test]
fn prepared_graph_matches_interpreter_when_seeking_across_strokes() {
    let library = standard_library();
    for name in ["chase", "dissolve_flash"] {
        let overrides = if name == "chase" {
            inputs()
        } else {
            BTreeMap::new()
        };
        let prepared = PreparedGraph::new(&library, name, &overrides, frame(8.0)).unwrap();
        for beat in [8.0, 9.3, 11.0, 8.2, 15.999, 12.1] {
            assert_eq!(
                prepared.evaluate(beat).unwrap(),
                library.evaluate(name, &overrides, frame(beat)).unwrap()
            );
        }
    }
}

#[test]
fn prepared_clip_owns_its_overrides_and_obeys_the_score_span() {
    let lib = standard_library();
    let mut score = Score::default();
    score
        .insert_effect(&lib, "chase", "local", 8.0, 4.0)
        .unwrap();
    score
        .clips
        .get_mut("local")
        .unwrap()
        .inputs
        .insert("width".into(), Value::Proportion(0.8));
    let prepared = score
        .prepare_clip(&lib, "local", &BTreeMap::new(), cells())
        .unwrap();
    let lit = prepared.evaluate(9.0).unwrap();
    assert!(prepared.evaluate(7.99).unwrap().is_empty());
    assert!(prepared.evaluate(12.0).unwrap().is_empty());
    score
        .clips
        .get_mut("local")
        .unwrap()
        .inputs
        .insert("width".into(), Value::Proportion(0.0));
    assert_eq!(prepared.evaluate(9.0).unwrap(), lit);
    assert_ne!(
        score
            .evaluate_clip(&lib, "local", &BTreeMap::new(), 9.0, cells())
            .unwrap(),
        lit
    );
    assert!(prepared.evaluate(f64::NAN).is_err());
    assert!(score
        .insert_effect(&lib, "chase", "overflow", f64::MAX, f64::MAX)
        .is_err());
    assert!(!score.clips.contains_key("overflow"));
}

#[test]
fn pill_softness_preserves_width_and_feathers_the_edge() {
    let mapping = Mapping::linear(
        [
            ("left".into(), 0.),
            ("edge".into(), 0.35),
            ("center".into(), 0.5),
            ("right".into(), 1.),
        ],
        false,
    )
    .unwrap();
    let hard = pill(
        &mapping,
        0.5,
        0.4,
        &Envelope {
            points: vec![[0., 1.], [1., 1.]],
        },
        Boundary::Clip,
    );
    let soft = pill(
        &mapping,
        0.5,
        0.4,
        &Envelope {
            points: vec![[0., 0.], [0.5, 1.], [1., 0.]],
        },
        Boundary::Clip,
    );
    assert_eq!(hard["edge"], 1.);
    assert!((soft["edge"] - 0.25).abs() < 1e-9);
    assert_eq!(soft["center"], 1.);
    assert_eq!(soft["left"], 0.);
    assert_eq!(soft["right"], 0.);
}

#[test]
fn spatial_envelope_is_signed_and_wraps_without_mirroring() {
    let mapping = Mapping::linear(
        [
            ("a".into(), 0.),
            ("b".into(), 0.1),
            ("c".into(), 0.9),
            ("d".into(), 1.),
        ],
        false,
    )
    .unwrap();
    let ramp = Envelope {
        points: vec![[0., 0.], [1., 1.]],
    };
    let result = pill(&mapping, 0., 0.4, &ramp, Boundary::Wrap);
    assert!((result["b"] - 0.75).abs() < 1e-9);
    assert!((result["c"] - 0.25).abs() < 1e-9);
    assert!((result["a"] - ramp.sample(0.5)).abs() < 1e-9);
    assert_eq!(result["a"], result["d"]);
}

#[test]
fn a_perpendicular_major_axis_hint_still_maps_a_horizontal_rig() {
    let cells: Vec<_> = (0..5)
        .map(|i| Cell {
            id: i.to_string(),
            group: "rig".into(),
            world: [i as f64, 0., 2.],
            uvz: [i as f64, 0., 2.],
        })
        .collect();
    let map = MappingSpec {
        source: MappingSource::MajorAxis {
            toward: [0., 0., 1.],
        },
        per_group: false,
        reverse: false,
    }
    .resolve(&cells)
    .unwrap();
    assert_eq!(
        map.coordinates
            .iter()
            .map(|c| c.position)
            .collect::<Vec<_>>(),
        vec![0., 0.25, 0.5, 0.75, 1.]
    );
}

#[test]
fn clips_share_graphs_until_made_independent_including_local_dependencies() {
    let lib = standard_library();
    let mut score = Score::default();
    score
        .insert_effect(&lib, "chase", "original", 0.0, 8.0)
        .unwrap();
    // Replace the built-in dependency with a local, editable copy.
    score
        .definitions
        .insert("local_chase".into(), lib.definitions["chase"].clone());
    let Body::Graph(graph) = &mut score.definitions.get_mut("original").unwrap().body else {
        panic!()
    };
    graph.nodes.get_mut("effect").unwrap().definition = "local_chase".into();
    let mut repeat = score.clips["original"].clone();
    repeat.start = 16.0;
    score.clips.insert("repeat".into(), repeat);
    score
        .make_independent(&lib, "repeat", "independent")
        .unwrap();
    assert_eq!(score.clips["original"].graph, "original");
    assert_eq!(score.clips["repeat"].graph, "independent");
    let Body::Graph(graph) = &score.definitions["independent"].body else {
        panic!()
    };
    let dependency = &graph.nodes["effect"].definition;
    assert_ne!(dependency, "local_chase");
    assert!(score.definitions.contains_key(dependency));
    score.validate(&lib).unwrap();
    let json = score.to_json(&lib).unwrap();
    assert!(!json.contains("\"patterns\""));
    let restored = Score::from_json(&lib, &json).unwrap();
    assert_eq!(restored.clips["repeat"].graph, "independent");
    // A collision fails without leaving partially cloned definitions.
    let before = score.to_json(&lib).unwrap();
    assert!(score.make_independent(&lib, "repeat", "original").is_err());
    assert_eq!(before, score.to_json(&lib).unwrap());
}

#[test]
fn fixture_output_writers_preserve_unset_capabilities() {
    let lib = standard_library();
    for (id, expected) in [
        ("write_position", [false, false, true, false, false]),
        ("write_dimmer", [false, true, false, false, false]),
        ("write_strobe", [false, false, false, true, false]),
        ("write_speed", [false, false, false, false, true]),
        ("chase", [true, true, false, false, false]),
    ] {
        let output = lib.evaluate(id, &BTreeMap::new(), frame(9.0)).unwrap();
        let Value::Lighting(values) = &output["lighting"] else {
            panic!()
        };
        assert!(values.values().all(|v| v.writes() == expected), "{id}");
    }
}

#[test]
fn internal_layering_preserves_color_when_only_movement_is_written() {
    let mut output = FixtureOutput::from_rgb([0.2, 0.4, 0.8]);
    let original = output.rgb();
    output.composite(
        &FixtureOutput {
            position: Some([0.0, 0.0]),
            ..Default::default()
        },
        BlendMode::Replace,
    );
    assert_eq!(output.rgb(), original);
    assert_eq!(output.position, Some([0.0, 0.0]));
    assert!(output.strobe.is_none());
    output.composite(
        &FixtureOutput {
            dimmer: Some(0.0),
            ..Default::default()
        },
        BlendMode::Replace,
    );
    assert_eq!(output.rgb(), [0.0; 3]);
    assert_eq!(output.position, Some([0.0, 0.0]));
}

#[test]
fn refreshing_dissolve_holds_each_interval_and_replays_after_seeking() {
    let lib = standard_library();
    let overrides = BTreeMap::from([
        ("coverage".into(), Value::Proportion(0.5)),
        ("refresh".into(), Value::Boolean(true)),
        ("refresh_every".into(), Value::Beats(0.125)),
    ]);
    let prepared = PreparedGraph::new(&lib, "dissolve_mask", &overrides, frame(8.0)).unwrap();
    let first = prepared.evaluate(8.0).unwrap();
    assert_eq!(first, prepared.evaluate(8.124).unwrap());
    let next = prepared.evaluate(8.125).unwrap();
    assert_ne!(first, next);
    prepared.evaluate(123.0).unwrap();
    assert_eq!(first, prepared.evaluate(8.0).unwrap());
    assert_eq!(next, prepared.evaluate(8.2).unwrap());
    for beat in [8.0, 8.125, 9.0, 12.01] {
        assert_eq!(
            prepared.evaluate(beat).unwrap(),
            lib.evaluate("dissolve_mask", &overrides, frame(beat))
                .unwrap()
        );
    }
}

#[test]
fn graph_pill_matches_the_previous_spatial_kernel() {
    let lib = standard_library();
    let mut mapping = bar();
    for closed in [false, true] {
        for c in &mut mapping.coordinates {
            c.closed = closed;
        }
        for boundary in [Boundary::Clip, Boundary::Wrap, Boundary::Natural] {
            for width in [0.0, 0.1, 0.25, 0.8, 1.0] {
                for center in [
                    -0.5,
                    -width / 2.0,
                    0.0,
                    0.173,
                    0.5,
                    1.0,
                    1.0 + width / 2.0,
                    2.0,
                ] {
                    for shape in [
                        Envelope::soft_edges(0.0),
                        Envelope::soft_edges(0.1),
                        Envelope {
                            points: vec![[0.0, 0.0], [1.0, 1.0]],
                        },
                    ] {
                        let inputs = BTreeMap::from([
                            ("mapping".into(), Value::Coordinates(mapping.clone())),
                            ("position".into(), Value::Position(center)),
                            ("width".into(), Value::Proportion(width)),
                            ("shape".into(), Value::Envelope(shape.clone())),
                            ("boundary".into(), Value::Boundary(boundary)),
                        ]);
                        let result = lib.evaluate("pill", &inputs, frame(9.0)).unwrap();
                        let Value::Mask(actual) = &result["mask"] else {
                            panic!()
                        };
                        let expected = reference_pill(&mapping, center, width, &shape, boundary);
                        for (id, value) in actual {
                            assert!((value - expected[id]).abs() < 1e-10, "{id}: {value} != {} at center {center}, width {width}, boundary {boundary:?}", expected[id]);
                        }
                    }
                }
            }
        }
    }
}

fn reference_pill(
    mapping: &Mapping,
    position: f64,
    width: f64,
    shape: &Envelope,
    boundary: Boundary,
) -> BTreeMap<String, f64> {
    mapping
        .coordinates
        .iter()
        .map(|c| {
            let wrap = boundary == Boundary::Wrap || (boundary == Boundary::Natural && c.closed);
            let offset = if wrap {
                (c.position - position + 0.5).rem_euclid(1.0) - 0.5
            } else {
                c.position - position
            };
            let half = width * 0.5;
            let coverage = if width <= 0.0
                || (!(wrap && width >= 1.0)
                    && offset.abs() + 8.0 * f64::EPSILON * position.abs().max(1.0) >= half)
            {
                0.0
            } else {
                shape.sample(offset / width + 0.5)
            };
            (c.cell.clone(), coverage)
        })
        .collect()
}

fn pill(
    mapping: &Mapping,
    position: f64,
    width: f64,
    shape: &Envelope,
    boundary: Boundary,
) -> BTreeMap<String, f64> {
    let cells: Vec<_> = mapping
        .coordinates
        .iter()
        .map(|c| Cell {
            id: c.cell.clone(),
            group: "all".into(),
            world: [0.0, 0.0, c.position],
            uvz: [0.0, 0.0, c.position],
        })
        .collect();
    let output = standard_library()
        .evaluate(
            "pill",
            &BTreeMap::from([
                ("mapping".into(), Value::Coordinates(mapping.clone())),
                ("position".into(), Value::Position(position)),
                ("width".into(), Value::Proportion(width)),
                ("shape".into(), Value::Envelope(shape.clone())),
                ("boundary".into(), Value::Boundary(boundary)),
            ]),
            Frame {
                features: None,
                cells: &cells,
                ..frame(9.0)
            },
        )
        .unwrap();
    let Value::Mask(mask) = output["mask"].clone() else {
        panic!()
    };
    mask
}

#[test]
fn dissolve_flash_uses_the_shared_envelope_and_keeps_the_rest_dark() {
    let library = standard_library();
    let inputs = BTreeMap::from([("shape".into(), Value::Envelope(Envelope::soft_edges(0.0)))]);
    assert!(light(&library, "dissolve_flash", &inputs, 9.9)
        .values()
        .all(|rgb| *rgb == [1.0; 3]));
    assert!(light(&library, "dissolve_flash", &inputs, 10.0)
        .values()
        .all(|rgb| *rgb == [0.0; 3]));
    assert!(light(&library, "dissolve_flash", &inputs, 11.9)
        .values()
        .all(|rgb| *rgb == [0.0; 3]));
}

#[test]
fn preparation_rejects_duplicate_head_identities_for_every_effect() {
    let library = standard_library();
    let duplicate = vec![cells()[0].clone(), cells()[0].clone()];
    for id in ["chase", "dissolve_flash", "write_strobe"] {
        let frame = Frame {
            features: None,
            cells: &duplicate,
            ..frame(8.0)
        };
        assert!(PreparedGraph::new(&library, id, &BTreeMap::new(), frame)
            .unwrap_err()
            .0
            .contains("unique"));
        assert!(library
            .evaluate(id, &BTreeMap::new(), frame)
            .unwrap_err()
            .0
            .contains("unique"));
    }
}
