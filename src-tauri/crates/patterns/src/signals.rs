//! Stateless signal sources and arithmetic. Musical clocks never accumulate
//! frame deltas, and coherent noise is a pure function of its coordinates.
use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryMath {
    Absolute,
    Floor,
    Fraction,
    Sine,
}

pub(crate) fn port(name: &str, kind: ValueType, default: Option<Value>) -> Input {
    Input {
        name: name.into(),
        description: name.into(),
        value_type: kind,
        rate: Rate::Frame,
        default,
    }
}
pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let number = |name| port(name, ValueType::Number, Some(Value::Number(0.0)));
    let field = |name| port(name, ValueType::Field, None);
    let (name, inputs, outputs) = match op {
        Primitive::ClipTime => (
            "Clip time",
            vec![],
            vec![
                ("elapsed", ValueType::Beats),
                ("progress", ValueType::Proportion),
                ("duration", ValueType::Beats),
                ("beat", ValueType::Number),
            ],
        ),
        Primitive::ScalarBinary(math) => (
            match math {
                FieldMath::Add => "Add numbers",
                FieldMath::Subtract => "Subtract numbers",
                FieldMath::Multiply => "Multiply numbers",
                FieldMath::Divide => "Divide numbers",
                FieldMath::Minimum => "Minimum number",
                FieldMath::Maximum => "Maximum number",
            },
            vec![("a", number("A")), ("b", number("B"))],
            vec![("value", ValueType::Number)],
        ),
        Primitive::ScalarConvert { from, to } => (
            "Convert units",
            vec![("value", port("Value", from.value_type(), None))],
            vec![("value", to.value_type())],
        ),
        Primitive::FieldUnary(math) => (
            match math {
                UnaryMath::Absolute => "Absolute value",
                UnaryMath::Floor => "Floor",
                UnaryMath::Fraction => "Fraction (wrap)",
                UnaryMath::Sine => "Sine (turns)",
            },
            vec![("value", field("Value"))],
            vec![("value", ValueType::Field)],
        ),
        Primitive::Noise => (
            "Coherent noise",
            vec![
                ("x", field("X")),
                ("y", field("Y")),
                ("z", field("Z / time")),
            ],
            vec![("value", ValueType::Field)],
        ),
        Primitive::WriteMask => (
            "Dimmer mask output",
            vec![("mask", port("Mask", ValueType::Mask, None))],
            vec![("lighting", ValueType::Lighting)],
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: outputs
            .into_iter()
            .map(|(k, value_type)| {
                (
                    k.into(),
                    Output {
                        value_type,
                        rate: Rate::Frame,
                    },
                )
            })
            .collect(),
        body: Body::Primitive(op),
    })
}

impl FieldMath {
    pub(crate) fn evaluate(self, a: f64, b: f64) -> f64 {
        match self {
            Self::Add => a + b,
            Self::Subtract => a - b,
            Self::Multiply => a * b,
            // Defined zero denominator, shared with field arithmetic. Explicit
            // graph clamps still control the useful range of ratios.
            Self::Divide => {
                if b == 0.0 {
                    0.0
                } else {
                    a / b
                }
            }
            Self::Minimum => a.min(b),
            Self::Maximum => a.max(b),
        }
    }
}

pub(crate) fn run(
    op: Primitive,
    i: &BTreeMap<String, Value>,
    frame: Frame,
) -> Option<Result<BTreeMap<String, Value>>> {
    let field = |name: &str| match &i[name] {
        Value::Field(v) | Value::Mask(v) => v,
        _ => unreachable!("validated numeric field"),
    };
    let value = match op {
        Primitive::ClipTime => {
            return Some(Ok(BTreeMap::from([
                (
                    "elapsed".into(),
                    Value::Beats((frame.beat - frame.clip_start).max(0.0)),
                ),
                (
                    "progress".into(),
                    Value::Proportion(
                        ((frame.beat - frame.clip_start) / frame.clip_duration).clamp(0.0, 1.0),
                    ),
                ),
                ("duration".into(), Value::Beats(frame.clip_duration)),
                ("beat".into(), Value::Number(frame.beat)),
            ])))
        }
        Primitive::ScalarBinary(math) => {
            Value::Number(math.evaluate(i["a"].scalar(), i["b"].scalar()))
        }
        Primitive::ScalarConvert { to, .. } => {
            let value = i["value"].scalar();
            match to {
                ScalarKind::Number => Value::Number(value),
                ScalarKind::Beats => Value::Beats(value),
                ScalarKind::Position => Value::Position(value),
                ScalarKind::Proportion => Value::Proportion(value.clamp(0.0, 1.0)),
            }
        }
        Primitive::FieldUnary(math) => Value::Field(
            field("value")
                .iter()
                .map(|(id, v)| {
                    (
                        id.clone(),
                        match math {
                            UnaryMath::Absolute => v.abs(),
                            UnaryMath::Floor => v.floor(),
                            UnaryMath::Fraction => v.rem_euclid(1.0),
                            UnaryMath::Sine => (v * std::f64::consts::TAU).sin(),
                        },
                    )
                })
                .collect(),
        ),
        Primitive::Noise => {
            let x = field("x");
            let y = field("y");
            let z = field("z");
            if !x.keys().eq(y.keys()) || !x.keys().eq(z.keys()) {
                return Some(Err(Error("noise coordinate head domains differ".into())));
            }
            let values = x
                .iter()
                .map(|(id, x)| {
                    coherent_noise([*x, y[id], z[id]], frame.seed).map(|v| (id.clone(), v))
                })
                .collect::<Result<_>>();
            match values {
                Ok(v) => Value::Field(v),
                Err(e) => return Some(Err(e)),
            }
        }
        Primitive::WriteMask => {
            return Some(Ok(BTreeMap::from([(
                "lighting".into(),
                Value::Lighting(
                    field("mask")
                        .iter()
                        .map(|(id, value)| {
                            (
                                id.clone(),
                                FixtureOutput {
                                    dimmer: Some(*value),
                                    ..Default::default()
                                },
                            )
                        })
                        .collect(),
                ),
            )])))
        }
        _ => return None,
    };
    Some(
        value
            .validate()
            .map(|_| BTreeMap::from([("value".into(), value)])),
    )
}

fn coherent_noise(point: [f64; 3], seed: u64) -> Result<f64> {
    if point.iter().any(|v| !v.is_finite() || v.abs() > 1e12) {
        return Err(Error(
            "noise coordinates must be finite and within ±1e12".into(),
        ));
    }
    let base = point.map(|v| v.floor() as i64);
    let fraction = point.map(|v| {
        let t = v - v.floor();
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    });
    let mut sum = 0.0;
    for corner in 0..8 {
        let mut hash = seed;
        let mut weight = 1.0;
        for axis in 0..3 {
            let upper = ((corner >> axis) & 1) as i64;
            hash = crate::spatial::epoch_seed(hash, base[axis] + upper);
            weight *= if upper == 0 {
                1.0 - fraction[axis]
            } else {
                fraction[axis]
            };
        }
        sum += weight * ((hash >> 11) as f64 / (1u64 << 53) as f64);
    }
    Ok(sum.clamp(0.0, 1.0))
}
