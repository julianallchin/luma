use crate::{Cell, Envelope, Error, Mapping, MappingSpec, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Signal(crate::SignalType),
    Number,
    Beats,
    Proportion,
    Position,
    Boolean,
    Color,
    Gradient,
    ColorField,
    Mapping,
    Coordinates,
    Boundary,
    Envelope,
    Field,
    Mask,
    Lighting,
    /// Clip input sources; see `sources`.
    Time,
    Random,
    Noise,
    Audio,
    Space,
    /// A named option of a choice input; the input's options list the names.
    Choice,
    /// A vector in stage U, V, Z: an aim direction or a point in metres.
    Vector,
}
impl std::fmt::Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::{Channels, Unit};
        if let Some(signal) = self.signal_type() {
            let unit = match signal.unit {
                Some(Unit::Number) => "number",
                Some(Unit::Beats) => "beats",
                Some(Unit::Proportion) => "0–1",
                Some(Unit::Position) => "position",
                Some(Unit::Degrees) => "degrees",
                Some(Unit::Seconds) => "seconds",
                None => "inferred units",
            };
            let channels = match signal.channels {
                Some(Channels::Value) => "one channel".into(),
                Some(Channels::Rgb) => "RGB".into(),
                Some(Channels::PanTilt) => "pan/tilt".into(),
                Some(Channels::Components(count)) => format!("{count} channels"),
                None => "any channels".into(),
            };
            write!(f, "Signal ({unit}, {channels})")
        } else {
            write!(f, "{self:?}")
        }
    }
}
impl ValueType {
    pub fn signal_type(self) -> Option<crate::SignalType> {
        use crate::{Channels, SignalType, Unit};
        Some(match self {
            Self::Signal(spec) => spec,
            Self::Number | Self::Field => SignalType::new(Unit::Number, Channels::Value),
            Self::Beats => SignalType::new(Unit::Beats, Channels::Value),
            Self::Position => SignalType::new(Unit::Position, Channels::Value),
            Self::Proportion | Self::Mask => SignalType::new(Unit::Proportion, Channels::Value),
            Self::Color | Self::ColorField => SignalType::new(Unit::Proportion, Channels::Rgb),
            Self::Vector => SignalType::new(Unit::Number, crate::tensor::VECTOR),
            _ => return None,
        })
    }
    pub fn accepts(self, actual: Self) -> bool {
        match (self.signal_type(), actual.signal_type()) {
            (Some(a), Some(b)) => a.accepts(b),
            _ => self == actual,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Natural,
    Clip,
    Wrap,
}

/// Authored literals and single-sample inspection values. Compiled numerical
/// wires use Signals; these stable document forms retain units and head identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Signal(crate::Signal),
    Degrees(f64),
    Seconds(f64),
    Number(f64),
    Beats(f64),
    Proportion(f64),
    Position(f64),
    Boolean(bool),
    Color([f64; 3]),
    Gradient(crate::Gradient),
    ColorField(BTreeMap<String, [f64; 3]>),
    Mapping(MappingSpec),
    Coordinates(Mapping),
    Boundary(Boundary),
    Envelope(Envelope),
    Field(BTreeMap<String, f64>),
    Mask(BTreeMap<String, f64>),
    Lighting(BTreeMap<String, crate::FixtureOutput>),
    /// One curve over the whole clip.
    Time(crate::TimeSource),
    Random(crate::RandomSource),
    Noise(crate::NoiseSource),
    Audio(crate::AudioLevel),
    /// Values along an axis of the heads, still or moving.
    Space(crate::SpaceSource),
    /// One named option, such as `"figure_8"`.
    Choice(String),
    /// U, V, Z: stage right, downstage, up.
    Vector([f64; 3]),
}
impl Value {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Signal(value) => {
                ValueType::Signal(crate::SignalType::new(value.unit(), *value.channels()))
            }
            Self::Degrees(_) => ValueType::Signal(crate::SignalType::new(
                crate::Unit::Degrees,
                crate::Channels::Value,
            )),
            Self::Seconds(_) => ValueType::Signal(crate::SignalType::new(
                crate::Unit::Seconds,
                crate::Channels::Value,
            )),
            Self::Number(_) => ValueType::Number,
            Self::Beats(_) => ValueType::Beats,
            Self::Proportion(_) => ValueType::Proportion,
            Self::Position(_) => ValueType::Position,
            Self::Boolean(_) => ValueType::Boolean,
            Self::Color(_) => ValueType::Color,
            Self::Gradient(_) => ValueType::Gradient,
            Self::ColorField(_) => ValueType::ColorField,
            Self::Mapping(_) => ValueType::Mapping,
            Self::Coordinates(_) => ValueType::Coordinates,
            Self::Boundary(_) => ValueType::Boundary,
            Self::Envelope(_) => ValueType::Envelope,
            Self::Field(_) => ValueType::Field,
            Self::Mask(_) => ValueType::Mask,
            Self::Lighting(_) => ValueType::Lighting,
            Self::Time(_) => ValueType::Time,
            Self::Random(_) => ValueType::Random,
            Self::Noise(_) => ValueType::Noise,
            Self::Audio(_) => ValueType::Audio,
            Self::Space(_) => ValueType::Space,
            Self::Choice(_) => ValueType::Choice,
            Self::Vector(_) => ValueType::Vector,
        }
    }
    /// The source kind of a non-plain form input value.
    pub fn source_kind(&self) -> Option<crate::SourceKind> {
        Some(match self {
            Self::Time(_) => crate::SourceKind::Time,
            Self::Random(_) => crate::SourceKind::Random,
            Self::Noise(_) => crate::SourceKind::Noise,
            Self::Audio(_) => crate::SourceKind::Audio,
            Self::Space(_) => crate::SourceKind::Space,
            _ => return None,
        })
    }
    pub fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Signal(_) => true,
            Self::Number(v) | Self::Position(v) | Self::Degrees(v) | Self::Seconds(v) => {
                v.is_finite()
            }
            Self::Beats(v) => v.is_finite() && *v >= 0.0,
            Self::Proportion(v) => v.is_finite() && (0.0..=1.0).contains(v),
            Self::Color(c) => c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            Self::Vector(v) => v.iter().all(|v| v.is_finite()),
            Self::Choice(name) => !name.is_empty(),
            Self::Gradient(g) => return g.validate(),
            Self::ColorField(colors) => {
                for (id, color) in colors {
                    if id.is_empty() {
                        return Err(Error("color field requires head identities".into()));
                    }
                    Self::Color(*color).validate()?;
                }
                true
            }
            Self::Envelope(e) => return e.validate(),
            Self::Time(curve) => return curve.validate(),
            Self::Random(source) => return source.validate(),
            Self::Noise(noise) => return noise.validate(),
            Self::Audio(audio) => return audio.validate(),
            Self::Space(space) => return space.validate(),
            Self::Mapping(m) => return m.validate(),
            Self::Coordinates(m) => return m.validate(),
            Self::Field(m) => m.iter().all(|(id, v)| !id.is_empty() && v.is_finite()),
            Self::Mask(m) => m
                .iter()
                .all(|(id, v)| !id.is_empty() && v.is_finite() && (0.0..=1.0).contains(v)),
            Self::Lighting(m) => {
                for (id, value) in m {
                    if id.is_empty() {
                        return Err(Error("fixture output requires a head identity".into()));
                    }
                    value.validate()?;
                }
                let layout = m.values().next().map(|v| v.writes());
                if m.values().any(|v| Some(v.writes()) != layout) {
                    return Err(Error(
                        "fixture output capabilities must cover one common head domain".into(),
                    ));
                }
                true
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(Error(format!("invalid {:?} value", self.value_type())))
        }
    }
    pub fn scalar_value(&self) -> Option<f64> {
        match self {
            Self::Signal(value)
                if value.fixtures().is_none() && value.values().dim() == (1, 1, 1) =>
            {
                Some(value.values()[[0, 0, 0]])
            }
            Self::Number(v)
            | Self::Beats(v)
            | Self::Proportion(v)
            | Self::Position(v)
            | Self::Degrees(v)
            | Self::Seconds(v) => Some(*v),
            _ => None,
        }
    }
    pub(crate) fn scalar(&self) -> f64 {
        self.scalar_value()
            .expect("graph type checking precedes execution")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub cells: &'a [Cell],
    pub features: Option<&'a dyn crate::FeatureSource>,
    /// Absolute musical position; host maps track seconds through its beat grid.
    pub beat: f64,
    pub clip_start: f64,
    /// Duration in beats; clip-relative progress always follows the placed clip.
    pub clip_duration: f64,
    pub seed: u64,
}

impl Frame<'_> {
    pub fn validate(&self) -> Result<()> {
        if !self.beat.is_finite() || !self.clip_start.is_finite() {
            return Err(Error("musical time must be finite".into()));
        }
        if !self.clip_duration.is_finite() || self.clip_duration <= 0.0 {
            return Err(Error("clip duration must be finite and positive".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for cell in self.cells {
            if cell.id.is_empty() || !seen.insert(&cell.id) {
                return Err(Error("head identities must be nonempty and unique".into()));
            }
            if cell.world.iter().chain(&cell.uvz).any(|v| !v.is_finite()) {
                return Err(Error("head geometry must be finite".into()));
            }
        }
        Ok(())
    }
}
