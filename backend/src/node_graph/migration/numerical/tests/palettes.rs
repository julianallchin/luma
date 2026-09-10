use super::*;

#[test]
fn shared_empty_and_single_color_palettes_match_the_original_consumers() {
    let fixture: Reference =
        serde_json::from_str(include_str!("../../fixtures/shared-palettes-v1.json")).unwrap();
    assert_eq!(fixture.cases.len(), 10);
    compare_reference(&fixture);
}

#[test]
fn shared_empty_overrides_keep_fallback_behavior_after_renaming_and_serialization() {
    let mut fixture: Reference =
        serde_json::from_str(include_str!("../../fixtures/shared-palettes-v1.json")).unwrap();
    fixture.cases.retain(|case| case.name.starts_with("empty-"));
    assert_eq!(fixture.cases.len(), 4);
    for case in &mut fixture.cases {
        let arg = &mut case.graph.args[0];
        let value = argument_value(&arg.arg_type, ValueType::Gradient, &arg.default_value).unwrap();
        assert_eq!(value, Value::Gradient(p::Gradient { stops: vec![] }));
        arg.default_value = match arg.arg_type {
            PatternArgType::Palette => serde_json::json!({"colors":["#ffffff"]}),
            _ => serde_json::json!({"stops":[{"t":0.7,"color":"#ffffff"}]}),
        };
        let mut score = convert(&case.graph, &case.name).unwrap();
        score.clips.insert(
            "clip".into(),
            serde_json::from_value(serde_json::json!({
                "graph":ROOT,"start":0.,"duration":12.,"seed":0,"inputs":{"colors":value}
            }))
            .unwrap(),
        );
        score
            .edit_graph(
                &p::standard_library(),
                ROOT,
                p::GraphEdit::RenameInput {
                    key: "colors".into(),
                    name: "Accent colors".into(),
                },
            )
            .unwrap();
        let saved: p::Score = serde_json::from_value(serde_json::to_value(score).unwrap()).unwrap();
        assert_eq!(
            saved.definitions[ROOT].inputs["colors"].name,
            "Accent colors"
        );
        assert_eq!(saved.clips["clip"].inputs["colors"], value);
        let Body::Graph(graph) = &saved.definitions[ROOT].body else {
            panic!()
        };
        for node in ["a", "sample", "regions"] {
            assert_eq!(
                graph.nodes[node].inputs["stops"],
                B::Input {
                    input: "colors".into()
                }
            );
        }
    }
    // The defaults above are now white. Explicit empty clip values must still
    // select rainbow at harmonic consumers and black at ordinary samplers.
    compare_reference_with_inputs(
        &fixture,
        &BTreeMap::from([(
            "colors".into(),
            Value::Gradient(p::Gradient { stops: vec![] }),
        )]),
    );
}
