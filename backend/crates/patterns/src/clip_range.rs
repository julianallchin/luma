//! A sampled range over the placed clip, prepared once rather than recomputed
//! from whichever frames playback happens to request.
use crate::*;
use std::collections::BTreeMap;

pub(crate) fn definition() -> Definition {
    let scalar = ValueType::Signal(SignalType {
        unit: None,
        channels: Some(Channels::Value),
    });
    Definition {
        name: "Clip range".into(),
        inputs: BTreeMap::from([
            (
                "value".into(),
                crate::signals::port("Signal", ValueType::Signal(SignalType::ANY), None),
            ),
            (
                "samples".into(),
                Input {
                    name: "Samples".into(),
                    description:
                        "Evenly spaced musical-time samples across the clip, including both ends"
                            .into(),
                    value_type: ValueType::Number,
                    rate: Rate::Fixed,
                    optional: false,
                    default: Some(Value::Number(1024.)),
                },
            ),
        ]),
        outputs: ["minimum", "maximum"]
            .into_iter()
            .map(|key| {
                (
                    key.into(),
                    Output {
                        value_type: scalar,
                        rate: Rate::Fixed,
                    },
                )
            })
            .collect(),
        body: Body::Primitive(Primitive::ClipRange),
    }
}

pub(crate) fn sample_count(value: f64) -> Result<usize> {
    if value.fract() != 0. || !(2. ..=16384.).contains(&value) {
        return Err(Error(
            "range sampling needs an integer sample count in 2–16,384".into(),
        ));
    }
    Ok(value as usize)
}

pub(crate) fn bounds(signal: &Signal) -> (f64, f64) {
    signal
        .values()
        .iter()
        .copied()
        .map(|v| (v, v))
        .reduce(|(lo, hi), (v, _)| (lo.min(v), hi.max(v)))
        .unwrap_or((0., 0.))
}

pub(crate) fn result(
    unit: Unit,
    minimum: f64,
    maximum: f64,
) -> Result<BTreeMap<String, EvaluatedValue>> {
    [("minimum", minimum), ("maximum", maximum)]
        .into_iter()
        .map(|(key, value)| {
            Ok((
                key.into(),
                EvaluatedValue::from_signal(
                    ValueType::Signal(SignalType::new(unit, Channels::Value)),
                    Signal::scalar(value, unit)?,
                )?,
            ))
        })
        .collect()
}
