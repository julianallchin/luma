use luma_patterns::*;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn add(graph: &mut Graph, id: &str, definition: &str, inputs: &[(&str, Binding)]) {
    graph.nodes.insert(
        id.into(),
        Node {
            definition: definition.into(),
            position: None,
            inputs: inputs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        },
    );
}
fn frame(start: f64, duration: f64) -> Frame<'static> {
    Frame {
        cells: &[],
        features: None,
        beat: start,
        clip_start: start,
        clip_duration: duration,
        seed: 0,
    }
}
fn library(graph: Graph) -> Library {
    let mut library = standard_library();
    let outputs = graph
        .outputs
        .iter()
        .map(|(key, binding)| {
            let (value_type, rate) = library
                .binding_type(&BTreeMap::new(), &graph, binding)
                .unwrap();
            (key.clone(), Output { value_type, rate })
        })
        .collect();
    library.definitions.insert(
        "example".into(),
        Definition {
            name: "Clip conditioning".into(),
            inputs: BTreeMap::new(),
            outputs,
            body: Body::Graph(graph),
        },
    );
    library
}
fn normalization(graph: &mut Graph, id: &str, source: Binding) -> Binding {
    let range = format!("{id}/range");
    let width = format!("{id}/width");
    let offset = format!("{id}/offset");
    add(graph, &range, "clip_range", &[("value", source.clone())]);
    add(
        graph,
        &width,
        "core/subtract",
        &[
            ("a", wire(&range, "maximum")),
            ("b", wire(&range, "minimum")),
        ],
    );
    add(
        graph,
        &offset,
        "core/subtract",
        &[("a", source), ("b", wire(&range, "minimum"))],
    );
    add(
        graph,
        id,
        "core/divide",
        &[("a", wire(&offset, "value")), ("b", wire(&width, "value"))],
    );
    wire(id, "value")
}

#[test]
fn nested_ranges_are_prepared_in_dependency_order_and_do_not_depend_on_seek_order() {
    let mut graph = Graph::default();
    add(&mut graph, "time", "clip_time", &[]);
    let first = normalization(&mut graph, "first", wire("time", "elapsed"));
    let second = normalization(&mut graph, "second", first);
    graph.outputs.insert("value".into(), second);
    graph
        .outputs
        .insert("length".into(), wire("first/range", "maximum"));
    let library = library(graph);
    for duration in [2., 7.] {
        let program =
            PreparedGraph::new(&library, "example", &BTreeMap::new(), frame(3., duration)).unwrap();
        let times = [3. + duration, 3., 3. + duration * 0.3, 3. + duration];
        let batch = program.evaluate_batch(&times).unwrap();
        assert_eq!(batch["length"].signal().unwrap().unit(), Unit::Beats);
        assert_eq!(
            batch["length"].signal().unwrap().values()[[0, 0, 0]],
            duration
        );
        for (index, expected) in [1., 0., 0.3, 1.].into_iter().enumerate() {
            let value = batch["value"].signal().unwrap().values()[[0, index, 0]];
            assert!((value - expected).abs() < 1e-12);
            let single = program.evaluate_batch(&[times[index]]).unwrap();
            assert_eq!(single["value"].signal().unwrap().values()[[0, 0, 0]], value);
        }
    }
}

#[test]
fn ranges_reduce_all_axes_but_preserve_units_and_validate_sampling_controls() {
    let signal = Signal::new(
        ndarray::Array3::from_shape_vec((2, 2, 2), vec![-3., 12., 4., 5., 20., 4., 9., 6.])
            .unwrap(),
        Unit::Degrees,
        Channels::components(2).unwrap(),
        Some(vec!["a".into(), "b".into()].into()),
    )
    .unwrap();
    let lib = standard_library();
    let args = BTreeMap::from([("value".into(), Value::Signal(signal))]);
    let program = PreparedGraph::new(&lib, "clip_range", &args, frame(0., 4.)).unwrap();
    assert_eq!(program.dynamic_step_count(), 0);
    let output = program.evaluate_batch(&[2., 0., 4.]).unwrap();
    for (key, expected) in [("minimum", -3.), ("maximum", 20.)] {
        let signal = output[key].signal().unwrap();
        assert_eq!(signal.values().dim(), (1, 1, 1));
        assert_eq!(signal.unit(), Unit::Degrees);
        assert!(signal.fixtures().is_none());
        assert_eq!(signal.values()[[0, 0, 0]], expected);
    }
    for samples in [0., 1., 2.5, 16385.] {
        let mut args = args.clone();
        args.insert("samples".into(), Value::Number(samples));
        assert!(PreparedGraph::new(&lib, "clip_range", &args, frame(0., 4.)).is_err());
    }
}

