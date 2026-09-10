//! Numerical signals use fixture × time × channel arrays. Event kernels may
//! introduce a temporary event axis, reduced before returning an output signal.
use crate::{Error, Result};
use ndarray::{Array3, Zip};
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
    pub(crate) fn unary(self, math: crate::UnaryMath) -> Result<Self> {
        let unit = match math {
            crate::UnaryMath::Absolute | crate::UnaryMath::Floor | crate::UnaryMath::Float32 => {
                self.unit
            }
            crate::UnaryMath::Fraction | crate::UnaryMath::Sine => Some(Unit::Number),
            crate::UnaryMath::SquareRoot => match self.unit {
                Some(Unit::Number | Unit::Proportion) | None => self.unit,
                _ => return Err(Error("square root requires a dimensionless signal".into())),
            },
        };
        Ok(Self { unit, ..self })
    }
    pub(crate) fn power(self, exponent: Self) -> Result<Self> {
        if [self.unit, exponent.unit]
            .into_iter()
            .flatten()
            .any(|u| !matches!(u, Unit::Number | Unit::Proportion))
        {
            return Err(Error("power requires dimensionless signals".into()));
        }
        Ok(Self {
            unit: Some(Unit::Number),
            channels: self.binary(exponent, crate::FieldMath::Multiply)?.channels,
        })
    }
    pub(crate) fn binary(self, other: Self, math: crate::FieldMath) -> Result<Self> {
        let channels = match (self.channels, other.channels) {
            (None, _) | (_, None) => None,
            (Some(a), Some(b)) => Some(a.merge(b)?),
        };
        let unit = match (self.unit, other.unit) {
            (Some(a), Some(b)) => Some(match math {
                crate::FieldMath::Multiply if matches!(a, Unit::Number | Unit::Proportion) => b,
                crate::FieldMath::Multiply if matches!(b, Unit::Number | Unit::Proportion) => a,
                crate::FieldMath::Divide if a == b => Unit::Number,
                crate::FieldMath::Divide if matches!(b, Unit::Number | Unit::Proportion) => a,
                crate::FieldMath::Add
                | crate::FieldMath::Subtract
                | crate::FieldMath::Minimum
                | crate::FieldMath::Maximum
                    if a == b || a == Unit::Number || b == Unit::Number =>
                {
                    if a == Unit::Number {
                        b
                    } else {
                        a
                    }
                }
                _ => return Err(Error(format!("cannot {math:?} {a:?} and {b:?} signals"))),
            }),
            _ => None,
        };
        Ok(Self { unit, channels })
    }

    pub(crate) fn join(self, other: Self) -> Result<Self> {
        let unit = Self {
            channels: None,
            ..self
        }
        .binary(
            Self {
                channels: None,
                ..other
            },
            crate::FieldMath::Maximum,
        )?
        .unit;
        let channels = self
            .channels
            .zip(other.channels)
            .map(|(a, b)| Channels::components(a.count() + b.count()))
            .transpose()?;
        Ok(Self { unit, channels })
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
    fn merge(self, other: Self) -> Result<Self> {
        if self.count() == 1 {
            return Ok(other);
        }
        if other.count() == 1 {
            return Ok(self);
        }
        if !self.accepts(other) {
            return Err(Error("signal channel meanings differ".into()));
        }
        Ok(if matches!(self, Self::Components(_)) {
            other
        } else {
            self
        })
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
    pub(crate) fn zip3(
        &self,
        b: &Self,
        c: &Self,
        unit: Unit,
        operation: impl Fn(f64, f64, f64) -> f64,
    ) -> Result<Self> {
        let layout = self.layout().merge(b)?.merge(c)?;
        let values = Zip::from(
            self.values
                .broadcast(layout.shape)
                .expect("checked broadcast"),
        )
        .and(b.values.broadcast(layout.shape).expect("checked broadcast"))
        .and(c.values.broadcast(layout.shape).expect("checked broadcast"))
        .map_collect(|a, b, c| operation(*a, *b, *c));
        Self::new(values, unit, layout.channels, layout.fixtures)
    }
    pub(crate) fn zip4(
        &self,
        b: &Self,
        c: &Self,
        d: &Self,
        unit: Unit,
        operation: impl Fn(f64, f64, f64, f64) -> f64,
    ) -> Result<Self> {
        let layout = self.layout().merge(b)?.merge(c)?.merge(d)?;
        let values = Zip::from(
            self.values
                .broadcast(layout.shape)
                .expect("checked broadcast"),
        )
        .and(b.values.broadcast(layout.shape).expect("checked broadcast"))
        .and(c.values.broadcast(layout.shape).expect("checked broadcast"))
        .and(d.values.broadcast(layout.shape).expect("checked broadcast"))
        .map_collect(|a, b, c, d| operation(*a, *b, *c, *d));
        Self::new(values, unit, layout.channels, layout.fixtures)
    }
    fn layout(&self) -> Layout {
        Layout {
            shape: self.values.dim(),
            channels: self.channels,
            fixtures: self.fixtures.clone(),
        }
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

    /// Equal axes or singleton broadcasting only; incompatible channel widths
    /// never repeat the last component, as the older evaluator did.
    pub fn zip(
        &self,
        other: &Self,
        unit: Unit,
        operation: impl Fn(f64, f64) -> f64,
    ) -> Result<Self> {
        let layout = self.layout().merge(other)?;
        let values = Zip::from(
            self.values
                .broadcast(layout.shape)
                .expect("checked broadcast"),
        )
        .and(
            other
                .values
                .broadcast(layout.shape)
                .expect("checked broadcast"),
        )
        .map_collect(|a, b| operation(*a, *b));
        Self::new(values, unit, layout.channels, layout.fixtures)
    }

    /// Concatenate channel vectors, broadcasting only the fixture and time
    /// axes. This is also the channel constructor used by editable graph nodes.
    pub fn join_channels(&self, other: &Self) -> Result<Self> {
        let metadata = SignalType::new(self.unit, self.channels)
            .join(SignalType::new(other.unit, other.channels))?;
        let channels = metadata.channels.expect("concrete channel layouts");
        let layout = self.layout().combine(other, channels)?;
        let a = self
            .values
            .broadcast((layout.shape.0, layout.shape.1, self.channels.count()))
            .expect("checked broadcast");
        let b = other
            .values
            .broadcast((layout.shape.0, layout.shape.1, other.channels.count()))
            .expect("checked broadcast");
        let values = ndarray::concatenate(ndarray::Axis(2), &[a, b])
            .map_err(|e| Error(format!("cannot join channels: {e}")))?;
        Self::new(
            values,
            metadata.unit.expect("concrete signal units"),
            channels,
            layout.fixtures,
        )
    }
}

struct Layout {
    shape: (usize, usize, usize),
    channels: Channels,
    fixtures: Option<Arc<[String]>>,
}
impl Layout {
    fn merge(self, other: &Signal) -> Result<Self> {
        let channels = self.channels.merge(other.channels)?;
        self.combine(other, channels)
    }
    fn combine(self, other: &Signal, channels: Channels) -> Result<Self> {
        let fixtures = match (&self.fixtures, &other.fixtures) {
            (Some(a), Some(b)) if a != b => {
                return Err(Error("signal fixture domains differ".into()))
            }
            (Some(a), _) | (_, Some(a)) => Some(a.clone()),
            _ => None,
        };
        let axis = |a, b| {
            if a == b || b == 1 {
                Ok(a)
            } else if a == 1 {
                Ok(b)
            } else {
                Err(Error("signal axes must match or broadcast from one".into()))
            }
        };
        let (a, b, _) = other.values.dim();
        let shape = (
            axis(self.shape.0, a)?,
            axis(self.shape.1, b)?,
            channels.count(),
        );
        if shape
            .0
            .checked_mul(shape.1)
            .and_then(|v| v.checked_mul(shape.2))
            .is_none_or(|v| v > 16_777_216)
        {
            return Err(Error(
                "signal tensor exceeds 16,777,216 elements; request fewer time samples".into(),
            ));
        }
        Ok(Self {
            shape,
            channels,
            fixtures,
        })
    }
}
