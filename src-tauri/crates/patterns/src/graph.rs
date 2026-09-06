use crate::{Error, Frame, Result, Value, ValueType};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rate {
    Fixed,
    Frame,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub name: String,
    pub description: String,
    pub value_type: ValueType,
    pub rate: Rate,
    pub default: Option<Value>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub value_type: ValueType,
    pub rate: Rate,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Optional editor layout. Omitted by code authors; never read by execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    /// Definition ID in this document version's fixed library, or a score-local ID.
    pub definition: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub nodes: BTreeMap<String, Node>,
    pub outputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    FieldBinary(crate::FieldMath),
    Broadcast(crate::ScalarKind),
    FieldClamp,
    MaskToField,
    FieldGreater,
    FieldSelect,
    RandomField,
    ChooseNumber,
    ResolveMapping,
    Rhythm,
    Motion,
    CoordinateOffset,
    FieldEnvelope,
    Appearance,
    WritePosition,
    WriteDimmer,
    WriteStrobe,
    WriteSpeed,
    AddLighting,
    Envelope,
    SoftEdges,
}
impl Primitive {
    /// All other primitives are pure functions of inputs and the prepared
    /// head domain/seed, and may be folded when their inputs are constant.
    pub(crate) fn reads_time(self) -> bool {
        match self {
            Self::Rhythm => true,
            Self::FieldBinary(_)
            | Self::Broadcast(_)
            | Self::FieldClamp
            | Self::MaskToField
            | Self::FieldGreater
            | Self::FieldSelect
            | Self::RandomField
            | Self::ChooseNumber
            | Self::ResolveMapping
            | Self::Motion
            | Self::CoordinateOffset
            | Self::FieldEnvelope
            | Self::Appearance
            | Self::WritePosition
            | Self::WriteDimmer
            | Self::WriteStrobe
            | Self::WriteSpeed
            | Self::AddLighting
            | Self::Envelope
            | Self::SoftEdges => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Body {
    Primitive(Primitive),
    Graph(Graph),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub inputs: BTreeMap<String, Input>,
    pub outputs: BTreeMap<String, Output>,
    pub body: Body,
}
impl Definition {
    /// A score-local, editable instance of any built-in node. Inputs are bindings,
    /// not a synthetic node. The label is optional; editors can derive it from
    /// the referenced node until the author chooses one.
    pub fn instance(&self, definition: &str) -> Self {
        Self {
            name: String::new(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            body: Body::Graph(Graph {
                nodes: BTreeMap::from([(
                    "effect".into(),
                    Node {
                        position: None,
                        definition: definition.into(),
                        inputs: self
                            .inputs
                            .keys()
                            .map(|key| (key.clone(), Binding::Input { input: key.clone() }))
                            .collect(),
                    },
                )]),
                outputs: self
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    pub definitions: BTreeMap<String, Definition>,
}

const MAX_DEFINITION_DEPTH: usize = 24;
pub(crate) const MAX_GRAPH_NODES: usize = 128;
const MAX_EXPANDED_NODES: usize = 8192;
const MAX_EXECUTION_DEPTH: usize = 96;

#[derive(Clone, Copy)]
struct Complexity {
    nodes: usize,
    depth: usize,
}

pub(crate) fn identity(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(Error("identity must contain 1–256 printable bytes".into()));
    }
    if id.starts_with('@') {
        return Err(Error(
            "identities starting with @ are reserved for editor controls".into(),
        ));
    }
    Ok(())
}

impl Library {
    /// Anonymous one-node graphs inherit their node's label in every authoring
    /// surface. Identity remains the reference key, independently of the label.
    pub fn display_name(&self, id: &str) -> String {
        let mut id = id;
        let mut visited = BTreeSet::new();
        while visited.insert(id) {
            let Some(definition) = self.definitions.get(id) else {
                return "Missing graph".into();
            };
            if !definition.name.is_empty() {
                return definition.name.clone();
            }
            if let Body::Graph(graph) = &definition.body {
                if graph.nodes.len() == 1 {
                    id = &graph.nodes.values().next().unwrap().definition;
                    continue;
                }
            }
            break;
        }
        "Custom graph".into()
    }
    pub fn validate(&self, id: &str) -> Result<()> {
        self.validate_many(std::iter::once(id))
    }
    pub(crate) fn validate_many<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
        let mut done = BTreeMap::new();
        for id in ids {
            self.validate_definition(id, &mut BTreeSet::new(), &mut done)?;
        }
        Ok(())
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
        done: &mut BTreeMap<String, Complexity>,
    ) -> Result<()> {
        if done.contains_key(id) {
            return Ok(());
        }
        identity(id)?;
        if visiting.len() >= MAX_DEFINITION_DEPTH {
            return Err(Error(format!(
                "graph nesting exceeds {MAX_DEFINITION_DEPTH} definitions"
            )));
        }
        if !visiting.insert(id.into()) {
            return Err(Error(format!("recursive graph definition {id}")));
        }
        let def = self.definition(id)?;
        if def.inputs.len() > 64 || def.outputs.len() > 32 {
            return Err(Error(format!(
                "{id}: a definition supports at most 64 inputs and 32 outputs"
            )));
        }
        for key in def.inputs.keys().chain(def.outputs.keys()) {
            identity(key)?;
        }
        for (name, input) in &def.inputs {
            if let Some(value) = &input.default {
                value.validate()?;
                if value.value_type() != input.value_type {
                    return Err(Error(format!("{id}.{name}: default type mismatch")));
                }
            }
        }
        let complexity = match &def.body {
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
                Complexity { nodes: 1, depth: 1 }
            }
            Body::Graph(graph) => {
                if graph.nodes.len() > MAX_GRAPH_NODES {
                    return Err(Error(format!(
                        "{id}: a graph supports at most {MAX_GRAPH_NODES} nodes"
                    )));
                }
                for (name, node) in &graph.nodes {
                    identity(name)?;
                    if node
                        .position
                        .is_some_and(|position| position.iter().any(|value| !value.is_finite()))
                    {
                        return Err(Error(format!("{name}: node position must be finite")));
                    }
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
                let nodes = 1 + graph
                    .nodes
                    .values()
                    .map(|node| done[&node.definition].nodes)
                    .sum::<usize>();
                if nodes > MAX_EXPANDED_NODES {
                    return Err(Error(format!(
                        "{id}: graph expansion exceeds {MAX_EXPANDED_NODES} nodes"
                    )));
                }
                let mut depths = BTreeMap::new();
                let mut depth = 1;
                for name in graph.nodes.keys() {
                    depth = depth.max(1 + execution_depth(graph, name, done, &mut depths));
                }
                if depth > MAX_EXECUTION_DEPTH {
                    return Err(Error(format!(
                        "{id}: execution dependency depth exceeds {MAX_EXECUTION_DEPTH}"
                    )));
                }
                Complexity { nodes, depth }
            }
        };
        visiting.remove(id);
        done.insert(id.into(), complexity);
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
        frame.validate()?;
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
// Called only after wire cycles and child definitions have been validated.
// Count nesting and upstream dependencies together before recursive lowering.
fn execution_depth(
    graph: &Graph,
    id: &str,
    definitions: &BTreeMap<String, Complexity>,
    depths: &mut BTreeMap<String, usize>,
) -> usize {
    if let Some(depth) = depths.get(id) {
        return *depth;
    }
    let node = &graph.nodes[id];
    let upstream = node
        .inputs
        .values()
        .filter_map(|binding| match binding {
            Binding::Connection { node, .. } => {
                Some(execution_depth(graph, node, definitions, depths))
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let depth = 1 + definitions[&node.definition].depth + upstream;
    depths.insert(id.into(), depth);
    depth
}

pub(crate) fn check_cycle(
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
    let current = graph
        .nodes
        .get(name)
        .ok_or_else(|| Error(format!("unknown node {name}")))?;
    for b in current.inputs.values() {
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
    if let Some(result) = crate::field_ops::run(p, i, frame) {
        return result;
    }
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
        Primitive::CoordinateOffset => {
            let Value::Boundary(boundary) = i["boundary"] else {
                unreachable!()
            };
            let center = n("position");
            let mut values = BTreeMap::new();
            let mut wrapped = BTreeMap::new();
            for c in &mapping("mapping").coordinates {
                let wrap = boundary == crate::Boundary::Wrap
                    || (boundary == crate::Boundary::Natural && c.closed);
                let delta = c.position - center;
                values.insert(
                    c.cell.clone(),
                    if wrap {
                        (delta + 0.5).rem_euclid(1.0) - 0.5
                    } else {
                        delta
                    },
                );
                wrapped.insert(c.cell.clone(), if wrap { 1.0 } else { 0.0 });
            }
            BTreeMap::from([
                ("value".into(), Value::Field(values)),
                ("wrapped".into(), Value::Mask(wrapped)),
            ])
        }
        Primitive::FieldEnvelope => {
            let Value::Envelope(shape) = &i["shape"] else {
                unreachable!()
            };
            let Value::Field(phase) = &i["phase"] else {
                unreachable!()
            };
            out(
                "mask",
                Value::Mask(
                    phase
                        .iter()
                        .map(|(id, phase)| (id.clone(), shape.sample(*phase)))
                        .collect(),
                ),
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
                            (
                                cell.clone(),
                                crate::FixtureOutput::from_rgb(color.map(|v| v * coverage)),
                            )
                        })
                        .collect(),
                ),
            )
        }
        Primitive::AddLighting => {
            let Value::Lighting(a) = &i["a"] else {
                unreachable!()
            };
            let Value::Lighting(b) = &i["b"] else {
                unreachable!()
            };
            if !a.keys().eq(b.keys()) {
                return Err(Error(
                    "output head domains differ; explicitly select a common domain".into(),
                ));
            }
            let mut sum = a.clone();
            for (cell, top) in b {
                let base = sum.get_mut(cell).unwrap();
                base.composite(top, crate::BlendMode::Add);
            }
            out("lighting", Value::Lighting(sum))
        }
        Primitive::WritePosition
        | Primitive::WriteDimmer
        | Primitive::WriteStrobe
        | Primitive::WriteSpeed => {
            let value = match p {
                Primitive::WritePosition => crate::FixtureOutput {
                    position: Some([n("pan"), n("tilt")]),
                    ..Default::default()
                },
                Primitive::WriteDimmer => crate::FixtureOutput {
                    dimmer: Some(n("value")),
                    ..Default::default()
                },
                Primitive::WriteStrobe => crate::FixtureOutput {
                    strobe: Some(n("value")),
                    ..Default::default()
                },
                Primitive::WriteSpeed => crate::FixtureOutput {
                    speed: Some(n("value")),
                    ..Default::default()
                },
                _ => unreachable!(),
            };
            out(
                "lighting",
                Value::Lighting(
                    frame
                        .cells
                        .iter()
                        .map(|c| (c.id.clone(), value.clone()))
                        .collect(),
                ),
            )
        }
        Primitive::SoftEdges => out(
            "shape",
            Value::Envelope(crate::Envelope::soft_edges(n("softness"))),
        ),
        Primitive::Envelope => {
            let Value::Envelope(e) = &i["shape"] else {
                unreachable!()
            };
            out("value", Value::Proportion(e.sample(n("progress"))))
        }
        _ => unreachable!("fundamental field op handled above"),
    })
}
