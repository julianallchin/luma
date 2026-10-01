//! Compiled graph values. Authored literals become tensors once during lowering;
//! every numerical kernel operates on fixture × time × channel arrays.
use crate::*;
use ndarray::Array3;
use std::collections::BTreeMap;

mod kernels;
mod lighting;
pub(crate) use kernels::run;
pub use lighting::LightingSignal;

#[derive(Clone, Debug)]
enum Data {
    Signal(Signal),
    Controls(std::sync::Arc<[Value]>),
    Lighting(LightingSignal),
}

/// A batch result retains its numerical axes. `sample` is an authoring/inspection
/// boundary; downstream graph operations never convert tensors to scalar maps.
#[derive(Clone, Debug)]
pub struct EvaluatedValue {
    kind: ValueType,
    data: Data,
}
impl EvaluatedValue {
    pub fn signal(&self) -> Option<&Signal> {
        if let Data::Signal(value) = &self.data {
            Some(value)
        } else {
            None
        }
    }
    pub fn lighting(&self) -> Option<&LightingSignal> {
        if let Data::Lighting(value) = &self.data {
            Some(value)
        } else {
            None
        }
    }
    pub(crate) fn numeric(&self) -> &Signal {
        self.signal().expect("validated numerical input")
    }
    pub(crate) fn control(&self, time: usize) -> &Value {
        let Data::Controls(values) = &self.data else {
            unreachable!("validated structured input")
        };
        &values[if values.len() == 1 { 0 } else { time }]
    }
    pub(crate) fn fixed_scalar(&self) -> Result<f64> {
        let signal = self.numeric();
        if signal.values().dim() != (1, 1, 1) {
            return Err(Error("this control requires one fixed value".into()));
        }
        Ok(signal.values()[[0, 0, 0]])
    }
    pub(crate) fn from_signal(kind: ValueType, signal: Signal) -> Result<Self> {
        let signal = signal.canonical();
        let actual = SignalType::new(signal.unit(), *signal.channels());
        if let ValueType::Signal(expected) = kind {
            if !expected.accepts(actual) {
                return Err(Error("incompatible numerical signal metadata".into()));
            }
            return Ok(Self {
                kind: ValueType::Signal(actual),
                data: Data::Signal(signal),
            });
        }
        let expected = kind.signal_type().expect("numerical output type");
        if expected.channels != actual.channels {
            return Err(Error(format!(
                "{kind:?} output has incompatible signal channels"
            )));
        }
        let unit = expected.unit.unwrap();
        // Legacy explicit conversion primitives still declare a destination unit.
        let signal = if signal.unit() == unit {
            signal
        } else {
            signal.map(unit, |v| v)?
        };
        Ok(Self {
            kind,
            data: Data::Signal(signal),
        })
    }
    pub(crate) fn declared(self, kind: ValueType) -> Result<Self> {
        if !kind.accepts(self.kind) {
            return Err(Error(format!("expected {kind:?}, got {:?}", self.kind)));
        }
        match self.data {
            Data::Signal(signal) => Self::from_signal(kind, signal),
            _ => Ok(self),
        }
    }
    pub(crate) fn controls(kind: ValueType, values: Vec<Value>) -> Result<Self> {
        if kind.signal_type().is_some() || kind == ValueType::Lighting {
            return Err(Error("expected a structured control type".into()));
        }
        for value in &values {
            value.validate()?;
            if value.value_type() != kind {
                return Err(Error("structured control type mismatch".into()));
            }
        }
        Ok(Self {
            kind,
            data: Data::Controls(values.into()),
        })
    }
    pub(crate) fn output(value: LightingSignal) -> Self {
        Self {
            kind: ValueType::Lighting,
            data: Data::Lighting(value),
        }
    }
    pub(crate) fn literal(value: &Value) -> Result<Self> {
        value.validate()?;
        let kind = value.value_type();
        let signal = match value {
            Value::Signal(signal) => signal.clone(),
            Value::Degrees(value) => Signal::scalar(*value, Unit::Degrees)?,
            Value::Seconds(value) => Signal::scalar(*value, Unit::Seconds)?,
            Value::Number(v) | Value::Beats(v) | Value::Proportion(v) => {
                Signal::scalar(*v, unit(kind).unwrap())?
            }
            Value::Color(rgb) => Signal::new(
                Array3::from_shape_vec((1, 1, 3), rgb.to_vec()).unwrap(),
                Unit::Proportion,
                Channels::Rgb,
                None,
            )?,
            Value::Vector(uvz) => Signal::vector(uvz.to_vec(), Unit::Number)?,
            Value::Lighting(values) => return Ok(Self::output(LightingSignal::literal(values)?)),
            _ => return Self::controls(kind, vec![value.clone()]),
        };
        Self::from_signal(kind, signal)
    }
    pub fn sample(&self, time: usize) -> Result<Value> {
        let signal = match &self.data {
            Data::Controls(values) => {
                return values
                    .get(if values.len() == 1 { 0 } else { time })
                    .cloned()
                    .ok_or_else(|| Error("control sample outside batch".into()))
            }
            Data::Lighting(value) => return value.sample(time).map(Value::Lighting),
            Data::Signal(value) => value,
        };
        if signal.values().dim().1 != 1 && time >= signal.values().dim().1 {
            return Err(Error("signal sample outside batch".into()));
        }
        if matches!(self.kind, ValueType::Signal(_)) || signal.fixtures().is_some() {
            return Ok(Value::Signal(signal.sample(time)?));
        }
        let scalar = || signal.at(0, time, 0);
        let value = match self.kind {
            ValueType::Number => Value::Number(scalar()),
            ValueType::Beats => Value::Beats(scalar()),
            ValueType::Proportion => Value::Proportion(scalar()),
            ValueType::Color => Value::Color(std::array::from_fn(|ch| signal.at(0, time, ch))),
            ValueType::Vector => Value::Vector(std::array::from_fn(|ch| signal.at(0, time, ch))),
            _ => return Ok(Value::Signal(signal.sample(time)?)),
        };
        // An unbounded intermediate signal is valid computation, even when its
        // values cannot be used as a bounded color/intensity editor default.
        if value.validate().is_ok() {
            Ok(value)
        } else {
            Ok(Value::Signal(signal.sample(time)?))
        }
    }

    pub(crate) fn kind(&self) -> ValueType {
        self.kind
    }
}

fn unit(kind: ValueType) -> Option<Unit> {
    kind.signal_type()?.unit
}

pub(crate) struct Batch<'a> {
    pub frame: Frame<'a>,
    pub times: &'a [f64],
    pub fixtures: &'a [String],
}
