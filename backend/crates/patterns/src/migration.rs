//! Pure, versioned authored-document conversion. The caller owns persistence and
//! history: upgrading a copy must never change the revision of historical bytes.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// The original vocabulary is data, not another execution engine.
pub fn v2_library() -> Library {
    static LIBRARY: std::sync::OnceLock<Library> = std::sync::OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            serde_json::from_str(include_str!("../migrations/v2-library.json"))
                .expect("checked-in v2 catalog")
        })
        .clone()
}

/// Validate historical content against its own vocabulary, without upgrading or
/// changing the source/revision. History readers and restores use this boundary.
pub fn validate_v2(score: &Score) -> Result<()> {
    if score.version != 2 {
        return Err(Error("expected a version 2 score".into()));
    }
    let frozen = v2_library();
    score
        .validate_using(&frozen, &|op| {
            frozen
                .definitions
                .values()
                .find(|d| d.body == Body::Primitive(op))
                .expect("v2 kernel vocabulary")
                .clone()
        })
        .map(|_| ())
}

/// Lower a standalone historical pattern using the same conversion as a score.
/// Hosts decode their old node/edge storage into this graph before execution.
pub fn upgrade_v2_definition(id: &str, definition: Definition) -> Result<Score> {
    upgrade_v2(&Score {
        version: 2,
        definitions: BTreeMap::from([(id.into(), definition)]),
        clips: BTreeMap::new(),
    })
}

/// Convert a v2 score into a reviewable v3 candidate. Clip identity, argument
/// keys/defaults, selection, timing, seeds and layering remain intact. Intermediate
/// capability bundles become numerical ports; only clip terminals pack output.
/// Nothing is written, and an error leaves the source completely untouched.
pub fn upgrade_v2(score: &Score) -> Result<Score> {
    let target = standard_library();
    if score.version == 3 {
        score.validate(&target)?;
        return Ok(score.clone());
    }
    if score.version != 2 {
        return Err(Error(format!(
            "cannot migrate score version {}",
            score.version
        )));
    }
    validate_v2(score)?;
    let source = source_with_events(score)?;
    let mut conversion = Conversion::new(&source, &target, score.definitions.keys().cloned());
    let mut clips = score.clips.clone();
    for clip in clips.values_mut() {
        clip.graph = conversion
            .definition(&clip.graph, BTreeMap::new(), true)?
            .id;
    }
    // Preserve unused authored helpers too. A helper accepting an old bundle
    // retains a general explicit capability interface; actual calls specialize
    // it to exactly the capabilities written by their upstream graph.
    for (id, definition) in &score.definitions {
        let shapes: Shapes = definition
            .inputs
            .iter()
            .filter(|(_, p)| p.value_type == ValueType::Lighting)
            .map(|(key, _)| (key.clone(), Capability::ALL.into_iter().collect()))
            .collect();
        let terminal = definition.playable() && shapes.is_empty();
        conversion.definition(id, shapes, terminal)?;
    }
    let upgraded = Score {
        version: 3,
        definitions: conversion.definitions,
        clips,
    };
    upgraded.validate(&target)?;
    Ok(upgraded)
}

fn source_with_events(score: &Score) -> Result<Library> {
    let mut source = score.library(&v2_library())?;
    // These public interfaces keep their old controls/defaults. Their internals
    // now consume event timestamps, so increasing travel cannot truncate a tail.
    let recipes: BTreeMap<String, Definition> =
        serde_json::from_str(include_str!("../migrations/v2-event-recipes.json"))
            .expect("versioned event conversion recipes");
    // A new kernel name was a legal authored name in v2. Never overwrite an
    // authored helper while introducing the migration's event vocabulary.
    let mut taken: BTreeSet<_> = source
        .definitions
        .keys()
        .chain(recipes.keys())
        .cloned()
        .collect();
    let remap: BTreeMap<_, _> = recipes
        .keys()
        .filter(|id| score.definitions.contains_key(*id))
        .map(|id| (id.clone(), unique(&mut taken, &format!("{id}/v3"))))
        .collect();
    for (id, mut definition) in recipes {
        if let Body::Graph(graph) = &mut definition.body {
            for node in graph.nodes.values_mut() {
                if let Some(replacement) = remap.get(&node.definition) {
                    node.definition = replacement.clone();
                }
            }
        }
        source
            .definitions
            .insert(remap.get(&id).unwrap_or(&id).clone(), definition);
    }
    Ok(source)
}

