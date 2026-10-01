//! Numerical signals use fixture × time × channel arrays. Event kernels may
//! introduce a temporary event axis, reduced before returning an output signal.
use crate::{Error, Result};
use ndarray::Array3;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Number,
    Beats,
    Proportion,
    Position,
    Degrees,
    Seconds,
}

/// The channels of a stage vector (U, V, Z), such as an aim direction.
pub const VECTOR: Channels = Channels::Components(match std::num::NonZeroU16::new(3) {
    Some(count) => count,
    None => unreachable!(),
});

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channels {
    Value,
    Rgb,
    PanTilt,
    /// An ordered vector without named channel roles. A matching named socket
    /// can give it meaning (for example three components feeding RGB).
    Components(std::num::NonZeroU16),
}

/// A wire constrains meaning, not whether a value happens to have one fixture
/// or one time sample. Omitted metadata is inferred from connected operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalType {
    pub unit: Option<Unit>,
    pub channels: Option<Channels>,
}
impl SignalType {
    pub const ANY: Self = Self {
        unit: None,
        channels: None,
    };
    pub const fn new(unit: Unit, channels: Channels) -> Self {
        Self {
            unit: Some(unit),
            channels: Some(channels),
        }
    }
    pub fn accepts(self, other: Self) -> bool {
        let dimensionless = |unit| matches!(unit, Unit::Number | Unit::Proportion | Unit::Position);
        self.unit
            .zip(other.unit)
            .is_none_or(|(a, b)| a == b || (dimensionless(a) && dimensionless(b)))
            && self
                .channels
                .zip(other.channels)
                .is_none_or(|(a, b)| a.accepts(b))
    }
}

impl Channels {
    pub fn components(count: usize) -> Result<Self> {
        if count == 1 {
            return Ok(Self::Value);
        }
        u16::try_from(count)
            .ok()
            .and_then(std::num::NonZeroU16::new)
            .map(Self::Components)
            .ok_or_else(|| Error("a signal needs 1–65,535 channels".into()))
    }
    pub fn count(self) -> usize {
        match self {
            Self::Value => 1,
            Self::Rgb => 3,
            Self::PanTilt => 2,
            Self::Components(count) => count.get() as usize,
        }
    }
    fn accepts(self, other: Self) -> bool {
        self == other
            || (self.count() == other.count()
                && matches!(
                    (self, other),
                    (Self::Components(_), _) | (_, Self::Components(_))
                ))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SignalData", into = "SignalData")]
pub struct Signal {
    values: Arc<Array3<f64>>,
    unit: Unit,
    channels: Channels,
    /// None denotes a value broadcast to every fixture. An explicit domain is
    /// ordered and checked; matching sizes alone do not make domains compatible.
    fixtures: Option<Arc<[String]>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalData {
    values: Array3<f64>,
    unit: Unit,
    channels: Channels,
    fixtures: Option<Arc<[String]>>,
}
impl TryFrom<SignalData> for Signal {
    type Error = Error;
    fn try_from(value: SignalData) -> Result<Self> {
        Self::new(value.values, value.unit, value.channels, value.fixtures)
    }
}
impl From<Signal> for SignalData {
    fn from(value: Signal) -> Self {
        Self {
            values: (*value.values).clone(),
            unit: value.unit,
            channels: value.channels,
            fixtures: value.fixtures,
        }
    }
}

impl Signal {
    pub(crate) fn sample(&self, time: usize) -> Result<Self> {
        let t = if self.values.dim().1 == 1 { 0 } else { time };
        if t >= self.values.dim().1 {
            return Err(Error("signal sample outside batch".into()));
        }
        Self::new(
            self.values.slice(ndarray::s![.., t..t + 1, ..]).to_owned(),
            self.unit,
            self.channels,
            self.fixtures.clone(),
        )
    }
    pub(crate) fn canonical(self) -> Self {
        let Some(ids) = &self.fixtures else {
            return self;
        };
        if ids.windows(2).all(|pair| pair[0] < pair[1]) {
            return self;
        }
        let mut order: Vec<_> = (0..ids.len()).collect();
        order.sort_by(|a, b| ids[*a].cmp(&ids[*b]));
        Self {
            values: self.values.select(ndarray::Axis(0), &order).into(),
            fixtures: Some(
                order
                    .iter()
                    .map(|i| ids[*i].clone())
                    .collect::<Vec<_>>()
                    .into(),
            ),
            ..self
        }
    }
    pub fn scalar(value: f64, unit: Unit) -> Result<Self> {
        Self::series(&[value], unit)
    }

    /// An authored channel vector, broadcast across fixtures and time.
    pub fn vector(values: Vec<f64>, unit: Unit) -> Result<Self> {
        let channels = Channels::components(values.len())?;
        Self::new(
            Array3::from_shape_vec((1, 1, values.len()), values).unwrap(),
            unit,
            channels,
            None,
        )
    }
    pub(crate) fn series(values: &[f64], unit: Unit) -> Result<Self> {
        Self::new(
            Array3::from_shape_vec((1, values.len(), 1), values.to_vec()).expect("series shape"),
            unit,
            Channels::Value,
            None,
        )
    }
    pub(crate) fn map(&self, unit: Unit, operation: impl Fn(f64) -> f64) -> Result<Self> {
        Self::new(
            self.values.mapv(operation),
            unit,
            self.channels,
            self.fixtures.clone(),
        )
    }
    pub(crate) fn on_fixtures(&self, fixtures: &[String]) -> Result<Self> {
        if let Some(current) = self.fixtures() {
            if current != fixtures {
                return Err(Error("signal fixture domains differ".into()));
            }
            return Ok(self.clone());
        }
        let (_, t, c) = self.values.dim();
        Self::new(
            self.values
                .broadcast((fixtures.len(), t, c))
                .expect("singleton fixture axis")
                .to_owned(),
            self.unit,
            self.channels,
            Some(fixtures.to_vec().into()),
        )
    }
    pub(crate) fn at(&self, fixture: usize, time: usize, channel: usize) -> f64 {
        let (n, t, c) = self.values.dim();
        self.values[[
            if n == 1 { 0 } else { fixture },
            if t == 1 { 0 } else { time },
            if c == 1 { 0 } else { channel },
        ]]
    }
    pub fn new(
        values: Array3<f64>,
        unit: Unit,
        channels: Channels,
        fixtures: Option<Arc<[String]>>,
    ) -> Result<Self> {
        let (n, _, c) = values.dim();
        if c != channels.count() || values.iter().any(|v| !v.is_finite()) {
            return Err(Error(
                "signal needs finite values and matching channel metadata".into(),
            ));
        }
        if let Some(ids) = &fixtures {
            let unique: std::collections::BTreeSet<_> = ids.iter().collect();
            if ids.len() != n || unique.len() != n || ids.iter().any(|id| id.is_empty()) {
                return Err(Error("signal needs one unique identity per fixture".into()));
            }
        } else if n != 1 {
            return Err(Error(
                "a signal without a fixture domain must broadcast from one row".into(),
            ));
        }
        Ok(Self {
            values: values.into(),
            unit,
            channels,
            fixtures,
        })
    }
    pub fn values(&self) -> &Array3<f64> {
        &self.values
    }
    pub fn unit(&self) -> Unit {
        self.unit
    }
    pub fn channels(&self) -> &Channels {
        &self.channels
    }
    pub fn fixtures(&self) -> Option<&[String]> {
        self.fixtures.as_deref()
    }
}
