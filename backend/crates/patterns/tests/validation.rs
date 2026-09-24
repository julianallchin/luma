use luma_patterns::*;
use std::collections::BTreeMap;

fn wire(node: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: "value".into(),
    }
}

/// A graph of one `effect` node that calls `id` with the same interface.
fn wrap(library: &Library, id: &str) -> Definition {
    let callee = &library.definitions[id];
    Definition {
        name: String::new(),
        inputs: callee.inputs.clone(),
        outputs: callee.outputs.clone(),
        body: Body::Graph(Graph {
            nodes: BTreeMap::from([(
                "effect".into(),
                Node {
                    definition: id.into(),
                    inputs: callee
                        .inputs
                        .keys()
                        .map(|key| (key.clone(), Binding::Input { input: key.clone() }))
                        .collect(),
                },
            )]),
            outputs: callee
                .outputs
                .keys()
                .map(|key| {
                    (
                        key.clone(),
                        Binding::Connection {
                            node: "effect".into(),
                            output: key.clone(),
                        },
                    )
                })
                .collect(),
        }),
    }
}

#[test]
fn compact_recursive_expansion_is_rejected_before_it_is_built() {
    let mut library = standard_library();
    let mut previous = "core/fraction".to_owned();
    for index in 0..16 {
        let mut definition = wrap(&library, &previous);
        let Body::Graph(graph) = &mut definition.body else {
            unreachable!()
        };
        graph
            .nodes
            .insert("copy".into(), graph.nodes["effect"].clone());
        graph.nodes.insert(
            "sum".into(),
            Node {
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
    let mut definition = wrap(&library, "core/fraction");
    let Body::Graph(graph) = &mut definition.body else {
        unreachable!()
    };
    let mut previous = "effect".to_owned();
    for index in 0..110 {
        let id = format!("sum-{index}");
        graph.nodes.insert(
            id.clone(),
            Node {
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

    let mut previous = "core/fraction".to_owned();
    for index in 0..30 {
        let definition = wrap(&library, &previous);
        previous = format!("wrapper-{index}");
        library.definitions.insert(previous.clone(), definition);
    }
    assert!(library
        .validate(&previous)
        .unwrap_err()
        .to_string()
        .contains("nesting"));
}
