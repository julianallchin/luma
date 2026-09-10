//! Validated, flattened graphs. Mapping/other fixed outputs are computed once;
//! the frame loop never walks definitions or repeats geometry solves.
use crate::{
    runtime::{self, Batch, EvaluatedValue},
    Binding, Body, Cell, Error, Frame, Graph, Library, Primitive, Result, Value,
};
use std::collections::BTreeMap;
mod clip_range;

#[derive(Clone, Debug)]
enum Source {
    Constant(EvaluatedValue),
    Slot(usize),
}
#[derive(Clone, Debug)]
struct Step {
    label: String,
    primitive: Primitive,
    inputs: BTreeMap<String, Source>,
    input_types: BTreeMap<String, crate::Input>,
    outputs: BTreeMap<String, usize>,
    output_types: BTreeMap<String, crate::Output>,
}
#[derive(Clone, Debug)]
pub struct PreparedGraph {
    steps: Vec<Step>,
    outputs: BTreeMap<String, Source>,
    output_types: BTreeMap<String, crate::Output>,
    slots: usize,
    cells: Vec<Cell>,
    fixtures: Vec<String>,
    clip_start: f64,
    clip_duration: f64,
    seed: u64,
    features: Option<std::sync::Arc<dyn crate::FeatureSource>>,
    requests: Vec<crate::FeatureRequest>,
    baked: Vec<Option<EvaluatedValue>>,
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
            output_types: library.definitions[definition].outputs.clone(),
            slots: 0,
            cells: frame.cells.to_vec(),
            fixtures: {
                let mut ids: Vec<_> = frame.cells.iter().map(|c| c.id.clone()).collect();
                ids.sort();
                ids
            },
            clip_start: frame.clip_start,
            clip_duration: frame.clip_duration,
            seed: frame.seed,
            features: None,
            requests: Vec::new(),
            baked: Vec::new(),
        };
        let bound = inputs
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    Source::Constant(EvaluatedValue::literal(value)?),
                ))
            })
            .collect::<Result<_>>()?;
        prepared.outputs = prepared.lower(library, definition, bound)?;
        // A nested definition may offer several independent outputs. Only the
        // connected ones belong to this program, including during preparation.
        let live = prepared.live_steps(prepared.steps.len(), &[], prepared.outputs.values());
        prepared.steps = std::mem::take(&mut prepared.steps)
            .into_iter()
            .enumerate()
            .filter_map(|(index, step)| live.binary_search(&index).is_ok().then_some(step))
            .collect();
        for step in &prepared.steps {
            let constants = step
                .inputs
                .iter()
                .filter_map(|(name, source)| match source {
                    Source::Constant(value) => {
                        Some(value.sample(0).map(|value| (name.clone(), value)))
                    }
                    Source::Slot(_) => None,
                })
                .collect::<Result<_>>()?;
            if let Some(request) = crate::features::request(step.primitive, &constants)? {
                if !prepared.requests.contains(&request) {
                    prepared.requests.push(request);
                }
            }
        }
        // Validate fixed controls during preparation, before the first device
        // tick. Dynamic errors still propagate from render.
        if prepared.requests.is_empty() {
            prepared.baked = prepared.prepare_fixed(None)?;
            prepared.evaluate(frame.clip_start)?;
        }
        Ok(prepared)
    }
    pub fn feature_requests(&self) -> &[crate::FeatureRequest] {
        &self.requests
    }
    pub fn output_types(&self) -> &BTreeMap<String, crate::Output> {
        &self.output_types
    }
    pub fn with_features(
        mut self,
        features: std::sync::Arc<dyn crate::FeatureSource>,
    ) -> Result<Self> {
        self.features = Some(features);
        self.baked = self.prepare_fixed(self.features.as_deref())?;
        self.evaluate(self.clip_start)?;
        Ok(self)
    }
    pub fn dynamic_step_count(&self) -> usize {
        self.live_steps(self.steps.len(), &self.baked, self.outputs.values())
            .len()
    }

    fn live_steps<'a>(
        &self,
        before: usize,
        baked: &[Option<EvaluatedValue>],
        roots: impl IntoIterator<Item = &'a Source>,
    ) -> Vec<usize> {
        let mut needed = vec![false; self.slots];
        for source in roots {
            if let Source::Slot(slot) = source {
                needed[*slot] = true;
            }
        }
        let mut steps = Vec::new();
        for index in (0..before).rev() {
            let step = &self.steps[index];
            if step
                .outputs
                .values()
                .any(|slot| needed[*slot] && baked.get(*slot).is_none_or(Option::is_none))
            {
                for source in step.inputs.values() {
                    if let Source::Slot(slot) = source {
                        needed[*slot] = true;
                    }
                }
                steps.push(index);
            }
        }
        steps.reverse();
        steps
    }
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
    pub fn evaluate(&self, beat: f64) -> Result<BTreeMap<String, Value>> {
        self.evaluate_batch(&[beat])?
            .into_iter()
            .map(|(key, value)| Ok((key, value.sample(0)?)))
            .collect()
    }
    pub fn evaluate_batch(&self, beats: &[f64]) -> Result<BTreeMap<String, EvaluatedValue>> {
        self.evaluate_cached(beats, self.features.as_deref(), &self.baked)
    }
    pub(crate) fn evaluate_using(
        &self,
        beats: &[f64],
        features: Option<&dyn crate::FeatureSource>,
    ) -> Result<BTreeMap<String, EvaluatedValue>> {
        let baked = self.prepare_fixed(features)?;
        self.evaluate_cached(beats, features, &baked)
    }
    fn evaluate_cached(
        &self,
        beats: &[f64],
        features: Option<&dyn crate::FeatureSource>,
        baked: &[Option<EvaluatedValue>],
    ) -> Result<BTreeMap<String, EvaluatedValue>> {
        if beats.iter().any(|beat| !beat.is_finite()) {
            return Err(Error("musical time must be finite".into()));
        }
        let clock = crate::Signal::series(beats, crate::Unit::Number)?;
        let mut slots = if baked.is_empty() {
            vec![None; self.slots]
        } else {
            baked.to_vec()
        };
        for index in self.live_steps(self.steps.len(), baked, self.outputs.values()) {
            let step = &self.steps[index];
            let outputs = self.run_step(step, beats, &clock, features, &slots)?;
            for (name, value) in outputs {
                slots[step.outputs[&name]] = Some(value);
            }
        }
        self.outputs
            .iter()
            .map(|(name, source)| {
                Ok((
                    name.clone(),
                    source
                        .read(&slots)?
                        .declared(self.output_types[name].value_type)?,
                ))
            })
            .collect()
    }
    fn run_step(
        &self,
        step: &Step,
        beats: &[f64],
        clock: &crate::Signal,
        features: Option<&dyn crate::FeatureSource>,
        slots: &[Option<EvaluatedValue>],
    ) -> Result<BTreeMap<String, EvaluatedValue>> {
        let inputs = step
            .inputs
            .iter()
            .map(|(key, source)| Ok((key.clone(), source.read(slots)?)))
            .collect::<Result<_>>()?;
        runtime::run(
            step.primitive,
            &inputs,
            &step.input_types,
            &step.output_types,
            Batch {
                times: beats,
                clock,
                fixtures: &self.fixtures,
                frame: Frame {
                    features,
                    beat: self.clip_start,
                    clip_start: self.clip_start,
                    clip_duration: self.clip_duration,
                    seed: self.seed,
                    cells: &self.cells,
                },
            },
        )
        .map_err(|error| Error(format!("{}: {error}", step.label)))
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
                if input.optional {
                    continue;
                }
                let value = input
                    .default
                    .as_ref()
                    .ok_or_else(|| Error(format!("{id}: required input {name}")))?;
                inputs.insert(
                    name.clone(),
                    Source::Constant(EvaluatedValue::literal(value)?),
                );
            }
            if let Source::Constant(value) = &inputs[name] {
                if !input.value_type.accepts(value.kind()) {
                    return Err(Error(format!(
                        "{id}.{name}: expected {:?}, got {:?}",
                        input.value_type,
                        value.kind()
                    )));
                }
            }
        }
        match &definition.body {
            Body::Primitive(primitive) => {
                let constants: BTreeMap<_, _> = inputs
                    .iter()
                    .filter_map(|(name, source)| match source {
                        Source::Constant(value) => {
                            Some(value.sample(0).map(|value| (name.clone(), value)))
                        }
                        _ => None,
                    })
                    .collect::<Result<_>>()?;
                validate_parameters(*primitive, &constants)?;
                if !primitive.reads_track()
                    && !primitive.reads_time()
                    && inputs
                        .values()
                        .all(|input| matches!(input, Source::Constant(_)))
                {
                    let values = inputs
                        .iter()
                        .map(|(key, source)| Ok((key.clone(), source.read(&[])?)))
                        .collect::<Result<_>>()?;
                    let result = runtime::run(
                        *primitive,
                        &values,
                        &definition.inputs,
                        &definition.outputs,
                        Batch {
                            clock: &crate::Signal::scalar(self.clip_start, crate::Unit::Number)?,
                            fixtures: &self.fixtures,
                            times: &[self.clip_start],
                            frame: Frame {
                                features: self.features.as_deref(),
                                beat: self.clip_start,
                                clip_start: self.clip_start,
                                clip_duration: self.clip_duration,
                                seed: self.seed,
                                cells: &self.cells,
                            },
                        },
                    )?;
                    result
                        .into_iter()
                        .map(|(name, value)| Ok((name, Source::Constant(value))))
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
                        label: definition.name.clone(),
                        primitive: *primitive,
                        inputs,
                        input_types: definition.inputs.clone(),
                        outputs,
                        output_types: definition.outputs.clone(),
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
            Binding::Value { value } => Ok(Source::Constant(EvaluatedValue::literal(value)?)),
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
    crate::event_tensor::validate_parameters(op, inputs)?;
    let number = |name| inputs.get(name).map(Value::scalar);
    match op {
        Primitive::ClipRange => {
            if let Some(samples) = number("samples") {
                crate::clip_range::sample_count(samples)?;
            }
            Ok(())
        }
        Primitive::Rhythm if number("repeat").is_some_and(|v| v <= 0.0) => {
            Err(Error("repeat interval must be greater than zero".into()))
        }
        _ => Ok(()),
    }
}
impl Source {
    fn read(&self, slots: &[Option<EvaluatedValue>]) -> Result<EvaluatedValue> {
        match self {
            Self::Constant(value) => Ok(value.clone()),
            Self::Slot(index) => slots
                .get(*index)
                .and_then(Clone::clone)
                .ok_or_else(|| Error("fixed input depends on an unevaluated frame".into())),
        }
    }
}
