//! Stateless signal sources and arithmetic. Musical clocks never accumulate
//! frame deltas, and coherent noise is a pure function of its coordinates.
use crate::*;

pub(crate) fn port(name: &str, kind: ValueType, default: Option<Value>) -> Input {
    Input {
        optional: false,
        name: name.into(),
        description: name.into(),
        value_type: kind,
        rate: Rate::Frame,
        default,
        author: None,
        promotable: Vec::new(),
    }
}
pub(crate) fn definition(op: Primitive) -> Option<Definition> {
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
        Primitive::Fraction => (
            "Fraction (wrap)",
            vec![(
                "value",
                port("Value", ValueType::Signal(SignalType::ANY), None),
            )],
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
