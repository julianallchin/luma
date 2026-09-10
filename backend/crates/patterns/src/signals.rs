//! Stateless signal sources and arithmetic. Musical clocks never accumulate
//! frame deltas, and coherent noise is a pure function of its coordinates.
use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryMath {
    Absolute,
    Floor,
    Float32,
    Fraction,
    Sine,
    SquareRoot,
}

pub(crate) fn port(name: &str, kind: ValueType, default: Option<Value>) -> Input {
    Input {
        optional: false,
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
                UnaryMath::Float32 => "32-bit precision",
                UnaryMath::Fraction => "Fraction (wrap)",
                UnaryMath::Sine => "Sine (turns)",
                UnaryMath::SquareRoot => "Square root",
            },
            vec![(
                "value",
                port("Value", ValueType::Signal(SignalType::ANY), None),
            )],
            vec![("value", ValueType::Signal(SignalType::ANY))],
        ),
        Primitive::ValueNoise1d | Primitive::ValueNoise3d => (
            if matches!(op, Primitive::ValueNoise1d) {
                "Value noise (1D)"
            } else {
                "Value noise (3D)"
            },
            if matches!(op, Primitive::ValueNoise1d) {
                vec![
                    ("seed", port("Seed", ValueType::Seed, Some(Value::Seed(0)))),
                    (
                        "position",
                        port(
                            "Position",
                            ValueType::Signal(SignalType {
                                unit: Some(Unit::Number),
                                channels: None,
                            }),
                            Some(Value::Number(0.)),
                        ),
                    ),
                    (
                        "octaves",
                        port("Octaves", ValueType::Number, Some(Value::Number(1.))),
                    ),
                ]
            } else {
                vec![
                    ("seed", port("Seed", ValueType::Seed, Some(Value::Seed(0)))),
                    (
                        "x",
                        port(
                            "X",
                            ValueType::Signal(SignalType {
                                unit: Some(Unit::Number),
                                channels: None,
                            }),
                            Some(Value::Number(0.)),
                        ),
                    ),
                    (
                        "y",
                        port(
                            "Y",
                            ValueType::Signal(SignalType {
                                unit: Some(Unit::Number),
                                channels: None,
                            }),
                            Some(Value::Number(0.)),
                        ),
                    ),
                    (
                        "z",
                        port(
                            "Z",
                            ValueType::Signal(SignalType {
                                unit: Some(Unit::Number),
                                channels: None,
                            }),
                            Some(Value::Number(0.)),
                        ),
                    ),
                    (
                        "octaves",
                        port("Octaves", ValueType::Number, Some(Value::Number(1.))),
                    ),
                ]
            },
            vec![(
                "value",
                ValueType::Signal(SignalType {
                    unit: Some(Unit::Number),
                    channels: None,
                }),
            )],
        ),
        Primitive::SeedStream => (
            "Seed stream",
            vec![
                ("seed", port("Seed", ValueType::Seed, Some(Value::Seed(0)))),
                (
                    "stream",
                    port("Stream", ValueType::Seed, Some(Value::Seed(0))),
                ),
            ],
            vec![("seed", ValueType::Seed)],
        ),
        Primitive::Power => (
            "Power",
            vec![
                (
                    "base",
                    port("Base", ValueType::Signal(SignalType::ANY), None),
                ),
                (
                    "exponent",
                    port(
                        "Exponent",
                        ValueType::Signal(SignalType::ANY),
                        Some(Value::Number(2.)),
                    ),
                ),
            ],
            vec![("value", ValueType::Signal(SignalType::ANY))],
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
        Primitive::WriteMask | Primitive::WriteStrobeMask => (
            if op == Primitive::WriteMask {
                "Dimmer mask output"
            } else {
                "Strobe mask output"
            },
            vec![("mask", port("Mask", ValueType::Mask, None))],
            vec![("lighting", ValueType::Lighting)],
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs
            .into_iter()
            .map(|(k, mut v)| {
                if v.value_type == ValueType::Seed {
                    v.rate = Rate::Fixed;
                }
                (k.into(), v)
            })
            .collect(),
        outputs: outputs
            .into_iter()
            .map(|(k, value_type)| {
                (
                    k.into(),
                    Output {
                        value_type,
                        rate: if op == Primitive::SeedStream {
                            Rate::Fixed
                        } else {
                            Rate::Frame
                        },
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

pub(crate) fn coherent_noise(point: [f64; 3], seed: u64) -> Result<f64> {
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
