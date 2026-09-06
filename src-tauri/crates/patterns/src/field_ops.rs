//! Fundamental numeric field operations. Field keys are stable head identities;
//! scalar-to-field broadcasting is explicit at a graph's unit boundary.
use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldMath {
    Add,
    Subtract,
    Multiply,
    Divide,
    Minimum,
    Maximum,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
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
        name: name.into(),
        description: name.into(),
        value_type: kind,
        rate: Rate::Frame,
        default: None,
    };
    let (name, inputs, output, kind) = match op {
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
                ("a", port("A", ValueType::Field)),
                ("b", port("B", ValueType::Field)),
            ],
            "value",
            ValueType::Field,
        ),
        Primitive::Broadcast(kind) => (
            "Broadcast",
            vec![("value", port("Value", kind.value_type()))],
            "value",
            ValueType::Field,
        ),
        Primitive::FieldClamp => (
            "Clamp coverage",
            vec![("value", port("Value", ValueType::Field))],
            "mask",
            ValueType::Mask,
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
                ("a", port("A", ValueType::Field)),
                ("b", port("B", ValueType::Field)),
                (
                    "tolerance",
                    Input {
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
            ValueType::Mask,
        ),
        Primitive::FieldSelect => (
            "Choose",
            vec![
                ("condition", port("Condition", ValueType::Mask)),
                ("yes", port("Yes", ValueType::Field)),
                ("no", port("No", ValueType::Field)),
            ],
            "value",
            ValueType::Field,
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

pub(crate) fn run(
    op: Primitive,
    i: &BTreeMap<String, Value>,
    frame: Frame,
) -> Option<Result<BTreeMap<String, Value>>> {
    if !matches!(
        op,
        Primitive::FieldBinary(_)
            | Primitive::Broadcast(_)
            | Primitive::FieldClamp
            | Primitive::MaskToField
            | Primitive::FieldGreater
            | Primitive::FieldSelect
            | Primitive::RandomField
            | Primitive::ChooseNumber
    ) {
        return None;
    }
    Some((|| {
        let field = |key: &str| match &i[key] {
            Value::Field(f) | Value::Mask(f) => f,
            _ => unreachable!("validated field input"),
        };
        let out = |key: &str, value: Value| Ok(BTreeMap::from([(key.into(), value)]));
        let zip = |a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>| {
            if a.keys().eq(b.keys()) {
                Ok(())
            } else {
                Err(Error("numeric field head domains differ".into()))
            }
        };
        match op {
            Primitive::Broadcast(_) => out(
                "value",
                Value::Field(
                    frame
                        .cells
                        .iter()
                        .map(|c| (c.id.clone(), i["value"].scalar()))
                        .collect(),
                ),
            ),
            Primitive::MaskToField => out("value", Value::Field(field("mask").clone())),
            Primitive::FieldBinary(math) => {
                let a = field("a");
                let b = field("b");
                zip(a, b)?;
                let result = a
                    .iter()
                    .map(|(id, a)| {
                        let b = b[id];
                        (
                            id.clone(),
                            match math {
                                FieldMath::Add => a + b,
                                FieldMath::Subtract => a - b,
                                FieldMath::Multiply => a * b,
                                FieldMath::Divide => {
                                    if b == 0.0 {
                                        0.0
                                    } else {
                                        a / b
                                    }
                                }
                                FieldMath::Minimum => a.min(b),
                                FieldMath::Maximum => a.max(b),
                            },
                        )
                    })
                    .collect();
                out("value", Value::Field(result))
            }
            Primitive::FieldClamp => out(
                "mask",
                Value::Mask(
                    field("value")
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clamp(0.0, 1.0)))
                        .collect(),
                ),
            ),
            Primitive::FieldGreater => {
                let a = field("a");
                let b = field("b");
                zip(a, b)?;
                out(
                    "mask",
                    Value::Mask(
                        a.iter()
                            .map(|(id, a)| {
                                (
                                    id.clone(),
                                    if *a - b[id] > i["tolerance"].scalar().max(0.0) {
                                        1.0
                                    } else {
                                        0.0
                                    },
                                )
                            })
                            .collect(),
                    ),
                )
            }
            Primitive::FieldSelect => {
                let c = field("condition");
                let yes = field("yes");
                let no = field("no");
                zip(c, yes)?;
                zip(c, no)?;
                out(
                    "value",
                    Value::Field(
                        c.iter()
                            .map(|(id, c)| (id.clone(), if *c > 0.0 { yes[id] } else { no[id] }))
                            .collect(),
                    ),
                )
            }
            Primitive::ChooseNumber => out(
                "value",
                i[if i["condition"] == Value::Boolean(true) {
                    "yes"
                } else {
                    "no"
                }]
                .clone(),
            ),
            Primitive::RandomField => {
                let epoch = i["epoch"].scalar();
                if epoch.fract() != 0.0 || epoch.abs() > 9_007_199_254_740_991.0 {
                    return Err(Error(
                        "random epoch must be an exactly representable integer".into(),
                    ));
                }
                let seed = crate::spatial::epoch_seed(frame.seed, epoch as i64);
                out(
                    "value",
                    Value::Field(
                        frame
                            .cells
                            .iter()
                            .map(|c| (c.id.clone(), crate::spatial::threshold(&c.id, seed)))
                            .collect(),
                    ),
                )
            }
            _ => unreachable!(),
        }
    })())
}
