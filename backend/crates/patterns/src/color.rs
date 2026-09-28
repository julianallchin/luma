//! One gradient value works in time and across a head field. Colors are
//! linear Rec. 2020 (see [`crate::color_space`]); gradients interpolate
//! perceptually in OKLab.
//! Sampling and masking happen before the output's color/dimmer split.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorStop {
    pub t: f64,
    pub color: [f64; 3],
    #[serde(default = "opaque", skip_serializing_if = "is_opaque")]
    pub alpha: f64,
}
fn opaque() -> f64 {
    1.
}
fn is_opaque(value: &f64) -> bool {
    *value == 1.
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gradient {
    pub stops: Vec<ColorStop>,
}
impl Default for Gradient {
    fn default() -> Self {
        Self {
            stops: vec![
                ColorStop {
                    alpha: 1.,
                    t: 0.0,
                    color: [0.0; 3],
                },
                ColorStop {
                    alpha: 1.,
                    t: 1.0,
                    color: [1.0; 3],
                },
            ],
        }
    }
}
impl Gradient {
    pub fn validate(&self) -> Result<()> {
        if self.stops.len() > 64
            || self
                .stops
                .iter()
                .any(|s| !s.t.is_finite() || !(0.0..=1.0).contains(&s.t))
            || self.stops.windows(2).any(|s| s[0].t > s[1].t)
        {
            return Err(Error(
                "gradient needs at most 64 ordered stops within 0–1".into(),
            ));
        }
        for stop in &self.stops {
            Value::Color(stop.color).validate()?;
            if !stop.alpha.is_finite() || !(0. ..=1.).contains(&stop.alpha) {
                return Err(Error("gradient opacity must be in 0–1".into()));
            }
        }
        Ok(())
    }
    pub fn sample(&self, position: f64) -> [f64; 3] {
        if self.stops.is_empty() {
            return [0.; 3];
        }
        let right = self.stops.partition_point(|s| s.t <= position);
        if right == 0 {
            return self.stops[0].color;
        }
        if right == self.stops.len() {
            return self.stops[right - 1].color;
        }
        let a = &self.stops[right - 1];
        let b = &self.stops[right];
        if position == a.t {
            return a.color;
        }
        let t = (position - a.t) / (b.t - a.t);
        crate::color_space::interpolate(a.color, b.color, t)
    }
    pub fn sample_alpha(&self, position: f64) -> f64 {
        if self.stops.is_empty() {
            return 1.;
        }
        let right = self.stops.partition_point(|s| s.t <= position);
        if right == 0 {
            return self.stops[0].alpha;
        }
        if right == self.stops.len() {
            return self.stops[right - 1].alpha;
        }
        let (a, b) = (&self.stops[right - 1], &self.stops[right]);
        let t = (position - a.t) / (b.t - a.t);
        a.alpha + (b.alpha - a.alpha) * t
    }
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let gradient = || {
        port(
            "Gradient",
            ValueType::Gradient,
            Some(Value::Gradient(Gradient::default())),
        )
    };
    let (name, inputs, output, kind) = match op {
        Primitive::SampleGradient => (
            "Sample gradient",
            vec![
                ("gradient", gradient()),
                (
                    "position",
                    port(
                        "Position",
                        ValueType::Proportion,
                        Some(Value::Proportion(0.0)),
                    ),
                ),
            ],
            "color",
            ValueType::Color,
        ),
        _ => return None,
    };
    let mut outputs = BTreeMap::from([(
        output.into(),
        Output {
            value_type: kind,
            rate: Rate::Frame,
        },
    )]);
    if op == Primitive::SampleGradient {
        outputs.insert(
            "opacity".into(),
            Output {
                value_type: ValueType::Proportion,
                rate: Rate::Frame,
            },
        );
    }
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs,
        body: Body::Primitive(op),
    })
}
