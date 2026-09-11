//! Inputs declare how a person or agent writes a constant; the wire type
//! alone decides compatibility. Controls come from one built-in kit.
use luma_patterns::*;
use std::collections::BTreeMap;

#[test]
fn shipped_curves_offer_named_presets_with_a_custom_editor() {
    let library = standard_library();
    for (graph, input) in [
        ("chase", "path"),
        ("pulse", "shape"),
        ("dissolve", "proportion"),
    ] {
        let spec = &library.definitions[graph].inputs[input];
        let Some(Author::Choice { options, custom }) = spec.author() else {
            panic!("{graph}.{input} should offer presets");
        };
        assert!(
            custom,
            "{graph}.{input} keeps the curve editor for custom shapes"
        );
        assert!(options.len() >= 3);
        for preset in &options {
            assert!(
                matches!(preset.value, Value::Envelope(_)),
                "{graph}.{input}"
            );
        }
        assert!(
            options
                .iter()
                .any(|preset| Some(&preset.value) == spec.default.as_ref()),
            "{graph}.{input}: the default is one of its presets"
        );
    }
}

#[test]
fn value_types_imply_their_controls() {
    let library = standard_library();
    let chase = &library.definitions["chase"];
    assert_eq!(
        chase.inputs["width"].author(),
        Some(Author::Number {
            min: Some(0.0),
            max: Some(1.0)
        })
    );
    assert_eq!(
        chase.inputs["travel"].author(),
        Some(Author::Number {
            min: Some(0.0),
            max: None
        })
    );
    assert_eq!(
        library.definitions["beat_chase"].inputs["grid_aligned"].author(),
        Some(Author::Toggle)
    );
    assert_eq!(
        library.definitions["wash"].inputs["color"].author(),
        Some(Author::Color)
    );
    assert_eq!(
        library.definitions["gradient"].inputs["gradient"].author(),
        Some(Author::Gradient)
    );
    assert_eq!(chase.inputs["trigger"].author(), None);
}

#[test]
fn presets_must_fit_the_wire_type_and_exposed_inputs_inherit_them() {
    let library = standard_library();
    let mut definition = library.definitions["beat_chase"].instance("beat_chase");
    definition
        .edit(
            &library,
            GraphEdit::AddInput {
                key: "travel_curve".into(),
                name: "Travel curve".into(),
                position: [0., 0.],
            },
        )
        .unwrap();
    definition
        .edit(
            &library,
            GraphEdit::Bind {
                node: "effect".into(),
                input: "path".into(),
                binding: Some(Binding::Input {
                    input: "travel_curve".into(),
                }),
            },
        )
        .unwrap();
    assert_eq!(
        definition.inputs["travel_curve"].author,
        library.definitions["beat_chase"].inputs["path"].author,
        "an exposed Input authors like its destination"
    );
    let mut wrong = definition.clone();
    wrong.inputs.get_mut("travel_curve").unwrap().author = Some(Author::Choice {
        options: vec![Preset {
            label: "Half".into(),
            value: Value::Proportion(0.5),
        }],
        custom: false,
    });
    let mut library = library;
    library.definitions.insert("wrong".into(), wrong);
    assert!(library
        .validate("wrong")
        .unwrap_err()
        .0
        .contains("does not fit"));
    let mut bad_range = definition.clone();
    bad_range.inputs.get_mut("width").unwrap().author = Some(Author::Number {
        min: Some(1.0),
        max: Some(0.0),
    });
    library.definitions.insert("range".into(), bad_range);
    assert!(library.validate("range").is_err());
    let mut score = Score::default();
    score
        .insert_effect(&library, "beat_chase", "clip", 0., 8.)
        .unwrap();
    let json = score.to_json(&library).unwrap();
    let restored = Score::from_json(&library, &json).unwrap();
    assert_eq!(restored.definitions, score.definitions);
    let _ = BTreeMap::<String, Value>::new();
}
