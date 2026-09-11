//! One gradient value works in time and across a head field. Colors use
//! normalized sRGB channels; gradients interpolate perceptually in OKLab.
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
        crate::oklab::interpolate(
            a.color.map(|v| v as f32),
            b.color.map(|v| v as f32),
            t as f32,
        )
        .map(f64::from)
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

mod mix;
pub(crate) use mix::mix_palette;

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let field = |name| port(name, ValueType::Field, None);
    let gradient = || {
        port(
            "Gradient",
            ValueType::Gradient,
            Some(Value::Gradient(Gradient::default())),
        )
    };
    let (name, inputs, output, kind) = match op {
        Primitive::PaletteFallback => (
            "Palette fallback",
            vec![
                (
                    "gradient",
                    port(
                        "Palette",
                        ValueType::Gradient,
                        Some(Value::Gradient(Gradient { stops: vec![] })),
                    ),
                ),
                (
                    "fallback",
                    port(
                        "If empty",
                        ValueType::Gradient,
                        Some(Value::Gradient(Gradient::default())),
                    ),
                ),
            ],
            "gradient",
            ValueType::Gradient,
        ),
        Primitive::MixPalette => (
            "Mix palette",
            vec![
                ("gradient", gradient()),
                (
                    "perceptual",
                    Input {
                        rate: Rate::Fixed,
                        ..port(
                            "Perceptual mix",
                            ValueType::Boolean,
                            Some(Value::Boolean(false)),
                        )
                    },
                ),
                (
                    "vibrance",
                    port("Vibrance", ValueType::Number, Some(Value::Number(0.6))),
                ),
                (
                    "weights",
                    port(
                        "Color weights",
                        ValueType::Signal(SignalType {
                            unit: Some(Unit::Number),
                            channels: None,
                        }),
                        None,
                    ),
                ),
            ],
            "color",
            ValueType::ColorField,
        ),
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
        Primitive::SampleGradientField => (
            "Sample gradient per head",
            vec![("gradient", gradient()), ("position", field("Position"))],
            "color",
            ValueType::ColorField,
        ),
        Primitive::ColorField => (
            "Broadcast color",
            vec![(
                "color",
                port("Color", ValueType::Color, Some(Value::Color([1.0; 3]))),
            )],
            "color",
            ValueType::ColorField,
        ),
        Primitive::WriteColor => (
            "Color output",
            vec![("color", port("Color", ValueType::ColorField, None))],
            "lighting",
            ValueType::Lighting,
        ),
        Primitive::Hsv => (
            "HSV color per head",
            vec![
                ("hue", field("Hue (turns)")),
                (
                    "saturation",
                    port(
                        "Saturation",
                        ValueType::Proportion,
                        Some(Value::Proportion(1.0)),
                    ),
                ),
                (
                    "value",
                    port("Value", ValueType::Proportion, Some(Value::Proportion(1.0))),
                ),
            ],
            "color",
            ValueType::ColorField,
        ),
        Primitive::RotateHue => (
            "Rotate hue",
            vec![
                ("color", port("Color", ValueType::ColorField, None)),
                (
                    "turns",
                    port(
                        "Rotation (turns)",
                        ValueType::Number,
                        Some(Value::Number(0.)),
                    ),
                ),
            ],
            "color",
            ValueType::ColorField,
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
    if matches!(
        op,
        Primitive::MixPalette | Primitive::SampleGradient | Primitive::SampleGradientField
    ) {
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

pub(crate) fn hsv(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let chroma = v * s;
    let x = chroma * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let rgb = match h.floor() as u8 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    rgb.map(|c| (c + v - chroma).clamp(0.0, 1.0))
}

/// Hue rotation preserves RGB extrema (and therefore both HSL lightness and
/// HSV value/saturation), including numerical headroom outside display gamut.
pub(crate) fn rotate_hue([r, g, b]: [f64; 3], turns: f64) -> [f64; 3] {
    let maximum = r.max(g).max(b);
    let minimum = r.min(g).min(b);
    let chroma = maximum - minimum;
    if chroma == 0. {
        return [r, g, b];
    }
    let hue = if maximum == r {
        (g - b) / chroma
    } else if maximum == g {
        (b - r) / chroma + 2.
    } else {
        (r - g) / chroma + 4.
    };
    let hue = (hue + turns.rem_euclid(1.) * 6.).rem_euclid(6.);
    let x = chroma * (1. - (hue.rem_euclid(2.) - 1.).abs());
    let rgb = match hue.floor() as u8 {
        0 => [chroma, x, 0.],
        1 => [x, chroma, 0.],
        2 => [0., chroma, x],
        3 => [0., x, chroma],
        4 => [x, 0., chroma],
        _ => [chroma, 0., x],
    };
    rgb.map(|c| c + minimum)
}
