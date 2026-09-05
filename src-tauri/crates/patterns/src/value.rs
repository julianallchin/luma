use crate::{Cell, Error, Mapping, MappingSpec, Result};
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
    Color,
    Mapping,
    Coordinates,
    Boundary,
    Envelope,
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

/// Normalized domain/value knots. The consumer supplies time or spatial meaning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub points: Vec<[f64; 2]>,
}

impl Envelope {
    /// A symmetric edge profile; useful as an editable preset or graph input.
    pub fn soft_edges(softness: f64) -> Self {
        let edge = softness.clamp(0., 1.) * 0.5;
        let points = if edge <= 0. {
            vec![[0., 1.], [1., 1.]]
        } else if edge >= 0.5 {
            vec![[0., 0.], [0.5, 1.], [1., 0.]]
        } else {
            vec![[0., 0.], [edge, 1.], [1. - edge, 1.], [1., 0.]]
        };
        Self { points }
    }

    pub fn validate(&self) -> Result<()> {
        if self.points.len() < 2
            || self.points[0][0] != 0.0
            || self.points.last().unwrap()[0] != 1.0
            || self
                .points
                .iter()
                .any(|p| p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)))
            || self.points.windows(2).any(|w| w[0][0] >= w[1][0])
        {
            return Err(Error(
                "envelope needs increasing normalized knots from 0 to 1".into(),
            ));
        }
        Ok(())
    }
    pub fn sample(&self, progress: f64) -> f64 {
        let p = progress.clamp(0.0, 1.0);
        for pair in self.points.windows(2) {
            if p <= pair[1][0] {
                let t = (p - pair[0][0]) / (pair[1][0] - pair[0][0]);
                return pair[0][1] + t * (pair[1][1] - pair[0][1]);
            }
        }
        self.points.last().map_or(0.0, |p| p[1])
    }
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
    Color([f64; 3]),
    Mapping(MappingSpec),
    Coordinates(Mapping),
    Boundary(Boundary),
    Envelope(Envelope),
    Mask(BTreeMap<String, f64>),
    Lighting(BTreeMap<String, [f64; 3]>),
}
impl Value {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Number(_) => ValueType::Number,
            Self::Beats(_) => ValueType::Beats,
            Self::Proportion(_) => ValueType::Proportion,
            Self::Position(_) => ValueType::Position,
            Self::Boolean(_) => ValueType::Boolean,
            Self::Color(_) => ValueType::Color,
            Self::Mapping(_) => ValueType::Mapping,
            Self::Coordinates(_) => ValueType::Coordinates,
            Self::Boundary(_) => ValueType::Boundary,
            Self::Envelope(_) => ValueType::Envelope,
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
            Self::Envelope(e) => return e.validate(),
            Self::Mapping(m) => return m.validate(),
            Self::Coordinates(m) => return m.validate(),
            Self::Mask(m) => m.values().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            Self::Lighting(m) => m.values().flatten().all(|v| v.is_finite() && *v >= 0.0),
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
    /// Absolute musical position; host maps track seconds through its beat grid.
    pub beat: f64,
    pub clip_start: f64,
    pub seed: u64,
}
