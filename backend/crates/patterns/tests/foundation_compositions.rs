//! Product acceptance graphs: the user-facing composition is the computation.
use luma_patterns::*;
use ndarray::Array3;
use std::collections::BTreeMap;

fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 8.,
        seed: 427,
    }
}
fn vector(values: &[f64], unit: Unit) -> Binding {
    Value::Signal(Signal::vector(values.to_vec(), unit).unwrap()).into()
}
#[derive(Default)]
struct Patch(Graph);
impl Patch {
    fn add<const N: usize>(&mut self, id: &str, definition: &str, inputs: [(&str, Binding); N]) {
        self.0.nodes.insert(
            id.into(),
            Node {
                definition: definition.into(),
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
    }
    fn prepare(mut self, outputs: &[(&str, Binding)], cells: &[Cell]) -> PreparedGraph {
        let mut library = standard_library();
        self.0.outputs = outputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let specs = outputs
            .iter()
            .map(|(k, v)| {
                let (value_type, _) = library.binding_type(&BTreeMap::new(), &self.0, v).unwrap();
                (
                    k.to_string(),
                    Output {
                        value_type,
                        rate: Rate::Frame,
                    },
                )
            })
            .collect();
        let definition = Definition {
            name: "Foundation example".into(),
            inputs: BTreeMap::new(),
            outputs: specs,
            body: Body::Graph(self.0),
        };
        // The same canonical document shape saved by the editor must roundtrip.
        let definition =
            serde_json::from_str(&serde_json::to_string(&definition).unwrap()).unwrap();
        library.definitions.insert("example".into(), definition);
        PreparedGraph::new(&library, "example", &BTreeMap::new(), frame(cells)).unwrap()
    }
}

#[test]
fn the_same_vector_math_transforms_sampled_paths_without_fixture_references() {
    // Samples of a geometric path parameter. This checks numerical portability;
    // a laser still needs an explicit scan-sample domain and device output.
    let samples = Signal::new(
        Array3::from_shape_vec((1, 3, 2), vec![-1., 0., 0., 1., 1., 0.]).unwrap(),
        Unit::Number,
        Channels::components(2).unwrap(),
        None,
    )
    .unwrap();
    let mut graph = Patch::default();
    graph.add(
        "scale",
        "core/multiply",
        [
            ("a", Value::Signal(samples).into()),
            ("b", vector(&[0.5, 0.25], Unit::Number)),
        ],
    );
    graph.add(
        "offset",
        "core/add",
        [
            ("a", wire("scale", "value")),
            ("b", vector(&[0.1, -0.1], Unit::Number)),
        ],
    );
    let program = graph.prepare(&[("path", wire("offset", "value"))], &[]);
    let output = program.evaluate_batch(&[0., 0.5, 1.]).unwrap();
    let signal = output["path"].signal().unwrap();
    assert!(signal.fixtures().is_none());
    let expected = [-0.4, -0.1, 0.1, 0.15, 0.6, -0.1];
    assert!(signal
        .values()
        .iter()
        .zip(expected)
        .all(|(a, b)| (a - b).abs() < 1e-12));
}