type Shapes = BTreeMap<String, BTreeSet<Capability>>;

/// The capabilities actually written by a historical bundle. Migration keeps
/// absent capabilities absent while replacing bundles with numerical wires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    Color,
    Dimmer,
    Pan,
    Tilt,
    Strobe,
    Speed,
}
impl Capability {
    const ALL: [Self; 6] = [
        Self::Color,
        Self::Dimmer,
        Self::Pan,
        Self::Tilt,
        Self::Strobe,
        Self::Speed,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Dimmer => "dimmer",
            Self::Pan => "pan",
            Self::Tilt => "tilt",
            Self::Strobe => "strobe",
            Self::Speed => "speed",
        }
    }
    fn kind(self) -> ValueType {
        ValueType::Signal(SignalType::new(
            if matches!(self, Self::Pan | Self::Tilt) {
                Unit::Degrees
            } else {
                Unit::Proportion
            },
            if self == Self::Color {
                Channels::Rgb
            } else {
                Channels::Value
            },
        ))
    }
    fn fallback(self) -> Binding {
        match self {
            Self::Color => Value::Color([1.0; 3]),
            Self::Speed => Value::Proportion(1.0),
            Self::Pan | Self::Tilt => Value::Degrees(0.0),
            _ => Value::Proportion(0.0),
        }
        .into()
    }
}

/// Port correspondence after migration: one value or separate capability wires.
#[derive(Clone, Debug)]
pub enum Expanded<T> {
    Single(T),
    Bundle(BTreeMap<Capability, T>),
}
type Operand = Expanded<Binding>;
impl Operand {
    fn single(&self) -> Result<Binding> {
        match self {
            Self::Single(value) => Ok(value.clone()),
            Self::Bundle(_) => Err(Error(
                "migration expected a numerical or structured value".into(),
            )),
        }
    }
    fn bundle(&self) -> Result<&BTreeMap<Capability, Binding>> {
        match self {
            Self::Bundle(value) => Ok(value),
            Self::Single(_) => Err(Error("migration expected a v2 capability bundle".into())),
        }
    }
}
#[derive(Clone)]
struct Converted {
    id: String,
    inputs: BTreeMap<String, Expanded<String>>,
    outputs: BTreeMap<String, Expanded<String>>,
}

/// A historical node imported without a terminal, for composition with other
/// numerical graphs. Definitions are ordinary score-owned graphs.
pub struct NodeUpgrade {
    pub definitions: BTreeMap<String, Definition>,
    pub definition: String,
    pub inputs: BTreeMap<String, Expanded<String>>,
    pub outputs: BTreeMap<String, Expanded<String>>,
}

pub fn upgrade_v2_node(
    id: &str,
    capabilities: BTreeMap<String, BTreeSet<Capability>>,
) -> Result<NodeUpgrade> {
    let target = standard_library();
    let source = source_with_events(&Score::default())?;
    let old = source
        .definitions
        .get(id)
        .ok_or_else(|| Error(format!("unknown historical node {id}")))?;
    for key in capabilities.keys() {
        if old.inputs.get(key).map(|p| p.value_type) != Some(ValueType::Lighting) {
            return Err(Error(format!("{id}.{key} is not a capability bundle")));
        }
    }
    let mut conversion = Conversion::new(&source, &target, std::iter::empty());
    let call = conversion.definition(id, capabilities, false)?;
    let score = Score {
        definitions: conversion.definitions,
        ..Score::default()
    };
    score.validate(&target)?;
    Ok(NodeUpgrade {
        definitions: score.definitions,
        definition: call.id,
        inputs: call.inputs,
        outputs: call.outputs,
    })
}
struct Conversion<'a> {
    source: &'a Library,
    target: &'a Library,
    local: BTreeSet<String>,
    taken: BTreeSet<String>,
    memo: BTreeMap<(String, Shapes, bool), Converted>,
    definitions: BTreeMap<String, Definition>,
}
fn unique(taken: &mut BTreeSet<String>, preferred: &str) -> String {
    let mut candidate = preferred.to_owned();
    let mut suffix = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{preferred}/{suffix}");
        suffix += 1;
    }
    candidate
}
fn signal_type(kind: ValueType) -> ValueType {
    kind.signal_type().map(ValueType::Signal).unwrap_or(kind)
}
fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}

