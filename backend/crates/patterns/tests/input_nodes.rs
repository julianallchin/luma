use luma_patterns::*;
use std::collections::BTreeMap;

fn bind(key: &str, node: &str, input: &str) -> GraphEdit {
    GraphEdit::Bind {
        node: node.into(),
        input: input.into(),
        binding: Some(Binding::Input { input: key.into() }),
    }
}

#[test]
fn input_infers_a_shared_value_and_renames_without_losing_overrides() {
    let base = standard_library();
    let mut score = Score::default();
    score
        .insert_effect(&base, "wash", "custom", 0., 8.)
        .unwrap();
    let edit = |score: &mut Score, op| score.edit_graph(&base, "custom", op).unwrap();
    edit(
        &mut score,
        GraphEdit::AddInput {
            key: "accent".into(),
            name: "Accent".into(),
            position: [-200., 30.],
        },
    );
    assert!(
        !score.definitions["custom"].inputs.contains_key("accent"),
        "an unwired Input has no invented type"
    );
    score.validate(&base).unwrap();
    edit(
        &mut score,
        GraphEdit::Bind {
            node: "effect".into(),
            input: "color".into(),
            binding: Some(Value::Color([0.2, 0.4, 0.8]).into()),
        },
    );
    edit(&mut score, bind("accent", "effect", "color"));
    let spec = &score.definitions["custom"].inputs["accent"];
    assert_eq!(
        spec.value_type,
        ValueType::Signal(SignalType::new(Unit::Proportion, Channels::Rgb))
    );
    assert_eq!(spec.default, Some(Value::Color([0.2, 0.4, 0.8])));
    edit(
        &mut score,
        GraphEdit::Add {
            id: "other".into(),
            definition: "wash".into(),
        },
    );
    edit(&mut score, bind("accent", "other", "color"));
    let value = Value::Color([1., 0.5, 0.]);
    score
        .clips
        .get_mut("custom")
        .unwrap()
        .inputs
        .insert("accent".into(), value.clone());
    edit(
        &mut score,
        GraphEdit::RenameInput {
            key: "accent".into(),
            name: "Snare color".into(),
        },
    );
    assert_eq!(
        score.definitions["custom"].inputs["accent"].name,
        "Snare color"
    );
    assert_eq!(score.clips["custom"].inputs["accent"], value);
    score.validate(&base).unwrap();

    // Removing a wire leaves its named Input available for reconnection.
    edit(
        &mut score,
        GraphEdit::Bind {
            node: "effect".into(),
            input: "color".into(),
            binding: None,
        },
    );
    edit(
        &mut score,
        GraphEdit::Bind {
            node: "other".into(),
            input: "color".into(),
            binding: None,
        },
    );
    assert!(score.definitions["custom"].inputs.contains_key("accent"));
    assert!(score.clips["custom"].inputs.contains_key("accent"));
    edit(&mut score, bind("accent", "effect", "color"));
    edit(
        &mut score,
        GraphEdit::RemoveInput {
            key: "accent".into(),
        },
    );
    assert!(!score.definitions["custom"].inputs.contains_key("accent"));
    assert!(!score.clips["custom"].inputs.contains_key("accent"));
    score.validate(&base).unwrap();
}

#[test]
fn dropdown_inputs_keep_their_kind_and_reject_incompatible_destinations_atomically() {
    let library = standard_library();
    let mut definition = library.definitions["chase"].instance("chase");
    definition
        .edit(
            &library,
            GraphEdit::Default {
                key: "boundary".into(),
                value: Value::Boundary(Boundary::Wrap),
            },
        )
        .unwrap();
    definition
        .edit(
            &library,
            GraphEdit::AddInput {
                key: "edges".into(),
                name: "Edges".into(),
                position: [0., 0.],
            },
        )
        .unwrap();
    definition
        .edit(&library, bind("edges", "effect", "boundary"))
        .unwrap();
    let spec = &definition.inputs["edges"];
    assert_eq!(spec.value_type, ValueType::Boundary);
    assert_eq!(spec.default, Some(Value::Boundary(Boundary::Wrap)));
    let before = definition.clone();
    assert!(definition
        .edit(&library, bind("edges", "effect", "travel"))
        .is_err());
    assert_eq!(definition, before);
    assert!(definition
        .edit(
            &library,
            GraphEdit::RenameInput {
                key: "edges".into(),
                name: "\n".into()
            }
        )
        .is_err());
    assert_eq!(definition, before);
    assert!(definition
        .edit(
            &library,
            GraphEdit::MoveInputs {
                positions: BTreeMap::from([("edges".into(), [f64::NAN, 0.])])
            }
        )
        .is_err());
    assert_eq!(definition, before);
}
