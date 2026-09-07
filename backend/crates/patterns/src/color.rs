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
                    t: 0.0,
                    color: [0.0; 3],
                },
                ColorStop {
                    t: 1.0,
                    color: [1.0; 3],
                },
            ],
        }
    }
}
impl Gradient {
    pub fn validate(&self) -> Result<()> {
        if self.stops.len() < 2
            || self.stops.len() > 64
            || self
                .stops
                .iter()
                .any(|s| !s.t.is_finite() || !(0.0..=1.0).contains(&s.t))
            || self.stops.windows(2).any(|s| s[0].t > s[1].t)
        {
            return Err(Error("gradient needs 2–64 ordered stops within 0–1".into()));
        }
        for stop in &self.stops {
            Value::Color(stop.color).validate()?;
        }
        Ok(())
    }
    pub fn sample(&self, position: f64) -> [f64; 3] {
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
}

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
        Primitive::MaskColor => (
            "Mask color",
            vec![
                ("color", port("Color", ValueType::ColorField, None)),
                ("mask", port("Mask", ValueType::Mask, None)),
            ],
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
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: BTreeMap::from([(
            output.into(),
            Output {
                value_type: kind,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(op),
    })
}

pub(crate) fn run(
    op: Primitive,
    i: &BTreeMap<String, Value>,
    frame: Frame,
) -> Option<Result<BTreeMap<String, Value>>> {
    let field = |name: &str| match &i[name] {
        Value::Field(v) | Value::Mask(v) => v,
        _ => unreachable!(),
    };
    let colors = |name: &str| match &i[name] {
        Value::ColorField(v) => v,
        _ => unreachable!(),
    };
    let gradient = || match &i["gradient"] {
        Value::Gradient(v) => v,
        _ => unreachable!(),
    };
    let (key, value) = match op {
        Primitive::SampleGradient => (
            "color",
            Value::Color(gradient().sample(i["position"].scalar())),
        ),
        Primitive::SampleGradientField => (
            "color",
            Value::ColorField(
                field("position")
                    .iter()
                    .map(|(id, p)| (id.clone(), gradient().sample(*p)))
                    .collect(),
            ),
        ),
        Primitive::ColorField => {
            let Value::Color(color) = i["color"] else {
                unreachable!()
            };
            (
                "color",
                Value::ColorField(frame.cells.iter().map(|c| (c.id.clone(), color)).collect()),
            )
        }
        Primitive::MaskColor => {
            let color = colors("color");
            let mask = field("mask");
            if !color.keys().eq(mask.keys()) {
                return Some(Err(Error("color and mask head domains differ".into())));
            }
            (
                "color",
                Value::ColorField(
                    color
                        .iter()
                        .map(|(id, c)| (id.clone(), c.map(|v| v * mask[id])))
                        .collect(),
                ),
            )
        }
        Primitive::WriteColor => (
            "lighting",
            Value::Lighting(
                colors("color")
                    .iter()
                    .map(|(id, c)| (id.clone(), FixtureOutput::from_rgb(*c)))
                    .collect(),
            ),
        ),
        Primitive::Hsv => (
            "color",
            Value::ColorField(
                field("hue")
                    .iter()
                    .map(|(id, h)| {
                        (
                            id.clone(),
                            hsv(*h, i["saturation"].scalar(), i["value"].scalar()),
                        )
                    })
                    .collect(),
            ),
        ),
        _ => return None,
    };
    Some(
        value
            .validate()
            .map(|_| BTreeMap::from([(key.into(), value)])),
    )
}

fn hsv(h: f64, s: f64, v: f64) -> [f64; 3] {
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
