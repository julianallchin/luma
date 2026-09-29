//! One source of wire compatibility for validation, native gestures and Python.
//! Numerical metadata flows through generic operators; tensor extents are runtime
//! axes and never invent separate scalar-versus-field connection types.
use crate::*;
use std::collections::BTreeMap;

impl Library {
    pub fn binding_type(
        &self,
        inputs: &BTreeMap<String, Input>,
        graph: &Graph,
        binding: &Binding,
    ) -> Result<(ValueType, Rate)> {
        self.infer_binding(inputs, graph, binding, 0, &mut BTreeMap::new())
    }
    fn infer_binding(
        &self,
        inputs: &BTreeMap<String, Input>,
        graph: &Graph,
        binding: &Binding,
        depth: usize,
        cache: &mut BTreeMap<(String, String), (ValueType, Rate)>,
    ) -> Result<(ValueType, Rate)> {
        if depth > crate::graph::MAX_EXECUTION_DEPTH {
            return Err(Error(
                "wire cycle or excessive signal inference depth".into(),
            ));
        }
        match binding {
            Binding::Value { value } => {
                value.validate()?;
                Ok((value.value_type(), Rate::Fixed))
            }
            Binding::Input { input } => inputs
                .get(input)
                .map(|i| (i.value_type, i.rate))
                .ok_or_else(|| Error(format!("unknown Input {input}"))),
            Binding::Connection { node, output } => {
                let key = (node.clone(), output.clone());
                if let Some(result) = cache.get(&key) {
                    return Ok(*result);
                }
                let node = graph
                    .nodes
                    .get(node)
                    .ok_or_else(|| Error(format!("unknown node {node}")))?;
                let definition = self
                    .definitions
                    .get(&node.definition)
                    .ok_or_else(|| Error(format!("unknown definition {}", node.definition)))?;
                let port = definition.outputs.get(output).ok_or_else(|| {
                    Error(format!("{}: unknown output {output}", node.definition))
                })?;
                let bound = definition
                    .inputs
                    .iter()
                    .map(|(key, spec)| {
                        let mut spec = spec.clone();
                        let (actual, rate) = if let Some(binding) = node.inputs.get(key) {
                            self.infer_binding(inputs, graph, binding, depth + 1, cache)?
                        } else {
                            (
                                spec.default
                                    .as_ref()
                                    .map(Value::value_type)
                                    .unwrap_or(spec.value_type),
                                if spec.default.is_some() {
                                    Rate::Fixed
                                } else {
                                    spec.rate
                                },
                            )
                        };
                        if !spec.value_type.accepts(actual) {
                            return Err(Error(format!(
                                "{}.{key}: expected {}, got {actual}",
                                node.definition, spec.value_type
                            )));
                        }
                        spec.value_type = actual;
                        spec.rate = rate;
                        Ok((key.clone(), spec))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                let (value_type, rate) = match &definition.body {
                    Body::Form(_) => (port.value_type, port.rate),
                    Body::Graph(graph) => self.infer_binding(
                        &bound,
                        graph,
                        graph
                            .outputs
                            .get(output)
                            .ok_or_else(|| Error(format!("missing output {output}")))?,
                        depth + 1,
                        &mut BTreeMap::new(),
                    )?,
                    Body::Primitive(op) => {
                        let signal = |key: &str| {
                            bound[key]
                                .value_type
                                .signal_type()
                                .expect("numerical kernel signature")
                        };
                        let value_type = if matches!(port.value_type, ValueType::Signal(_)) {
                            ValueType::Signal(match op {
                                Primitive::Envelope => SignalType {
                                    unit: Some(Unit::Proportion),
                                    channels: signal("progress").channels,
                                },
                                Primitive::JoinChannels => signal("a").join(signal("b"))?,
                                Primitive::FieldBinary(math) => {
                                    signal("a").binary(signal("b"), *math)?
                                }
                                Primitive::Fraction => SignalType {
                                    unit: Some(Unit::Number),
                                    channels: signal("value").channels,
                                },
                                Primitive::ClipRange | Primitive::ChannelMaximum => SignalType {
                                    unit: signal("value").unit,
                                    channels: Some(Channels::Value),
                                },
                                Primitive::FieldClamp => SignalType {
                                    unit: Some(Unit::Proportion),
                                    channels: signal("value").channels,
                                },
                                Primitive::FieldGreater => SignalType {
                                    unit: Some(Unit::Proportion),
                                    channels: signal("a")
                                        .binary(signal("b"), FieldMath::Subtract)?
                                        .channels,
                                },
                                _ => port.value_type.signal_type().unwrap(),
                            })
                        } else {
                            port.value_type
                        };
                        let rate = if port.rate == Rate::Fixed {
                            Rate::Fixed
                        } else if op.reads_time()
                            || bound.values().any(|input| input.rate == Rate::Frame)
                        {
                            Rate::Frame
                        } else {
                            Rate::Fixed
                        };
                        (value_type, rate)
                    }
                };
                if !port.value_type.accepts(value_type) {
                    return Err(Error(format!(
                        "{}.{output}: incompatible signal result",
                        node.definition
                    )));
                }
                let result = (value_type, rate);
                cache.insert(key, result);
                Ok(result)
            }
        }
    }
}
