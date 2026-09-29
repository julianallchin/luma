use luma_patterns::*;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn cells() -> Vec<Cell> {
    // Identity order deliberately differs from spatial/selection order.
    (0..16)
        .rev()
        .map(|i| Cell {
            id: format!("head-{i}"),
            group: "bars".into(),
            world: [i as f64, (i % 3) as f64, (i % 5) as f64],
            uvz: [i as f64, (i % 3) as f64, (i % 5) as f64],
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 2.0,
        clip_start: 2.0,
        clip_duration: 8.0,
        seed: 427,
    }
}

#[derive(Debug, Default)]
struct Analysis;
impl FeatureSource for Analysis {
    fn sample(&self, _: &FeatureRequest, beat: f64) -> Result<f64> {
        Ok(0.5 + 0.4 * beat.sin())
    }
}

#[test]
fn default_graphs_keep_all_time_samples_through_arithmetic_color_and_output() {
    let library = standard_library();
    let cells = cells();
    let times = [9.9, 2.0, 4.5, -0.2, 6.3, 2.0, 3.2];
    let mut checked = Vec::new();
    for (id, definition) in &library.definitions {
        if definition
            .inputs
            .values()
            .any(|input| input.default.is_none())
        {
            continue;
        }
        // A form clip carries every input; its defaults are a whole preset.
        let inputs = if is_form(id) {
            definition
                .inputs
                .iter()
                .map(|(key, input)| (key.clone(), input.default.clone().unwrap()))
                .collect()
        } else {
            BTreeMap::new()
        };
        let program = PreparedGraph::new(&library, id, &inputs, frame(&cells))
            .unwrap_or_else(|e| panic!("prepare {id}: {e}"))
            .with_features(Arc::new(Analysis))
            .unwrap();
        let batch = program
            .evaluate_batch(&times)
            .unwrap_or_else(|e| panic!("batch {id}: {e}"));
        for (t, beat) in times.iter().enumerate() {
            let single = program
                .evaluate(*beat)
                .unwrap_or_else(|e| panic!("single {id}: {e}"));
            for (key, value) in &batch {
                assert_eq!(
                    value.sample(t).unwrap(),
                    single[key],
                    "{id}.{key} at {beat}"
                );
            }
        }
        program
            .evaluate_batch(&[])
            .unwrap_or_else(|e| panic!("empty {id}: {e}"));
        checked.push(id.as_str());
    }
    for form in FORMS {
        assert!(
            checked.contains(&form),
            "the {form} defaults must exercise the tensor graph"
        );
    }
}

fn wired(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn node(definition: &str, inputs: BTreeMap<String, Binding>) -> Node {
    Node {
        definition: definition.into(),
        inputs,
    }
}

#[test]
fn nested_outputs_only_prepare_their_connected_analysis_and_reductions() {
    // Only the bass band was analyzed.
    #[derive(Debug, Default)]
    struct BassOnly(AtomicUsize);
    impl FeatureSource for BassOnly {
        fn sample(&self, request: &FeatureRequest, _: f64) -> Result<f64> {
            if request.low_hz != 20. {
                return Err(Error("only bass was analyzed".into()));
            }
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(0.25)
        }
    }
    fn define(library: &mut Library, id: &str, graph: Graph) {
        let outputs = graph
            .outputs
            .iter()
            .map(|(name, binding)| {
                let (value_type, rate) = library
                    .binding_type(&BTreeMap::new(), &graph, binding)
                    .unwrap();
                (name.clone(), Output { value_type, rate })
            })
            .collect();
        library.definitions.insert(
            id.into(),
            Definition {
                name: id.into(),
                inputs: BTreeMap::new(),
                outputs,
                body: Body::Graph(graph),
            },
        );
    }
    let mut library = standard_library();
    let mut graph = Graph::default();
    for (name, low, high) in [("bass", 20., 60.), ("treble", 4000., 8000.)] {
        graph.nodes.insert(
            name.into(),
            node(
                "band_energy",
                BTreeMap::from([
                    ("low_hz".into(), Value::Number(low).into()),
                    ("high_hz".into(), Value::Number(high).into()),
                ]),
            ),
        );
        graph.outputs.insert(name.into(), wired(name, "value"));
    }
    graph.nodes.insert(
        "range".into(),
        node(
            "clip_range",
            BTreeMap::from([("value".into(), wired("treble", "value"))]),
        ),
    );
    graph
        .outputs
        .insert("peak".into(), wired("range", "maximum"));
    graph
        .outputs
        .insert("constant".into(), Value::Number(0.5).into());
    define(&mut library, "sources", graph);
    let mut wrapper = Graph::default();
    wrapper
        .nodes
        .insert("sources".into(), node("sources", BTreeMap::new()));
    for key in ["bass", "treble", "peak", "constant"] {
        wrapper.outputs.insert(key.into(), wired("sources", key));
    }
    define(&mut library, "wrapper", wrapper);
    for key in ["bass", "constant", "treble", "peak"] {
        let mut root = Graph::default();
        root.nodes
            .insert("source".into(), node("wrapper", BTreeMap::new()));
        root.outputs.insert("value".into(), wired("source", key));
        define(&mut library, "root", root);
        let prepared = PreparedGraph::new(&library, "root", &BTreeMap::new(), frame(&[])).unwrap();
        if key == "constant" {
            assert!(prepared.feature_requests().is_empty());
            assert_eq!(prepared.dynamic_step_count(), 0);
            assert_eq!(
                prepared.evaluate(100.).unwrap()["value"],
                Value::Number(0.5)
            );
            continue;
        }
        assert_eq!(prepared.feature_requests().len(), 1);
        let features = Arc::new(BassOnly::default());
        if key != "bass" {
            // Connecting an unavailable branch still reports its real error.
            assert!(prepared.with_features(features).is_err());
            continue;
        }
        assert_eq!(
            prepared.feature_requests(),
            &[FeatureRequest {
                low_hz: 20.,
                high_hz: 60.,
            }]
        );
        let prepared = prepared.with_features(features.clone()).unwrap();
        for beat in [2., 1., -1., 2.] {
            assert_eq!(
                prepared.evaluate(beat).unwrap()["value"].scalar_value(),
                Some(0.25)
            );
        }
    }
}
