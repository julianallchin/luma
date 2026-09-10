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

/// An immutable audio preparation request. Bare sources retain their original
/// string serialization; filters describe preprocessing, never playback state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum AudioInput {
    Source(AudioSource),
    Filtered {
        source: AudioSource,
        filters: Vec<AudioFilter>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioFilter {
    Lowpass { cutoff_hz: f64 },
    Highpass { cutoff_hz: f64 },
}
impl From<AudioSource> for AudioInput {
    fn from(source: AudioSource) -> Self {
        Self::Source(source)
    }
}
impl AudioInput {
    pub fn source(&self) -> AudioSource {
        match self {
            Self::Source(source) | Self::Filtered { source, .. } => *source,
        }
    }
    pub fn name(&self) -> &'static str {
        self.source().name()
    }
    pub fn filters(&self) -> &[AudioFilter] {
        match self {
            Self::Source(_) => &[],
            Self::Filtered { filters, .. } => filters,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.filters().len() > 64 {
            return Err(Error("audio supports at most 64 filter stages".into()));
        }
        for filter in self.filters() {
            let (AudioFilter::Lowpass { cutoff_hz } | AudioFilter::Highpass { cutoff_hz }) = filter;
            if !cutoff_hz.is_finite() || *cutoff_hz < 1. {
                return Err(Error(
                    "audio cutoff must be finite and at least 1 Hz".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn filtered(&self, filter: AudioFilter) -> Result<Self> {
        let mut filters = self.filters().to_vec();
        filters.push(filter);
        let audio = Self::Filtered {
            source: self.source(),
            filters,
        };
        audio.validate()?;
        Ok(audio)
    }
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
    Timing,
    Spectrum {
        source: AudioInput,
        hold_edges: bool,
    },
    Band {
        source: AudioInput,
        low_hz: f64,
        high_hz: f64,
    },
    Onsets(Drum),
    Harmony,
}
#[derive(Clone, Debug)]
pub enum FeatureSample {
    Timing(std::sync::Arc<TrackTiming>),
    /// Magnitude bins normalized by the FFT size, with their uniform spacing.
    Spectrum {
        bins: Vec<f64>,
        bin_hz: f64,
    },
    Energy(f64),
    /// Absolute beat of the latest event and its stable, zero-based index.
    Onset(Option<(f64, u64)>),
    PitchClass(Option<u8>),
}
pub trait FeatureSource: std::fmt::Debug + Send + Sync {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample>;
    /// Immutable absolute event timestamps for the analyzed track.
    fn onsets(&self, drum: Drum) -> Result<EventTimes>;
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let fixed = |name, kind, value| Input {
        optional: false,
        rate: Rate::Fixed,
        ..port(name, kind, Some(value))
    };
    let (name, inputs, outputs) = match op {
        Primitive::FilterAudio { highpass } => (
            if highpass {
                "Audio highpass"
            } else {
                "Audio lowpass"
            },
            vec![
                (
                    "source",
                    fixed(
                        "Audio source",
                        ValueType::AudioSource,
                        Value::AudioSource(AudioSource::Mix.into()),
                    ),
                ),
                (
                    "cutoff_hz",
                    fixed("Cutoff (Hz)", ValueType::Number, Value::Number(200.)),
                ),
            ],
            vec![("source", ValueType::AudioSource)],
        ),
        Primitive::AudioSpectrum => (
            "Audio spectrum",
            vec![
                (
                    "source",
                    fixed(
                        "Audio source",
                        ValueType::AudioSource,
                        Value::AudioSource(AudioSource::Mix.into()),
                    ),
                ),
                (
                    "hold_edges",
                    fixed(
                        "Hold audio boundaries",
                        ValueType::Boolean,
                        Value::Boolean(false),
                    ),
                ),
            ],
            vec![
                (
                    "spectrum",
                    ValueType::Signal(SignalType {
                        unit: Some(Unit::Number),
                        channels: None,
                    }),
                ),
                ("bin_hz", ValueType::Number),
            ],
        ),
        Primitive::BandEnergy => (
            "Frequency energy",
            vec![
                (
                    "source",
                    fixed(
                        "Audio source",
                        ValueType::AudioSource,
                        Value::AudioSource(AudioSource::Mix.into()),
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
        Primitive::DrumClock | Primitive::DrumEvents => (
            "Drum event time",
            vec![(
                "drum",
                fixed("Drum", ValueType::Drum, Value::Drum(Drum::Kick)),
            )],
            if op == Primitive::DrumEvents {
                vec![("trigger", ValueType::Events)]
            } else {
                vec![
                    ("elapsed", ValueType::Beats),
                    ("index", ValueType::Number),
                    ("present", ValueType::Proportion),
                ]
            },
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
        name: if op == Primitive::DrumEvents {
            "Drum trigger"
        } else {
            name
        }
        .into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: outputs
            .into_iter()
            .map(|(k, value_type)| {
                (
                    k.into(),
                    Output {
                        value_type,
                        rate: if matches!(op, Primitive::FilterAudio { .. }) {
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

pub(crate) fn request(
    op: Primitive,
    inputs: &BTreeMap<String, Value>,
) -> Result<Option<FeatureRequest>> {
    Ok(Some(match op {
        Primitive::TrackTime
        | Primitive::GridEvents
        | Primitive::EventWindow
        | Primitive::EventSpacing
        | Primitive::ThinEvents => FeatureRequest::Timing,
        Primitive::AudioSpectrum => {
            let Value::AudioSource(source) = &inputs["source"] else {
                unreachable!()
            };
            let Value::Boolean(hold_edges) = inputs["hold_edges"] else {
                unreachable!()
            };
            FeatureRequest::Spectrum {
                source: source.clone(),
                hold_edges,
            }
        }
        Primitive::BandEnergy => {
            let Value::AudioSource(source) = &inputs["source"] else {
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
                source: source.clone(),
                low_hz,
                high_hz,
            }
        }
        Primitive::DrumClock | Primitive::DrumEvents => {
            let Value::Drum(drum) = inputs["drum"] else {
                unreachable!()
            };
            FeatureRequest::Onsets(drum)
        }
        Primitive::Harmony => FeatureRequest::Harmony,
        _ => return Ok(None),
    }))
}
