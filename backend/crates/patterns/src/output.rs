use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// One head's contribution, preserving the distinction between an unwritten
/// capability and a capability explicitly written as zero. A pattern applies
/// color; brightness is its peak channel, split out here as the dimmer the
/// compositor and fixtures expect. Chromaticity stays normalized.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimmer: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strobe: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}
impl FixtureOutput {
    pub fn from_rgb(rgb: [f64; 3]) -> Self {
        let value = rgb.into_iter().fold(0.0_f64, f64::max);
        Self {
            color: Some(if value > 1e-5 {
                rgb.map(|v| v / value)
            } else {
                [0.0; 3]
            }),
            dimmer: Some(value),
            ..Self::default()
        }
    }
    pub fn rgb(&self) -> [f64; 3] {
        self.color
            .unwrap_or([1.0; 3])
            .map(|v| v * self.dimmer.unwrap_or(0.0))
    }
    /// Capability layout is static across a graph's selected head domain.
    pub fn writes(&self) -> [bool; 5] {
        [
            self.color.is_some(),
            self.dimmer.is_some(),
            self.position.is_some(),
            self.strobe.is_some(),
            self.speed.is_some(),
        ]
    }
    /// Layer complete effect outputs with exactly the cross-clip blend math.
    /// Connections decide base/top ordering; no hidden graph traversal priority.
    pub fn composite(&mut self, top: &Self, mode: crate::BlendMode) {
        let opacity = top.dimmer.unwrap_or(0.0).clamp(0.0, 1.0) as f32;
        if let Some(color) = top.color {
            self.color = Some(
                crate::blend_color(
                    self.color.unwrap_or([1.0; 3]).map(|v| v as f32),
                    color.map(|v| v as f32),
                    opacity,
                    mode,
                )
                .map(f64::from),
            );
        }
        if top.dimmer.is_some() {
            self.dimmer = Some(f64::from(
                crate::blend_value(self.dimmer.unwrap_or(0.0) as f32, opacity, mode)
                    .clamp(0.0, 1.0),
            ));
        }
        if let Some(strobe) = top.strobe {
            self.strobe = Some(f64::from(
                crate::blend_value(self.strobe.unwrap_or(0.0) as f32, strobe as f32, mode)
                    .clamp(0.0, 1.0),
            ));
        }
        if top.position.is_some() {
            self.position = top.position;
        }
        if let Some(speed) = top.speed {
            self.speed = Some(if speed > 0.5 { 1.0 } else { 0.0 });
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self
            .color
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || self.dimmer.is_some_and(|v| !v.is_finite() || v < 0.0)
            || self.position.iter().flatten().any(|v| !v.is_finite())
            || [self.strobe, self.speed]
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
        {
            return Err(Error("invalid fixture output".into()));
        }
        Ok(())
    }
}

pub(crate) fn terminal_definition() -> crate::Definition {
    use crate::*;
    use std::collections::BTreeMap;
    let port = |name: &str, value: Value| Input {
        optional: true,
        name: name.into(),
        description: "Unwired leaves this capability untouched".into(),
        value_type: value.value_type(),
        rate: Rate::Frame,
        default: Some(value),
        author: None,
    };
    Definition {
        name: "Apply".into(),
        inputs: BTreeMap::from([
            ("color".into(), port("Color", Value::Color([1.0; 3]))),
            ("pan".into(), port("Pan (degrees)", Value::Degrees(0.0))),
            ("tilt".into(), port("Tilt (degrees)", Value::Degrees(0.0))),
            ("strobe".into(), port("Strobe", Value::Proportion(0.0))),
            (
                "speed".into(),
                port("Movement speed", Value::Proportion(1.0)),
            ),
        ]),
        outputs: BTreeMap::from([(
            "lighting".into(),
            Output {
                value_type: ValueType::Lighting,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(Primitive::Output),
    }
}
