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
    /// Optional terminal sockets are unwritten until bound. Their default is
    /// an editor seed; required inputs use their default during execution.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
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
    /// Named parameter sockets in the editor. Type/default metadata lives in
    /// Definition::inputs after the first connection; unconnected new sockets
    /// need no invented value type and do not participate in execution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_nodes: BTreeMap<String, InputNode>,
    pub nodes: BTreeMap<String, Node>,
    pub outputs: BTreeMap<String, Binding>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
}

impl InputNode {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.name.chars().any(char::is_control) {
            return Err(Error("an Input needs a name".into()));
        }
        if self
            .position
            .is_some_and(|p| p.iter().any(|v| !v.is_finite()))
        {
            return Err(Error("Input position must be finite".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    Output,
    FieldBinary(crate::FieldMath),
    ScalarBinary(crate::FieldMath),
    ScalarConvert {
        from: crate::ScalarKind,
        to: crate::ScalarKind,
    },
    FieldUnary(crate::UnaryMath),
    Power,
    FieldRank,
    FieldFirst,
    FieldReduce(crate::FieldReduction),
    ChannelMaximum,
    ChannelSum,
    ChannelArgmax,
    ChannelIndex,
    ChannelCount,
    Channel,
    JoinChannels,
    StageCoordinates,
    WorldGeometry,
    WanderPoints,
    ProximityWeights,
    RadialCoordinates,
    CirclePhase,
    PrincipalDirection,
    RankNearby,
    ClipTime,
    ClipRange,
    BandEnergy,
    AudioSpectrum,
    FilterAudio {
        highpass: bool,
    },
    DrumClock,
    BeatEvents,
    DrumEvents,
    TrackTime,
    GridEvents,
    EventWindow,
    EventSpacing,
    ThinEvents,
    RandomEventTargets,
    ChaseEvents,
    PulseEvents,
    DissolveEvents,
    Harmony,
    Noise,
    ValueNoise1d,
    ValueNoise3d,
    SeedStream,
    DomainIndex,
    AlignDomain,
    WriteMask,
    WriteStrobeMask,
    SampleGradient,
    SampleGradientField,
    MixPalette,
    PaletteFallback,
    ColorField,
    MaskColor,
    WriteColor,
    Hsv,
    RotateHue,
    Broadcast(crate::ScalarKind),
    FieldClamp,
    MaskToField,
    FieldGreater,
    FieldSelect,
    RandomField,
    ChooseNumber,
    ResolveMapping,
    Rhythm,
    TravelClock,
    CoordinateOffset,
    FieldEnvelope,
    WritePosition,
    WriteSpeed,
    AddLighting,
    Envelope,
    SoftEdges,
}
impl Primitive {
    pub(crate) fn reads_track(self) -> bool {
        matches!(
            self,
            Self::BandEnergy
                | Self::AudioSpectrum
                | Self::DrumClock
                | Self::DrumEvents
                | Self::TrackTime
                | Self::GridEvents
                | Self::EventWindow
                | Self::EventSpacing
                | Self::ThinEvents
                | Self::Harmony
        )
    }
    /// All other primitives are pure functions of inputs and the prepared
    /// head domain/seed, and may be folded when their inputs are constant.
    pub(crate) fn reads_time(self) -> bool {
        matches!(
            self,
            Self::Rhythm
                | Self::ClipTime
                | Self::ChaseEvents
                | Self::PulseEvents
                | Self::DissolveEvents
                | Self::TrackTime
                | Self::BandEnergy
                | Self::AudioSpectrum
                | Self::DrumClock
                | Self::Harmony
        )
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
            inputs: self
                .inputs
                .iter()
                .map(|(key, input)| {
                    let mut input = input.clone();
                    input.optional = false;
                    (key.clone(), input)
                })
                .collect(),
            outputs: self.outputs.clone(),
            body: Body::Graph(Graph {
                input_nodes: BTreeMap::new(),
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

    /// A complete numerical effect can be placed by connecting its named
    /// capability signals to Output. Arbitrary helper signals need explicit wiring.
    pub fn placeable(&self) -> bool {
        if self.playable() {
            return true;
        }
        let terminal = crate::output::terminal_definition();
        !self.outputs.contains_key("lighting")
            && !self
                .outputs
                .values()
                .any(|output| output.value_type == ValueType::Lighting)
            && self.outputs.iter().any(|(key, output)| {
                terminal
                    .inputs
                    .get(key)
                    .is_some_and(|input| input.value_type.accepts(output.value_type))
            })
            && self.outputs.iter().all(|(key, output)| {
                terminal
                    .inputs
                    .get(key)
                    .is_none_or(|input| input.value_type.accepts(output.value_type))
            })
    }

    /// A placed effect is an editable graph with a visible terminal. Only its
    /// supplied capability signals are connected; color can be set independently
    /// on Output without changing the effect or its trigger.
    pub fn clip_instance(&self, definition: &str) -> Result<Definition> {
        if !self.placeable() {
            return Err(Error(
                "connect this graph's signals to Output before placing it".into(),
            ));
        }
        let mut instance = self.instance(definition);
        if self.playable() {
            return Ok(instance);
        }
        instance.name = self.name.clone();
        let Body::Graph(graph) = &mut instance.body else {
            unreachable!()
        };
        let terminal = crate::output::terminal_definition();
        let mut inputs = BTreeMap::new();
        graph.outputs.retain(|key, binding| {
            if terminal.inputs.contains_key(key) {
                inputs.insert(key.clone(), binding.clone());
                false
            } else {
                true
            }
        });
        instance
            .outputs
            .retain(|key, _| !terminal.inputs.contains_key(key));
        graph.nodes.insert(
            "output".into(),
            Node {
                position: None,
                definition: "output".into(),
                inputs,
            },
        );
        graph.outputs.insert(
            "lighting".into(),
            Binding::Connection {
                node: "output".into(),
                output: "lighting".into(),
            },
        );
        instance.outputs.extend(terminal.outputs);
        Ok(instance)
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
// A signal operation and its definition each contribute a stack level.
// Composed envelope arithmetic needs more than 96; retain a finite bound below
// the separate node/expansion budgets and use it in inference too.
pub(crate) const MAX_EXECUTION_DEPTH: usize = 192;

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
        self.validate_many_using(ids, &crate::catalog::primitive)
    }
    pub(crate) fn validate_many_using<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
        interface: &impl Fn(Primitive) -> Definition,
    ) -> Result<()> {
        let mut done = BTreeMap::new();
        for id in ids {
            self.validate_definition(id, &mut BTreeSet::new(), &mut done, interface)?;
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
        interface: &impl Fn(Primitive) -> Definition,
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
                value.validate().map_err(|error| {
                    Error(format!("graph {id}, input {name}, default: {error}"))
                })?;
                if !input.value_type.accepts(value.value_type()) {
                    return Err(Error(format!("{id}.{name}: default type mismatch")));
                }
            }
        }
        let complexity = match &def.body {
            Body::Primitive(p) => {
                // Primitive interfaces are owned by the kernel catalog, not editable JSON.
                let canonical = interface(*p);
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
                if def.inputs.values().any(|input| input.optional) {
                    return Err(Error(
                        "graph Inputs always supply a value; only Output sockets may be unwritten"
                            .into(),
                    ));
                }
                if graph.input_nodes.len() > MAX_GRAPH_NODES {
                    return Err(Error(format!(
                        "a graph supports at most {MAX_GRAPH_NODES} Inputs"
                    )));
                }
                for (key, node) in &graph.input_nodes {
                    identity(key)?;
                    node.validate()?;
                    if def
                        .inputs
                        .get(key)
                        .is_some_and(|spec| spec.name != node.name)
                    {
                        return Err(Error(format!(
                            "Input {key}: name differs from its interface"
                        )));
                    }
                }
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
                    self.validate_definition(&node.definition, visiting, done, interface)?;
                    let child = self.definition(&node.definition)?;
                    for key in node.inputs.keys() {
                        if !child.inputs.contains_key(key) {
                            return Err(Error(format!("{name}: unknown input {key}")));
                        }
                    }
                    for (key, input) in &child.inputs {
                        match node.inputs.get(key) {
                            Some(binding) => self
                                .check_binding(def, graph, binding, input.value_type, input.rate)
                                .map_err(|error| {
                                    Error(format!("graph {id}, node {name}, input {key}: {error}"))
                                })?,
                            None if input.optional || input.default.is_some() => (),
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
                    self.check_binding(def, graph, binding, output.value_type, output.rate)
                        .map_err(|error| Error(format!("graph {id}, output {name}: {error}")))?;
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
                        "{id}: execution dependency depth {depth} exceeds {MAX_EXECUTION_DEPTH}"
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
        let (actual, actual_rate) = self.binding_type(&def.inputs, graph, binding)?;
        if !expected.accepts(actual) {
            return Err(Error(format!("expected {expected:?}, got {actual:?}")));
        }
        if rate == Rate::Fixed && actual_rate == Rate::Frame {
            return Err(Error(
                "frame-varying wire connected to a fixed input".into(),
            ));
        }
        Ok(())
    }
    /// Authoring convenience API, using the same flattened tensor program as playback.
    pub fn evaluate(
        &self,
        id: &str,
        overrides: &BTreeMap<String, Value>,
        frame: Frame,
    ) -> Result<BTreeMap<String, Value>> {
        let prepared = crate::PreparedGraph::new(
            self,
            id,
            overrides,
            Frame {
                features: None,
                ..frame
            },
        )?;
        prepared
            .evaluate_using(&[frame.beat], frame.features)?
            .into_iter()
            .map(|(key, value)| Ok((key, value.sample(0)?)))
            .collect()
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
