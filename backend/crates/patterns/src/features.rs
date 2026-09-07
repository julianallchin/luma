//! The core asks for analyzed track data through a read-only, seekable source.
//! A missing analysis is an error, not a silent replacement with another stem.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    Mix,
    Bass,
    Drums,
    Vocals,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Drum {
    Kick,
    Snare,
    Hihat,
    Cymbal,
}
impl AudioSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mix => "mix",
            Self::Bass => "bass",
            Self::Drums => "drums",
            Self::Vocals => "vocals",
            Self::Other => "other",
        }
    }
}
impl Drum {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::Snare => "snare",
            Self::Hihat => "hat",
            Self::Cymbal => "cymbal",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum FeatureRequest {
    Band {
        source: AudioSource,
        low_hz: f64,
        high_hz: f64,
    },
    Onsets(Drum),
    Harmony,
}
#[derive(Clone, Debug)]
pub enum FeatureSample {
    Energy(f64),
    /// Absolute beat of the latest event and its stable, zero-based index.
    Onset(Option<(f64, u64)>),
    PitchClass(Option<u8>),
}
pub trait FeatureSource: std::fmt::Debug + Send + Sync {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample>;
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let fixed = |name, kind, value| Input {
        rate: Rate::Fixed,
        ..port(name, kind, Some(value))
    };
    let (name, inputs, outputs) = match op {
        Primitive::BandEnergy => (
            "Frequency energy",
            vec![
                (
                    "source",
                    fixed(
                        "Audio source",
                        ValueType::AudioSource,
                        Value::AudioSource(AudioSource::Mix),
                    ),
                ),
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
            ],
            vec![("value", ValueType::Number)],
        ),
        Primitive::DrumClock => (
            "Drum event time",
            vec![(
                "drum",
                fixed("Drum", ValueType::Drum, Value::Drum(Drum::Kick)),
            )],
            vec![
                ("elapsed", ValueType::Beats),
                ("index", ValueType::Number),
                ("present", ValueType::Proportion),
            ],
        ),
        Primitive::Harmony => (
            "Harmony",
            vec![],
            vec![
                ("pitch_class", ValueType::Number),
                ("present", ValueType::Proportion),
            ],
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

pub(crate) fn request(
    op: Primitive,
    inputs: &BTreeMap<String, Value>,
) -> Result<Option<FeatureRequest>> {
    Ok(Some(match op {
        Primitive::BandEnergy => {
            let Value::AudioSource(source) = inputs["source"] else {
                unreachable!()
            };
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
            FeatureRequest::Band {
                source,
                low_hz,
                high_hz,
            }
        }
        Primitive::DrumClock => {
            let Value::Drum(drum) = inputs["drum"] else {
                unreachable!()
            };
            FeatureRequest::Onsets(drum)
        }
        Primitive::Harmony => FeatureRequest::Harmony,
        _ => return Ok(None),
    }))
}

pub(crate) fn run(
    op: Primitive,
    inputs: &BTreeMap<String, Value>,
    frame: Frame,
) -> Option<Result<BTreeMap<String, Value>>> {
    if !op.reads_track() {
        return None;
    }
    Some((|| {
        let request = request(op, inputs)?.expect("track operation");
        let source = frame
            .features
            .ok_or_else(|| Error("this graph requires analyzed track data".into()))?;
        let outputs = match (op, source.sample(&request, frame.beat)?) {
            (Primitive::BandEnergy, FeatureSample::Energy(value))
                if value.is_finite() && value >= 0.0 =>
            {
                BTreeMap::from([("value".into(), Value::Number(value))])
            }
            (Primitive::DrumClock, FeatureSample::Onset(event)) => {
                let (elapsed, index, present) = match event {
                    Some((beat, index))
                        if beat.is_finite()
                            && beat <= frame.beat
                            && index <= 9_007_199_254_740_991 =>
                    {
                        (frame.beat - beat, index as f64, 1.0)
                    }
                    None => (0.0, 0.0, 0.0),
                    _ => return Err(Error("invalid analyzed onset".into())),
                };
                BTreeMap::from([
                    ("elapsed".into(), Value::Beats(elapsed)),
                    ("index".into(), Value::Number(index)),
                    ("present".into(), Value::Proportion(present)),
                ])
            }
            (Primitive::Harmony, FeatureSample::PitchClass(pitch))
                if pitch.is_none_or(|p| p < 12) =>
            {
                BTreeMap::from([
                    (
                        "pitch_class".into(),
                        Value::Number(pitch.unwrap_or(0) as f64),
                    ),
                    (
                        "present".into(),
                        Value::Proportion(if pitch.is_some() { 1.0 } else { 0.0 }),
                    ),
                ])
            }
            _ => {
                return Err(Error(
                    "track source returned the wrong feature type or range".into(),
                ))
            }
        };
        Ok(outputs)
    })())
}
