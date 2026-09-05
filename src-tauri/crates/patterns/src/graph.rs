use crate::{dissolve, pill, Error, Frame, Result, Value, ValueType};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rate {
    Fixed,
    Frame,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub name: String,
    pub description: String,
    pub value_type: ValueType,
    pub rate: Rate,
    pub default: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub value_type: ValueType,
    pub rate: Rate,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    Value { value: Value },
    Input { input: String },
    Connection { node: String, output: String },
}
impl From<Value> for Binding {
    fn from(value: Value) -> Self {
        Self::Value { value }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Immutable definition/revision ID, not a mutable library name.
    pub definition: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub nodes: BTreeMap<String, Node>,
    pub outputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    ResolveMapping,
    Rhythm,
    Motion,
    Pill,
    Dissolve,
    Appearance,
    MultiplyMask,
    AddLighting,
    Envelope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Body {
    Primitive(Primitive),
    Graph(Graph),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub inputs: BTreeMap<String, Input>,
    pub outputs: BTreeMap<String, Output>,
    pub body: Body,
}
impl Definition {
    pub fn lighting_output(&self) -> Option<&str> {
        let mut outputs = self
            .outputs
            .iter()
            .filter(|(_, output)| output.value_type == ValueType::Lighting);
        let (name, _) = outputs.next()?;
        outputs.next().is_none().then_some(name.as_str())
    }
    pub fn playable(&self) -> bool {
        self.lighting_output().is_some()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    pub definitions: BTreeMap<String, Definition>,
}

impl Library {
    pub fn validate(&self, id: &str) -> Result<()> {
        self.validate_definition(id, &mut BTreeSet::new(), &mut BTreeSet::new())
    }
    fn definition(&self, id: &str) -> Result<&Definition> {
        self.definitions
            .get(id)
            .ok_or_else(|| Error(format!("unknown definition {id}")))
    }
    fn validate_definition(
        &self,
        id: &str,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<()> {
        if done.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.into()) {
            return Err(Error(format!("recursive graph definition {id}")));
        }
        let def = self.definition(id)?;
        for (name, input) in &def.inputs {
            if let Some(value) = &input.default {
                value.validate()?;
                if value.value_type() != input.value_type {
                    return Err(Error(format!("{id}.{name}: default type mismatch")));
                }
            }
        }
        match &def.body {
            Body::Primitive(p) => {
                // Primitive interfaces are owned by the kernel catalog, not editable JSON.
                let canonical = crate::catalog::primitive(*p);
                if serde_json::to_value(&def.inputs).unwrap()
                    != serde_json::to_value(&canonical.inputs).unwrap()
                    || serde_json::to_value(&def.outputs).unwrap()
                        != serde_json::to_value(&canonical.outputs).unwrap()
                {
                    return Err(Error(format!(
                        "{id}: primitive interface differs from its kernel"
                    )));
                }
            }
            Body::Graph(graph) => {
                for (name, node) in &graph.nodes {
                    self.validate_definition(&node.definition, visiting, done)?;
                    let child = self.definition(&node.definition)?;
                    for key in node.inputs.keys() {
                        if !child.inputs.contains_key(key) {
                            return Err(Error(format!("{name}: unknown input {key}")));
                        }
                    }
                    for (key, input) in &child.inputs {
                        match node.inputs.get(key) {
                            Some(binding) => self.check_binding(
                                def,
                                graph,
                                binding,
                                input.value_type,
                                input.rate,
                            )?,
                            None if input.default.is_some() => (),
                            None => {
                                return Err(Error(format!(
                                    "{name}: required input {key} is unconnected"
                                )))
                            }
                        }
                    }
                }
                for (name, output) in &def.outputs {
                    let binding = graph
                        .outputs
                        .get(name)
                        .ok_or_else(|| Error(format!("{id}: missing output {name}")))?;
                    self.check_binding(def, graph, binding, output.value_type, output.rate)?;
                }
                if graph.outputs.len() != def.outputs.len() {
                    return Err(Error(format!("{id}: undeclared graph output")));
                }
                let mut finished = BTreeSet::new();
                for name in graph.nodes.keys() {
                    check_cycle(graph, name, &mut BTreeSet::new(), &mut finished)?;
                }
            }
        }
        visiting.remove(id);
        done.insert(id.into());
        Ok(())
    }
    fn check_binding(
        &self,
        def: &Definition,
        graph: &Graph,
        binding: &Binding,
        expected: ValueType,
        rate: Rate,
    ) -> Result<()> {
        let (actual, actual_rate) = match binding {
            Binding::Value { value } => {
                value.validate()?;
                (value.value_type(), Rate::Fixed)
            }
            Binding::Input { input } => {
                let i = def
                    .inputs
                    .get(input)
                    .ok_or_else(|| Error(format!("unknown exposed input {input}")))?;
                (i.value_type, i.rate)
            }
            Binding::Connection { node, output } => {
                let n = graph
                    .nodes
                    .get(node)
                    .ok_or_else(|| Error(format!("unknown node {node}")))?;
                let o = self
                    .definition(&n.definition)?
                    .outputs
                    .get(output)
                    .ok_or_else(|| Error(format!("{node}: unknown output {output}")))?;
                (o.value_type, o.rate)
            }
        };
        if expected != actual {
            return Err(Error(format!("expected {expected:?}, got {actual:?}")));
        }
        if rate == Rate::Fixed && actual_rate == Rate::Frame {
            return Err(Error(
                "frame-varying wire connected to a fixed input".into(),
            ));
        }
        Ok(())
    }
    /// Validates before executing. A future compiled plan may cache validation;
    /// the public boundary never accepts unchecked graph JSON.
    pub fn evaluate(
        &self,
        id: &str,
        overrides: &BTreeMap<String, Value>,
        frame: Frame,
    ) -> Result<BTreeMap<String, Value>> {
        if !frame.beat.is_finite() || !frame.clip_start.is_finite() {
            return Err(Error("musical time must be finite".into()));
        }
        self.validate(id)?;
        self.run(id, overrides, frame)
    }
    fn run(
        &self,
        id: &str,
        overrides: &BTreeMap<String, Value>,
        frame: Frame,
    ) -> Result<BTreeMap<String, Value>> {
        let def = self.definition(id)?;
        let mut inputs = BTreeMap::new();
        for key in overrides.keys() {
            if !def.inputs.contains_key(key) {
                return Err(Error(format!("{id}: unknown input {key}")));
            }
        }
        for (name, input) in &def.inputs {
            let v = overrides
                .get(name)
                .or(input.default.as_ref())
                .ok_or_else(|| Error(format!("{id}: required input {name}")))?;
            v.validate()?;
            if v.value_type() != input.value_type {
                return Err(Error(format!(
                    "{id}.{name}: expected {:?}, got {:?}",
                    input.value_type,
                    v.value_type()
                )));
            }
            inputs.insert(name.clone(), v.clone());
        }
        let outputs: BTreeMap<String, Value> = match &def.body {
            Body::Primitive(p) => run_primitive(*p, &inputs, frame),
            Body::Graph(graph) => {
                let mut cache = BTreeMap::new();
                graph
                    .outputs
                    .iter()
                    .map(|(name, binding)| {
                        Ok((
                            name.clone(),
                            self.resolve(graph, binding, &inputs, frame, &mut cache)?,
                        ))
                    })
                    .collect()
            }
        }?;
        for value in outputs.values() {
            value.validate()?;
        }
        Ok(outputs)
    }
    fn resolve(
        &self,
        graph: &Graph,
        binding: &Binding,
        inputs: &BTreeMap<String, Value>,
        frame: Frame,
        cache: &mut BTreeMap<String, BTreeMap<String, Value>>,
    ) -> Result<Value> {
        match binding {
            Binding::Value { value } => Ok(value.clone()),
            Binding::Input { input } => Ok(inputs[input].clone()),
            Binding::Connection { node, output } => {
                if !cache.contains_key(node) {
                    let n = &graph.nodes[node];
                    let values = n
                        .inputs
                        .iter()
                        .map(|(name, b)| {
                            Ok((name.clone(), self.resolve(graph, b, inputs, frame, cache)?))
                        })
                        .collect::<Result<_>>()?;
                    cache.insert(node.clone(), self.run(&n.definition, &values, frame)?);
                }
                Ok(cache[node][output].clone())
            }
        }
    }
}
fn check_cycle(
    graph: &Graph,
    name: &str,
    visiting: &mut BTreeSet<String>,
    done: &mut BTreeSet<String>,
) -> Result<()> {
    if done.contains(name) {
        return Ok(());
    }
    if !visiting.insert(name.into()) {
        return Err(Error(format!("wire cycle at {name}")));
    }
    for b in graph.nodes[name].inputs.values() {
        if let Binding::Connection { node, .. } = b {
            check_cycle(graph, node, visiting, done)?;
        }
    }
    visiting.remove(name);
    done.insert(name.into());
    Ok(())
}

pub(crate) fn run_primitive(
    p: Primitive,
    i: &BTreeMap<String, Value>,
    frame: Frame,
) -> Result<BTreeMap<String, Value>> {
    let n = |key: &str| i[key].scalar();
    let mapping = |key: &str| match &i[key] {
        Value::Coordinates(m) => m,
        _ => unreachable!(),
    };
    let mask = |key: &str| match &i[key] {
        Value::Mask(m) => m,
        _ => unreachable!(),
    };
    let out = |name: &str, value| BTreeMap::from([(name.into(), value)]);
    Ok(match p {
        Primitive::ResolveMapping => {
            let Value::Mapping(spec) = &i["mapping"] else {
                unreachable!()
            };
            out(
                "coordinates",
                Value::Coordinates(spec.resolve(frame.cells)?),
            )
        }
        Primitive::Rhythm => {
            let period = n("repeat");
            if period <= 0.0 {
                return Err(Error("repeat interval must be greater than zero".into()));
            }
            let origin = if i["grid_aligned"] == Value::Boolean(true) {
                0.0
            } else {
                frame.clip_start
            };
            let elapsed = frame.beat - origin;
            let cycle = (elapsed / period).floor();
            BTreeMap::from([
                ("elapsed".into(), Value::Beats(elapsed.rem_euclid(period))),
                ("cycle".into(), Value::Number(cycle)),
            ])
        }
        Primitive::Motion => {
            let travel = n("travel");
            let repeat = n("repeat");
            if travel <= 0.0 || repeat < travel {
                return Err(Error("travel must be positive and no longer than repeat; overlap needs explicit composition".into()));
            }
            let elapsed = n("elapsed");
            let progress = (elapsed / travel).clamp(0.0, 1.0);
            BTreeMap::from([
                (
                    "position".into(),
                    Value::Position(n("start") + progress * (n("end") - n("start"))),
                ),
                ("progress".into(), Value::Proportion(progress)),
                (
                    "active".into(),
                    Value::Proportion(if elapsed < travel { 1.0 } else { 0.0 }),
                ),
            ])
        }
        Primitive::Pill => {
            let Value::Boundary(boundary) = i["boundary"] else {
                unreachable!()
            };
            let mut m = pill(
                mapping("mapping"),
                n("position"),
                n("width"),
                n("softness"),
                boundary,
            );
            for v in m.values_mut() {
                *v *= n("active");
            }
            out("mask", Value::Mask(m))
        }
        Primitive::Dissolve => {
            let cycle = if i["reseed"] == Value::Boolean(true) {
                n("cycle") as i64 as u64
            } else {
                0
            };
            out(
                "mask",
                Value::Mask(dissolve(
                    mapping("mapping"),
                    n("progress"),
                    n("softness"),
                    frame.seed ^ cycle.wrapping_mul(0x9e3779b97f4a7c15),
                )),
            )
        }
        Primitive::Appearance => {
            let Value::Color(color) = i["color"] else {
                unreachable!()
            };
            out(
                "lighting",
                Value::Lighting(
                    mask("mask")
                        .iter()
                        .map(|(cell, coverage)| {
                            (cell.clone(), color.map(|v| v * coverage * n("brightness")))
                        })
                        .collect(),
                ),
            )
        }
        Primitive::MultiplyMask => {
            let a = mask("a");
            let b = mask("b");
            if !a.keys().eq(b.keys()) {
                return Err(Error(
                    "mask cell domains differ; explicitly select a common domain".into(),
                ));
            }
            out(
                "mask",
                Value::Mask(a.iter().map(|(k, v)| (k.clone(), v * b[k])).collect()),
            )
        }
        Primitive::AddLighting => {
            let Value::Lighting(a) = &i["a"] else {
                unreachable!()
            };
            let Value::Lighting(b) = &i["b"] else {
                unreachable!()
            };
            let mut sum = a.clone();
            for (cell, color) in b {
                let dest = sum.entry(cell.clone()).or_insert([0.0; 3]);
                for ch in 0..3 {
                    dest[ch] += color[ch];
                }
            }
            out("lighting", Value::Lighting(sum))
        }
        Primitive::Envelope => {
            let Value::Envelope(e) = &i["shape"] else {
                unreachable!()
            };
            out("value", Value::Proportion(e.sample(n("progress"))))
        }
    })
}
