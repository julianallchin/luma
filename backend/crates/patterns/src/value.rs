use crate::{Cell, Curve, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Signal(crate::SignalType),
    Number,
    Beats,
    Proportion,
    Color,
    Gradient,
    /// A curve shape: points over x 0–1 with values 0–1.
    Points,
    Lighting,
    /// A vector in stage U, V, Z: an aim direction or a point in metres.
    Vector,
}
impl std::fmt::Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl ValueType {
    pub fn signal_type(self) -> Option<crate::SignalType> {
        use crate::{Channels, SignalType, Unit};
        Some(match self {
            Self::Signal(spec) => spec,
            Self::Number => SignalType::new(Unit::Number, Channels::Value),
            Self::Beats => SignalType::new(Unit::Beats, Channels::Value),
            Self::Proportion => SignalType::new(Unit::Proportion, Channels::Value),
            Self::Color => SignalType::new(Unit::Proportion, Channels::Rgb),
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

/// Literals and single-sample inspection values. Compiled numerical wires
/// use Signals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Signal(crate::Signal),
    Degrees(f64),
    Seconds(f64),
    Number(f64),
    Beats(f64),
    Proportion(f64),
    Color([f64; 3]),
    Gradient(crate::Gradient),
    Points(Curve<f64>),
    Lighting(BTreeMap<String, crate::FixtureOutput>),
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
            Self::Color(_) => ValueType::Color,
            Self::Gradient(_) => ValueType::Gradient,
            Self::Points(_) => ValueType::Points,
            Self::Lighting(_) => ValueType::Lighting,
            Self::Vector(_) => ValueType::Vector,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Signal(_) => true,
            Self::Number(v) | Self::Degrees(v) | Self::Seconds(v) => v.is_finite(),
            Self::Beats(v) => v.is_finite() && *v >= 0.0,
            Self::Proportion(v) => v.is_finite() && (0.0..=1.0).contains(v),
            Self::Color(c) => c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            Self::Vector(v) => v.iter().all(|v| v.is_finite()),
            Self::Gradient(g) => return g.validate(),
            Self::Points(points) => return points.validate(),
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
