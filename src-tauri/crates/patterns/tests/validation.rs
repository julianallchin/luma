use luma_patterns::*;
use std::collections::BTreeMap;

fn wire(node: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: "lighting".into(),
    }
}

#[test]
fn compact_recursive_expansion_is_rejected_before_it_is_built() {
    let mut library = standard_library();
    let mut previous = "write_position".to_owned();
    for index in 0..16 {
        let mut definition = library.definitions[&previous].instance(&previous);
        let Body::Graph(graph) = &mut definition.body else {
            unreachable!()
        };
        graph
            .nodes
            .insert("copy".into(), graph.nodes["effect"].clone());
        graph.nodes.insert(
            "sum".into(),
            Node {
                position: None,
                definition: "add_lighting".into(),
                inputs: BTreeMap::from([("a".into(), wire("effect")), ("b".into(), wire("copy"))]),
            },
        );
        graph.outputs.insert("lighting".into(), wire("sum"));
        previous = format!("level-{index}");
        library.definitions.insert(previous.clone(), definition);
    }
    let error = library.validate(&previous).unwrap_err().to_string();
    assert!(error.contains("expansion"), "{error}");
}

#[test]
fn long_wire_chains_and_deep_definition_chains_are_bounded() {
    let mut library = standard_library();
    let mut definition = library.definitions["write_position"].instance("write_position");
    let Body::Graph(graph) = &mut definition.body else {
        unreachable!()
    };
    let mut previous = "effect".to_owned();
    for index in 0..90 {
        let id = format!("sum-{index}");
        graph.nodes.insert(
            id.clone(),
            Node {
                position: None,
                definition: "add_lighting".into(),
                inputs: BTreeMap::from([
                    ("a".into(), wire(&previous)),
                    ("b".into(), wire("effect")),
                ]),
            },
        );
        previous = id;
    }
    graph.outputs.insert("lighting".into(), wire(&previous));
    library.definitions.insert("chain".into(), definition);
    assert!(library
        .validate("chain")
        .unwrap_err()
        .to_string()
        .contains("dependency depth"));

    let mut previous = "write_position".to_owned();
    for index in 0..30 {
        let definition = library.definitions[&previous].instance(&previous);
        previous = format!("wrapper-{index}");
        library.definitions.insert(previous.clone(), definition);
    }
    assert!(library
        .validate(&previous)
        .unwrap_err()
        .to_string()
        .contains("nesting"));
}

#[test]
fn score_validation_rejects_invalid_timing_even_without_a_resolved_venue() {
    let library = standard_library();
    let mut score = Score::default();
    score
        .insert_effect(&library, "chase", "clip", 0.0, 16.0)
        .unwrap();
    for repeat in [0.0, 0.5] {
        score
            .clips
            .get_mut("clip")
            .unwrap()
            .inputs
            .insert("repeat".into(), Value::Beats(repeat));
        assert!(score.validate(&library).is_err());
    }
    score.clips.get_mut("clip").unwrap().inputs.clear();
    score.validate(&library).unwrap();
    let input = score.definitions["clip"].inputs["width"].clone();
    score
        .definitions
        .get_mut("clip")
        .unwrap()
        .inputs
        .insert("@clip/selection".into(), input);
    assert!(score
        .validate(&library)
        .unwrap_err()
        .to_string()
        .contains("reserved"));
}