#[derive(Debug)]
struct Analysis {
    scale: f64,
    reads: AtomicUsize,
}
impl FeatureSource for Analysis {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
        assert!(matches!(request, FeatureRequest::Band { .. }));
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(FeatureSample::Energy(beat * self.scale))
    }
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        unreachable!()
    }
}

#[test]
fn playback_reuses_the_range_and_rebinding_analysis_recomputes_it() {
    let mut graph = Graph::default();
    add(&mut graph, "audio", "band_energy", &[]);
    add(
        &mut graph,
        "range",
        "clip_range",
        &[
            ("value", wire("audio", "value")),
            ("samples", Value::Number(259.).into()),
        ],
    );
    graph
        .outputs
        .insert("maximum".into(), wire("range", "maximum"));
    let library = library(graph);
    let first = Arc::new(Analysis {
        scale: 2.,
        reads: AtomicUsize::new(0),
    });
    let second = Arc::new(Analysis {
        scale: 3.,
        reads: AtomicUsize::new(0),
    });
    let program = PreparedGraph::new(&library, "example", &BTreeMap::new(), frame(1., 1.))
        .unwrap()
        .with_features(first.clone())
        .unwrap();
    assert_eq!(program.dynamic_step_count(), 0);
    assert_eq!(first.reads.load(Ordering::SeqCst), 259);
    for time in [1., 100., 0., 1.5] {
        assert_eq!(
            program.evaluate_batch(&[time]).unwrap()["maximum"]
                .signal()
                .unwrap()
                .values()[[0, 0, 0]],
            4.
        );
    }
    assert_eq!(first.reads.load(Ordering::SeqCst), 259);
    let rebound = program.clone().with_features(second.clone()).unwrap();
    assert_eq!(
        rebound.evaluate_batch(&[1.]).unwrap()["maximum"]
            .signal()
            .unwrap()
            .values()[[0, 0, 0]],
        6.
    );
    assert_eq!(second.reads.load(Ordering::SeqCst), 259);
    assert_eq!(
        program.evaluate_batch(&[1.]).unwrap()["maximum"]
            .signal()
            .unwrap()
            .values()[[0, 0, 0]],
        4.
    );
}

#[test]
fn range_sampling_does_not_execute_an_unrelated_output_branch() {
    let mut graph = Graph::default();
    add(&mut graph, "time", "clip_time", &[]);
    add(
        &mut graph,
        "range",
        "clip_range",
        &[("value", wire("time", "progress"))],
    );
    add(
        &mut graph,
        "offset",
        "core/subtract",
        &[
            ("a", Value::Number(0.5).into()),
            ("b", wire("time", "progress")),
        ],
    );
    add(
        &mut graph,
        "root",
        "core/power",
        &[
            ("base", wire("offset", "value")),
            ("exponent", Value::Number(0.5).into()),
        ],
    );
    graph
        .outputs
        .insert("maximum".into(), wire("range", "maximum"));
    graph.outputs.insert("other".into(), wire("root", "value"));
    let library = library(graph);
    let program = PreparedGraph::new(&library, "example", &BTreeMap::new(), frame(0., 1.)).unwrap();
    assert_eq!(
        program.evaluate_batch(&[0.]).unwrap()["maximum"]
            .signal()
            .unwrap()
            .values()[[0, 0, 0]],
        1.
    );
    assert!(program.evaluate_batch(&[1.]).is_err());
}
