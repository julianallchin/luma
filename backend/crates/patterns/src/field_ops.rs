//! Authoring signatures for numerical operations. Compiled execution shares
//! the tensor kernels regardless of a document's scalar/field port spelling.
use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldMath {
    Add,
    Subtract,
    Multiply,
    Divide,
    Minimum,
    Maximum,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarKind {
    Number,
    Beats,
    Proportion,
    Position,
}
impl ScalarKind {
    pub fn value_type(self) -> ValueType {
        match self {
            Self::Number => ValueType::Number,
            Self::Beats => ValueType::Beats,
            Self::Proportion => ValueType::Proportion,
            Self::Position => ValueType::Position,
        }
    }
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let port = |name: &str, kind: ValueType| Input {
        optional: false,
        name: name.into(),
        description: name.into(),
        value_type: kind,
        rate: Rate::Frame,
        default: None,
    };
    let (name, inputs, output, kind) = match op {
        Primitive::JoinChannels => (
            "Join channels",
            vec![
                ("a", port("A", ValueType::Signal(SignalType::ANY))),
                ("b", port("B", ValueType::Signal(SignalType::ANY))),
            ],
            "value",
            ValueType::Signal(SignalType::ANY),
        ),
        Primitive::Channel => (
            "Channel",
            vec![
                ("value", port("Signal", ValueType::Signal(SignalType::ANY))),
                (
                    "index",
                    Input {
                        optional: false,
                        name: "Index".into(),
                        description: "Channel index, starting at zero".into(),
                        value_type: ValueType::Number,
                        rate: Rate::Fixed,
                        default: Some(Value::Number(0.)),
                    },
                ),
            ],
            "value",
            ValueType::Signal(SignalType {
                unit: None,
                channels: Some(Channels::Value),
            }),
        ),
        Primitive::ChannelArgmax => (
            "Strongest channel",
            vec![("value", port("Signal", ValueType::Signal(SignalType::ANY)))],
            "value",
            ValueType::Number,
        ),
        Primitive::ChannelMaximum | Primitive::ChannelSum => (
            if op == Primitive::ChannelSum {
                "Sum channels"
            } else {
                "Maximum channel"
            },
            vec![("value", port("Value", ValueType::Signal(SignalType::ANY)))],
            "value",
            ValueType::Signal(SignalType {
                unit: None,
                channels: Some(Channels::Value),
            }),
        ),
        Primitive::FieldBinary(math) => (
            match math {
                FieldMath::Add => "Add",
                FieldMath::Subtract => "Subtract",
                FieldMath::Multiply => "Multiply",
                FieldMath::Divide => "Divide",
                FieldMath::Minimum => "Minimum",
                FieldMath::Maximum => "Maximum",
            },
            vec![
                ("a", port("A", ValueType::Signal(SignalType::ANY))),
                ("b", port("B", ValueType::Signal(SignalType::ANY))),
            ],
            "value",
            ValueType::Signal(SignalType::ANY),
        ),
        Primitive::Broadcast(kind) => (
            "Broadcast",
            vec![("value", port("Value", kind.value_type()))],
            "value",
            ValueType::Field,
        ),
        Primitive::FieldClamp => (
            "Clamp coverage",
            vec![("value", port("Value", ValueType::Signal(SignalType::ANY)))],
            "mask",
            ValueType::Signal(SignalType {
                unit: Some(Unit::Proportion),
                channels: None,
            }),
        ),
        Primitive::MaskToField => (
            "Coverage values",
            vec![("mask", port("Mask", ValueType::Mask))],
            "value",
            ValueType::Field,
        ),
        Primitive::FieldGreater => (
            "Greater than",
            vec![
                ("a", port("A", ValueType::Signal(SignalType::ANY))),
                ("b", port("B", ValueType::Signal(SignalType::ANY))),
                (
                    "tolerance",
                    Input {
                        optional: false,
                        name: "Tolerance".into(),
                        description: "Ignore differences at or below this absolute tolerance"
                            .into(),
                        value_type: ValueType::Number,
                        rate: Rate::Frame,
                        default: Some(Value::Number(0.0)),
                    },
                ),
            ],
            "mask",
            ValueType::Signal(SignalType {
                unit: Some(Unit::Proportion),
                channels: None,
            }),
        ),
        Primitive::FieldSelect => (
            "Choose",
            vec![
                (
                    "condition",
                    port(
                        "Condition",
                        ValueType::Signal(SignalType {
                            unit: Some(Unit::Proportion),
                            channels: None,
                        }),
                    ),
                ),
                ("yes", port("Yes", ValueType::Signal(SignalType::ANY))),
                ("no", port("No", ValueType::Signal(SignalType::ANY))),
            ],
            "value",
            ValueType::Signal(SignalType::ANY),
        ),
        Primitive::ChooseNumber => (
            "Choose number",
            vec![
                ("condition", port("Condition", ValueType::Boolean)),
                ("yes", port("Yes", ValueType::Number)),
                ("no", port("No", ValueType::Number)),
            ],
            "value",
            ValueType::Number,
        ),
        Primitive::RandomField => (
            "Random per head",
            vec![(
                "epoch",
                Input {
                    optional: false,
                    name: "Epoch".into(),
                    description: "Changing this index chooses a new deterministic random field"
                        .into(),
                    value_type: ValueType::Number,
                    rate: Rate::Frame,
                    default: Some(Value::Number(0.0)),
                },
            )],
            "value",
            ValueType::Field,
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: BTreeMap::from([(
            output.into(),
            Output {
                value_type: kind,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(op),
    })
}