impl<'a> Conversion<'a> {
    fn new(source: &'a Library, target: &'a Library, local: impl Iterator<Item = String>) -> Self {
        Self {
            source,
            target,
            local: local.collect(),
            taken: source
                .definitions
                .keys()
                .chain(target.definitions.keys())
                .cloned()
                .collect(),
            memo: BTreeMap::new(),
            definitions: BTreeMap::new(),
        }
    }
    fn definition(&mut self, id: &str, shapes: Shapes, terminal: bool) -> Result<Converted> {
        let key = (id.to_owned(), shapes.clone(), terminal);
        if let Some(converted) = self.memo.get(&key) {
            return Ok(converted.clone());
        }
        let old = self
            .source
            .definitions
            .get(id)
            .ok_or_else(|| Error(format!("migration: unknown definition {id}")))?
            .clone();
        let keep_id = self.local.contains(id)
            && !self.target.definitions.contains_key(id)
            && shapes.is_empty()
            && (terminal || !old.playable());
        let new_id = if keep_id {
            id.into()
        } else {
            unique(&mut self.taken, &format!("{id}/signals"))
        };
        let mut inputs = BTreeMap::new();
        let mut input_ports = BTreeMap::new();
        let mut input_names: BTreeSet<_> = old.inputs.keys().cloned().collect();
        let mut bindings = BTreeMap::new();
        for (key, input) in &old.inputs {
            if input.value_type == ValueType::Lighting {
                let shape = shapes
                    .get(key)
                    .ok_or_else(|| Error(format!("{id}.{key}: missing capability layout")))?;
                let mut ports = BTreeMap::new();
                let mut values = BTreeMap::new();
                for cap in shape {
                    let name = unique(&mut input_names, &format!("{key}_{}", cap.key()));
                    let mut port = input.clone();
                    let label = if input.name == "Lighting" {
                        match key.as_str() {
                            "a" => "Base",
                            "b" => "Top",
                            _ => key,
                        }
                    } else {
                        &input.name
                    };
                    port.name = format!("{label} {}", cap.key());
                    port.value_type = cap.kind();
                    port.default = None;
                    inputs.insert(name.clone(), port);
                    values.insert(
                        *cap,
                        Binding::Input {
                            input: name.clone(),
                        },
                    );
                    ports.insert(*cap, name);
                }
                input_ports.insert(key.clone(), Expanded::Bundle(ports));
                bindings.insert(key.clone(), Expanded::Bundle(values));
            } else {
                let mut port = input.clone();
                port.value_type = signal_type(port.value_type);
                inputs.insert(key.clone(), port);
                input_ports.insert(key.clone(), Expanded::Single(key.clone()));
                bindings.insert(
                    key.clone(),
                    Expanded::Single(Binding::Input { input: key.clone() }),
                );
            }
        }
        let graph = match &old.body {
            Body::Graph(graph) => graph.clone(),
            Body::Primitive(_) => Graph {
                nodes: BTreeMap::from([(
                    "value".into(),
                    Node {
                        position: None,
                        definition: id.into(),
                        inputs: old
                            .inputs
                            .keys()
                            .map(|key| (key.clone(), Binding::Input { input: key.clone() }))
                            .collect(),
                    },
                )]),
                outputs: old
                    .outputs
                    .keys()
                    .map(|key| (key.clone(), wire("value", key)))
                    .collect(),
                ..Graph::default()
            },
        };
        let mut builder = Builder {
            conversion: self,
            original: &graph,
            inputs: bindings,
            values: BTreeMap::new(),
            graph: Graph::default(),
            taken: graph.nodes.keys().cloned().collect(),
        };
        for node in graph.nodes.keys() {
            builder.node(node)?;
        }
        let mut outputs = BTreeMap::new();
        let mut output_ports = BTreeMap::new();
        let mut output_names: BTreeSet<_> = old.outputs.keys().cloned().collect();
        for (key, output) in &old.outputs {
            let value = builder.binding(&graph.outputs[key])?;
            match value {
                Expanded::Single(value) => {
                    outputs.insert(
                        key.clone(),
                        Output {
                            value_type: signal_type(output.value_type),
                            rate: output.rate,
                        },
                    );
                    builder.graph.outputs.insert(key.clone(), value);
                    output_ports.insert(key.clone(), Expanded::Single(key.clone()));
                }
                Expanded::Bundle(caps) if terminal => {
                    let name = unique(&mut builder.taken, "output");
                    builder.graph.nodes.insert(
                        name.clone(),
                        Node {
                            position: None,
                            definition: "output".into(),
                            inputs: caps
                                .into_iter()
                                .map(|(cap, binding)| (cap.key().into(), binding))
                                .collect(),
                        },
                    );
                    builder
                        .graph
                        .outputs
                        .insert(key.clone(), wire(&name, "lighting"));
                    outputs.insert(key.clone(), output.clone());
                    output_ports.insert(key.clone(), Expanded::Single(key.clone()));
                }
                Expanded::Bundle(caps) => {
                    let mut ports = BTreeMap::new();
                    for (cap, binding) in caps {
                        let preferred = if key == "lighting" {
                            cap.key().into()
                        } else {
                            format!("{key}_{}", cap.key())
                        };
                        let name = unique(&mut output_names, &preferred);
                        builder.graph.outputs.insert(name.clone(), binding);
                        outputs.insert(
                            name.clone(),
                            Output {
                                value_type: cap.kind(),
                                rate: output.rate,
                            },
                        );
                        ports.insert(cap, name);
                    }
                    output_ports.insert(key.clone(), Expanded::Bundle(ports));
                }
            }
        }
        for (key, input_node) in &graph.input_nodes {
            match input_ports.get(key) {
                Some(Expanded::Bundle(ports)) => {
                    for name in ports.values() {
                        builder.graph.input_nodes.insert(
                            name.clone(),
                            InputNode {
                                name: inputs[name].name.clone(),
                                position: None,
                            },
                        );
                    }
                }
                _ => {
                    builder
                        .graph
                        .input_nodes
                        .insert(key.clone(), input_node.clone());
                }
            }
        }
        let converted = Converted {
            id: new_id.clone(),
            inputs: input_ports,
            outputs: output_ports,
        };
        let graph = builder.graph;
        self.definitions.insert(
            new_id,
            Definition {
                // Adding a terminal makes an anonymous one-node wrapper a
                // multi-node graph. Preserve its previously inherited label.
                name: if old.name.is_empty() {
                    self.source.display_name(id)
                } else {
                    old.name
                },
                inputs,
                outputs,
                body: Body::Graph(graph),
            },
        );
        self.memo.insert(key, converted.clone());
        Ok(converted)
    }
}

