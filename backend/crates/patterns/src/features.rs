//! The core asks for analyzed track data through a read-only, seekable source.
//! A missing analysis is an error, not a silent replacement with another stem.
use crate::*;
use std::collections::BTreeMap;

/// The energy of one frequency band of the track's full mix.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureRequest {
    pub low_hz: f64,
    pub high_hz: f64,
}
pub trait FeatureSource: std::fmt::Debug + Send + Sync {
    /// The band's energy at `beat`.
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<f64>;
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let fixed = |name, kind, value| Input {
        optional: false,
        rate: Rate::Fixed,
        ..port(name, kind, Some(value))
    };
    if op != Primitive::BandEnergy {
        return None;
    }
    let inputs = [
        (
            "low_hz",
            fixed("Low frequency (Hz)", ValueType::Number, Value::Number(20.0)),
        ),
        (
            "high_hz",
            fixed(
                "High frequency (Hz)",
                ValueType::Number,
                Value::Number(60.0),
            ),
        ),
    ];
    Some(Definition {
        name: "Frequency energy".into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: BTreeMap::from([(
            "value".into(),
            Output {
                value_type: ValueType::Number,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(op),
    })
}

pub(crate) fn request(
    op: Primitive,
    inputs: &BTreeMap<String, Value>,
) -> Result<Option<FeatureRequest>> {
    if op != Primitive::BandEnergy {
        return Ok(None);
    }
    let low_hz = inputs["low_hz"].scalar();
    let high_hz = inputs["high_hz"].scalar();
    if !low_hz.is_finite()
        || !high_hz.is_finite()
        || low_hz < 0.0
        || high_hz <= low_hz
        || high_hz > 100_000.0
    {
        return Err(Error(
            "frequency band needs 0 ≤ low < high ≤ 100,000 Hz".into(),
        ));
    }
    Ok(Some(FeatureRequest { low_hz, high_hz }))
}
