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

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let port = |name: &str, kind: ValueType| Input {
        optional: false,
        name: name.into(),
        description: name.into(),
        value_type: kind,
        rate: Rate::Frame,
        default: None,
        author: None,
        promotable: Vec::new(),
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
        Primitive::ChannelMaximum => (
            "Maximum channel",
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
        Primitive::FieldClamp => (
            "Clamp coverage",
            vec![("value", port("Value", ValueType::Signal(SignalType::ANY)))],
            "mask",
            ValueType::Signal(SignalType {
                unit: Some(Unit::Proportion),
                channels: None,
            }),
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
                        author: None,
                        promotable: Vec::new(),
                    },
                ),
            ],
            "mask",
            ValueType::Signal(SignalType {
                unit: Some(Unit::Proportion),
                channels: None,
            }),
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