struct Builder<'a, 'b> {
    conversion: &'a mut Conversion<'b>,
    original: &'a Graph,
    inputs: BTreeMap<String, Operand>,
    values: BTreeMap<String, BTreeMap<String, Operand>>,
    graph: Graph,
    taken: BTreeSet<String>,
}
impl Builder<'_, '_> {
    fn binding(&mut self, binding: &Binding) -> Result<Operand> {
        match binding {
            Binding::Value { value } => Ok(Expanded::Single(value.clone().into())),
            Binding::Input { input } => self
                .inputs
                .get(input)
                .cloned()
                .ok_or_else(|| Error(format!("migration: unknown Input {input}"))),
            Binding::Connection { node, output } => {
                self.node(node)?;
                self.values[node]
                    .get(output)
                    .cloned()
                    .ok_or_else(|| Error(format!("migration: unknown output {node}.{output}")))
            }
        }
    }
    fn node(&mut self, id: &str) -> Result<()> {
        if self.values.contains_key(id) {
            return Ok(());
        }
        let node = &self.original.nodes[id];
        let definition = self.conversion.source.definitions[&node.definition].clone();
        let mut args = BTreeMap::new();
        for (key, input) in &definition.inputs {
            if let Some(binding) = node.inputs.get(key) {
                args.insert(key.clone(), self.binding(binding)?);
            } else if let Some(value) = &input.default {
                args.insert(key.clone(), Expanded::Single(value.clone().into()));
            } else {
                return Err(Error(format!(
                    "{}.{key}: missing migration input",
                    node.definition
                )));
            }
        }
        let outputs = match definition.body {
            Body::Primitive(op) => self.primitive(id, op, &args, node.position)?,
            Body::Graph(_) => {
                let shapes = args
                    .iter()
                    .filter_map(|(key, value)| match value {
                        Expanded::Bundle(caps) => {
                            Some((key.clone(), caps.keys().copied().collect()))
                        }
                        _ => None,
                    })
                    .collect();
                let converted = self
                    .conversion
                    .definition(&node.definition, shapes, false)?;
                let mut inputs = BTreeMap::new();
                for (key, ports) in converted.inputs {
                    match ports {
                        Expanded::Single(port) => {
                            inputs.insert(port, args[&key].single()?);
                        }
                        Expanded::Bundle(ports) => {
                            for (cap, port) in ports {
                                inputs.insert(port, args[&key].bundle()?[&cap].clone());
                            }
                        }
                    }
                }
                self.graph.nodes.insert(
                    id.into(),
                    Node {
                        definition: converted.id,
                        inputs,
                        position: node.position,
                    },
                );
                converted
                    .outputs
                    .into_iter()
                    .map(|(key, ports)| {
                        (
                            key,
                            match ports {
                                Expanded::Single(port) => Expanded::Single(wire(id, &port)),
                                Expanded::Bundle(ports) => Expanded::Bundle(
                                    ports
                                        .into_iter()
                                        .map(|(cap, port)| (cap, wire(id, &port)))
                                        .collect(),
                                ),
                            },
                        )
                    })
                    .collect()
            }
        };
        self.values.insert(id.into(), outputs);
        Ok(())
    }
    fn operation(
        &mut self,
        stem: &str,
        definition: &str,
        output: &str,
        inputs: &[(&str, Binding)],
    ) -> Binding {
        let id = unique(&mut self.taken, stem);
        let position = (id == stem)
            .then(|| self.original.nodes.get(stem).and_then(|n| n.position))
            .flatten();
        self.graph.nodes.insert(
            id.clone(),
            Node {
                definition: definition.into(),
                position,
                inputs: inputs
                    .iter()
                    .map(|(key, value)| ((*key).into(), value.clone()))
                    .collect(),
            },
        );
        wire(&id, output)
    }
    fn math(&mut self, stem: &str, op: &str, a: Binding, b: Binding) -> Binding {
        self.operation(stem, &format!("core/{op}"), "value", &[("a", a), ("b", b)])
    }
    fn clamp(&mut self, stem: &str, value: Binding) -> Binding {
        let lower = self.math(stem, "maximum", value, Value::Number(0.0).into());
        self.math(stem, "minimum", lower, Value::Number(1.0).into())
    }
    fn primitive(
        &mut self,
        id: &str,
        op: Primitive,
        args: &BTreeMap<String, Operand>,
        position: Option<[f64; 2]>,
    ) -> Result<BTreeMap<String, Operand>> {
        use Capability::*;
        let arg = |key: &str| args[key].single();
        if matches!(
            op,
            Primitive::WriteMask
                | Primitive::WriteStrobeMask
                | Primitive::WriteSpeed
                | Primitive::WritePosition
                | Primitive::WriteColor
                | Primitive::AddLighting
        ) {
            self.taken.remove(id);
        }
        let bundle = match op {
            Primitive::TravelClock => {
                self.taken.remove(id);
                // This helper receives an explicit elapsed signal, not events.
                // Normalize it directly; periodic/event timing belongs upstream.
                let elapsed = self.math(id, "divide", arg("elapsed")?, arg("travel")?);
                let progress = self.clamp(id, elapsed.clone());
                let active = self.operation(
                    id,
                    "core/greater",
                    "mask",
                    &[
                        ("a", Value::Number(1.0).into()),
                        ("b", elapsed),
                        ("tolerance", Value::Number(0.0).into()),
                    ],
                );
                return Ok(BTreeMap::from([
                    ("progress".into(), Expanded::Single(progress)),
                    ("active".into(), Expanded::Single(active)),
                ]));
            }
            Primitive::Broadcast(ScalarKind::Beats) => {
                // v2's beats-to-field adapter also erased its unit. Express
                // that conversion as a ratio rather than keeping an adapter.
                self.taken.remove(id);
                let value = self.math(id, "divide", arg("value")?, Value::Beats(1.0).into());
                return Ok(BTreeMap::from([("value".into(), Expanded::Single(value))]));
            }
            Primitive::ScalarConvert { from, to } => {
                self.taken.remove(id);
                let value = if from == ScalarKind::Beats {
                    self.math(id, "divide", arg("value")?, Value::Beats(1.0).into())
                } else if to == ScalarKind::Beats {
                    self.math(id, "multiply", arg("value")?, Value::Beats(1.0).into())
                } else if to == ScalarKind::Proportion {
                    self.clamp(id, arg("value")?)
                } else {
                    arg("value")?
                };
                return Ok(BTreeMap::from([("value".into(), Expanded::Single(value))]));
            }
            Primitive::WriteMask | Primitive::WriteStrobeMask | Primitive::WriteSpeed => {
                let (cap, key) = match op {
                    Primitive::WriteMask => (Dimmer, "mask"),
                    Primitive::WriteStrobeMask => (Strobe, "mask"),
                    _ => (Speed, "value"),
                };
                BTreeMap::from([(cap, self.clamp(id, arg(key)?))])
            }
            Primitive::WritePosition => [Pan, Tilt]
                .into_iter()
                .map(|cap| {
                    Ok((
                        cap,
                        self.math(id, "multiply", arg(cap.key())?, Value::Degrees(1.0).into()),
                    ))
                })
                .collect::<Result<_>>()?,
            Primitive::WriteColor => {
                let color = arg("color")?;
                let brightness = self.operation(
                    id,
                    "core/channel_maximum",
                    "value",
                    &[("value", color.clone())],
                );
                let positive = self.operation(
                    id,
                    "core/greater",
                    "mask",
                    &[
                        ("a", brightness.clone()),
                        ("b", Value::Number(1e-5).into()),
                        ("tolerance", Value::Number(0.0).into()),
                    ],
                );
                let normalized = self.math(id, "divide", color, brightness.clone());
                let normalized = self.operation(
                    id,
                    "core/choose",
                    "value",
                    &[
                        ("condition", positive),
                        ("yes", normalized),
                        ("no", Value::Color([0.0; 3]).into()),
                    ],
                );
                BTreeMap::from([(Color, normalized), (Dimmer, brightness)])
            }
            Primitive::AddLighting => {
                let mut base = args["a"].bundle()?.clone();
                let top = args["b"].bundle()?;
                let opacity = self.clamp(
                    id,
                    top.get(&Dimmer)
                        .cloned()
                        .unwrap_or_else(|| Dimmer.fallback()),
                );
                for (cap, upper) in top {
                    let lower = base.get(cap).cloned().unwrap_or_else(|| cap.fallback());
                    let value = match cap {
                        Color => {
                            let sum = self.math(id, "add", lower.clone(), upper.clone());
                            let sum = self.math(id, "minimum", sum, Value::Number(1.0).into());
                            let weighted = self.math(id, "multiply", sum, opacity.clone());
                            let inverse = self.math(
                                id,
                                "subtract",
                                Value::Number(1.0).into(),
                                opacity.clone(),
                            );
                            let remaining = self.math(id, "multiply", lower, inverse);
                            self.math(id, "add", weighted, remaining)
                        }
                        Dimmer | Strobe => {
                            let sum = self.math(
                                id,
                                "add",
                                lower,
                                if *cap == Dimmer {
                                    opacity.clone()
                                } else {
                                    upper.clone()
                                },
                            );
                            self.clamp(id, sum)
                        }
                        Speed => self.operation(
                            id,
                            "core/greater",
                            "mask",
                            &[
                                ("a", upper.clone()),
                                ("b", Value::Number(0.5).into()),
                                ("tolerance", Value::Number(0.0).into()),
                            ],
                        ),
                        Pan | Tilt => upper.clone(),
                    };
                    base.insert(*cap, value);
                }
                base
            }
            Primitive::Broadcast(_) | Primitive::ColorField | Primitive::MaskToField => {
                let (key, output) = match op {
                    Primitive::ColorField => ("color", "color"),
                    Primitive::MaskToField => ("mask", "value"),
                    _ => ("value", "value"),
                };
                return Ok(BTreeMap::from([(
                    output.into(),
                    Expanded::Single(arg(key)?),
                )]));
            }
            _ => {
                let op = if let Primitive::ScalarBinary(math) = op {
                    Primitive::FieldBinary(math)
                } else {
                    op
                };
                let (target, definition) = self
                    .conversion
                    .target
                    .definitions
                    .iter()
                    .find(|(_, definition)| definition.body == Body::Primitive(op))
                    .ok_or_else(|| Error(format!("migration has no signal kernel for {op:?}")))?;
                self.graph.nodes.insert(
                    id.into(),
                    Node {
                        definition: target.clone(),
                        position,
                        inputs: args
                            .iter()
                            .map(|(key, value)| Ok((key.clone(), value.single()?)))
                            .collect::<Result<_>>()?,
                    },
                );
                return Ok(definition
                    .outputs
                    .keys()
                    .map(|key| (key.clone(), Expanded::Single(wire(id, key))))
                    .collect());
            }
        };
        Ok(BTreeMap::from([(
            "lighting".into(),
            Expanded::Bundle(bundle),
        )]))
    }
}
