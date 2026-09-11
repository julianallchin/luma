use luma_patterns::*;
use std::collections::BTreeMap;

fn wire(node: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: "value".into(),
    }
}

#[test]
fn compact_recursive_expansion_is_rejected_before_it_is_built() {
    let mut library = standard_library();
    let mut previous = "core/absolute".to_owned();
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
                definition: "core/add".into(),
                inputs: BTreeMap::from([("a".into(), wire("effect")), ("b".into(), wire("copy"))]),
            },
        );
        graph.outputs.insert("value".into(), wire("sum"));
        previous = format!("level-{index}");
        library.definitions.insert(previous.clone(), definition);
    }
    let error = library.validate(&previous).unwrap_err().to_string();
    assert!(error.contains("expansion"), "{error}");
}

#[test]
fn long_wire_chains_and_deep_definition_chains_are_bounded() {
    let mut library = standard_library();
    let mut definition = library.definitions["core/absolute"].instance("core/absolute");
    let Body::Graph(graph) = &mut definition.body else {
        unreachable!()
    };
    let mut previous = "effect".to_owned();
    for index in 0..110 {
        let id = format!("sum-{index}");
        graph.nodes.insert(
            id.clone(),
            Node {
                position: None,
                definition: "core/add".into(),
                inputs: BTreeMap::from([
                    ("a".into(), wire(&previous)),
                    ("b".into(), wire("effect")),
                ]),
            },
        );
        previous = id;
    }
    graph.outputs.insert("value".into(), wire(&previous));
    library.definitions.insert("chain".into(), definition);
    assert!(library
        .validate("chain")
        .unwrap_err()
        .to_string()
        .contains("dependency depth"));

    let mut previous = "core/absolute".to_owned();
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
        .insert_effect(&library, "beat_chase", "clip", 0.0, 16.0)
        .unwrap();
    score
        .clips
        .get_mut("clip")
        .unwrap()
        .inputs
        .insert("repeat".into(), Value::Beats(0.0));
    assert!(score.validate(&library).is_err());
    score
        .clips
        .get_mut("clip")
        .unwrap()
        .inputs
        .insert("repeat".into(), Value::Beats(0.5));
    score.validate(&library).unwrap();
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

#[test]
fn invalid_envelopes_identify_the_authored_location_and_anchor() {
    let library = standard_library();
    let mut original = Score::default();
    original
        .insert_effect(&library, "beat_chase", "intro", 0., 16.)
        .unwrap();
    let invalid = Value::Envelope(Envelope::linear(vec![
        [0., 0.],
        [0.5, 1.],
        [0.4, 0.],
        [1., 0.],
    ]));
    let check = |score: &Score, location: &str| {
        let error = score.validate(&library).unwrap_err().to_string();
        assert!(error.contains(location), "{error}");
        assert!(error.contains("points[2].x"), "{error}");
        assert!(error.contains("previous x (0.5)"), "{error}");
    };
    let mut score = original.clone();
    score
        .clips
        .get_mut("intro")
        .unwrap()
        .inputs
        .insert("shape".into(), invalid.clone());
    check(&score, "clip intro, graph intro, input shape");
    let mut score = original.clone();
    score
        .definitions
        .get_mut("intro")
        .unwrap()
        .inputs
        .get_mut("shape")
        .unwrap()
        .default = Some(invalid.clone());
    check(&score, "graph intro, input shape, default");
    let mut score = original;
    let Body::Graph(graph) = &mut score.definitions.get_mut("intro").unwrap().body else {
        unreachable!()
    };
    graph
        .nodes
        .get_mut("effect")
        .unwrap()
        .inputs
        .insert("shape".into(), invalid.into());
    check(&score, "graph intro, node effect, input shape");
    let Body::Graph(graph) = &mut score.definitions.get_mut("intro").unwrap().body else {
        unreachable!()
    };
    graph.nodes.get_mut("effect").unwrap().inputs.insert(
        "shape".into(),
        Value::Envelope(Envelope::linear(vec![[0., 0.], [0.5, 1.], [1., 0.]])).into(),
    );
    score.validate(&library).unwrap();
}
