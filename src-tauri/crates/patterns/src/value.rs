use crate::{Cell, Envelope, Error, Mapping, MappingSpec, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Number,
    Beats,
    Proportion,
    Position,
    Boolean,
    AudioSource,
    Drum,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Natural,
    Clip,
    Wrap,
}

/// Values retain their units across graph boundaries. Masks and Lighting are
/// keyed by cell identity, so reordering a selection cannot misalign a wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Number(f64),
    Beats(f64),
    Proportion(f64),
    Position(f64),
    Boolean(bool),
    AudioSource(crate::AudioSource),
    Drum(crate::Drum),
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
}
impl Value {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Number(_) => ValueType::Number,
            Self::Beats(_) => ValueType::Beats,
            Self::Proportion(_) => ValueType::Proportion,
            Self::Position(_) => ValueType::Position,
            Self::Boolean(_) => ValueType::Boolean,
            Self::AudioSource(_) => ValueType::AudioSource,
            Self::Drum(_) => ValueType::Drum,
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
        }
    }
    pub fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Number(v) | Self::Position(v) => v.is_finite(),
            Self::Beats(v) => v.is_finite() && *v >= 0.0,
            Self::Proportion(v) => v.is_finite() && (0.0..=1.0).contains(v),
            Self::Color(c) => c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
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
    pub(crate) fn scalar(&self) -> f64 {
        match self {
            Self::Number(v) | Self::Beats(v) | Self::Proportion(v) | Self::Position(v) => *v,
            _ => unreachable!("graph type checking precedes execution"),
        }
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
