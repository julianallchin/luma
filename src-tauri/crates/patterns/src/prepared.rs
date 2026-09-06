//! Validated, flattened graphs. Mapping/other fixed outputs are computed once;
//! the frame loop never walks definitions or repeats geometry solves.
use crate::{
    graph::run_primitive, Binding, Body, Cell, Error, Frame, Graph, Library, Primitive, Rate,
    Result, Value,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
enum Source {
    Constant(Value),
    Slot(usize),
}
#[derive(Clone, Debug)]
struct Step {
    primitive: Primitive,
    inputs: BTreeMap<String, Source>,
    outputs: BTreeMap<String, usize>,
}
#[derive(Clone, Debug)]
pub struct PreparedGraph {
    steps: Vec<Step>,
    outputs: BTreeMap<String, Source>,
    slots: usize,
    cells: Vec<Cell>,
    clip_start: f64,
    clip_duration: f64,
    seed: u64,
    features: Option<std::sync::Arc<dyn crate::FeatureSource>>,
    requests: Vec<crate::FeatureRequest>,
}
impl PreparedGraph {
    pub fn new(
        library: &Library,
        definition: &str,
        inputs: &BTreeMap<String, Value>,
        frame: Frame,
    ) -> Result<Self> {
        library.validate(definition)?;
        Self::new_validated(library, definition, inputs, frame)
    }

    /// The caller has already validated this library's reachable definitions.
    /// Used by whole-score validation to share that work across repeated clips.
    pub(crate) fn new_validated(
        library: &Library,
        definition: &str,
        inputs: &BTreeMap<String, Value>,
        frame: Frame,
    ) -> Result<Self> {
        frame.validate()?;
        if frame.features.is_some() {
            return Err(Error(
                "bind owned track data with PreparedGraph::with_features".into(),
            ));
        }
        let mut prepared = Self {
            steps: Vec::new(),
            outputs: BTreeMap::new(),
            slots: 0,
            cells: frame.cells.to_vec(),
            clip_start: frame.clip_start,
            clip_duration: frame.clip_duration,
            seed: frame.seed,
            features: None,
            requests: Vec::new(),
        };
        let bound = inputs
            .iter()
            .map(|(key, value)| (key.clone(), Source::Constant(value.clone())))
            .collect();
        prepared.outputs = prepared.lower(library, definition, bound)?;
        // Validate relationships such as travel <= repeat at preparation, not
        // on the first device tick. Dynamic errors still propagate from render.
        if prepared.requests.is_empty() {
            prepared.evaluate(frame.clip_start)?;
        }
        Ok(prepared)
    }
    pub fn feature_requests(&self) -> &[crate::FeatureRequest] {
        &self.requests
    }
    pub fn with_features(
        mut self,
        features: std::sync::Arc<dyn crate::FeatureSource>,
    ) -> Result<Self> {
        self.features = Some(features);
        self.evaluate(self.clip_start)?;
        Ok(self)
    }
    pub fn dynamic_step_count(&self) -> usize {
        self.steps.len()
    }
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
    pub fn evaluate(&self, beat: f64) -> Result<BTreeMap<String, Value>> {
        if !beat.is_finite() {
            return Err(Error("musical time must be finite".into()));
        }
        let mut slots = vec![None; self.slots];
        for step in &self.steps {
            let inputs = step
                .inputs
                .iter()
                .map(|(key, source)| Ok((key.clone(), source.read(&slots)?)))
                .collect::<Result<_>>()?;
            let outputs = run_primitive(
                step.primitive,
                &inputs,
                Frame {
                    features: self.features.as_deref(),
                    beat,
                    clip_start: self.clip_start,
                    clip_duration: self.clip_duration,
                    seed: self.seed,
                    cells: &self.cells,
                },
            )?;
            for (name, value) in outputs {
                value.validate()?;
                slots[step.outputs[&name]] = Some(value);
            }
        }
        self.outputs
            .iter()
            .map(|(name, source)| Ok((name.clone(), source.read(&slots)?)))
            .collect()
    }
    fn lower(
        &mut self,
        library: &Library,
        id: &str,
        mut inputs: BTreeMap<String, Source>,
    ) -> Result<BTreeMap<String, Source>> {
        let definition = &library.definitions[id];
        for name in inputs.keys() {
            if !definition.inputs.contains_key(name) {
                return Err(Error(format!("{id}: unknown input {name}")));
            }
        }
        for (name, input) in &definition.inputs {
            if !inputs.contains_key(name) {
                let value = input
                    .default
                    .as_ref()
                    .ok_or_else(|| Error(format!("{id}: required input {name}")))?;
                inputs.insert(name.clone(), Source::Constant(value.clone()));
            }
            if let Source::Constant(value) = &inputs[name] {
                value.validate()?;
                if value.value_type() != input.value_type {
                    return Err(Error(format!("{id}.{name}: input type mismatch")));
                }
            }
        }
        match &definition.body {
            Body::Primitive(primitive) => {
                let constants: BTreeMap<_, _> = inputs
                    .iter()
                    .filter_map(|(name, source)| match source {
                        Source::Constant(value) => Some((name.clone(), value.clone())),
                        _ => None,
                    })
                    .collect();
                validate_parameters(*primitive, &constants)?;
                if let Some(request) = crate::features::request(*primitive, &constants)? {
                    if !self.requests.contains(&request) {
                        self.requests.push(request);
                    }
                }
                if definition.outputs.values().all(|o| o.rate == Rate::Fixed)
                    || (!primitive.reads_time()
                        && inputs
                            .values()
                            .all(|input| matches!(input, Source::Constant(_))))
                {
                    let values = inputs
                        .iter()
                        .map(|(key, source)| Ok((key.clone(), source.read(&[])?)))
                        .collect::<Result<_>>()?;
                    let result = run_primitive(
                        *primitive,
                        &values,
                        Frame {
                            features: self.features.as_deref(),
                            beat: self.clip_start,
                            clip_start: self.clip_start,
                            clip_duration: self.clip_duration,
                            seed: self.seed,
                            cells: &self.cells,
                        },
                    )?;
                    result
                        .into_iter()
                        .map(|(name, value)| {
                            value.validate()?;
                            Ok((name, Source::Constant(value)))
                        })
                        .collect()
                } else {
                    let mut outputs = BTreeMap::new();
                    let mut sources = BTreeMap::new();
                    for name in definition.outputs.keys() {
                        outputs.insert(name.clone(), self.slots);
                        sources.insert(name.clone(), Source::Slot(self.slots));
                        self.slots += 1;
                    }
                    self.steps.push(Step {
                        primitive: *primitive,
                        inputs,
                        outputs,
                    });
                    Ok(sources)
                }
            }
            Body::Graph(graph) => {
                let mut nodes = BTreeMap::new();
                graph
                    .outputs
                    .iter()
                    .map(|(name, binding)| {
                        Ok((
                            name.clone(),
                            self.bind(library, graph, binding, &inputs, &mut nodes)?,
                        ))
                    })
                    .collect()
            }
        }
    }
    fn bind(
        &mut self,
        library: &Library,
        graph: &Graph,
        binding: &Binding,
        inputs: &BTreeMap<String, Source>,
        nodes: &mut BTreeMap<String, BTreeMap<String, Source>>,
    ) -> Result<Source> {
        match binding {
            Binding::Value { value } => Ok(Source::Constant(value.clone())),
            Binding::Input { input } => Ok(inputs[input].clone()),
            Binding::Connection { node, output } => {
                if !nodes.contains_key(node) {
                    let instance = &graph.nodes[node];
                    let inputs = instance
                        .inputs
                        .iter()
                        .map(|(name, binding)| {
                            Ok((
                                name.clone(),
                                self.bind(library, graph, binding, inputs, nodes)?,
                            ))
                        })
                        .collect::<Result<_>>()?;
                    nodes.insert(
                        node.clone(),
                        self.lower(library, &instance.definition, inputs)?,
                    );
                }
                Ok(nodes[node][output].clone())
            }
        }
    }
}

/// Relations involving fixed controls are checked even when a graph's dynamic
/// branch requires track data that is deliberately absent during source validation.
fn validate_parameters(op: Primitive, inputs: &BTreeMap<String, Value>) -> Result<()> {
    let number = |name| inputs.get(name).map(Value::scalar);
    match op {
        Primitive::Rhythm if number("repeat").is_some_and(|v| v <= 0.0) => {
            Err(Error("repeat interval must be greater than zero".into()))
        }
        Primitive::Motion
            if number("travel")
                .zip(number("repeat"))
                .is_some_and(|(travel, repeat)| travel <= 0.0 || repeat < travel) =>
        {
            Err(Error(
                "travel must be positive and no longer than repeat".into(),
            ))
        }
        _ => Ok(()),
    }
}
impl Source {
    fn read(&self, slots: &[Option<Value>]) -> Result<Value> {
        match self {
            Self::Constant(value) => Ok(value.clone()),
            Self::Slot(index) => slots
                .get(*index)
                .and_then(Clone::clone)
                .ok_or_else(|| Error("fixed input depends on an unevaluated frame".into())),
        }
    }
}
