use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// One head's contribution, preserving the distinction between an unwritten
/// capability and a capability explicitly written as zero. A pattern applies
/// color, linear Rec. 2020; brightness is its peak channel, split out here as
/// the dimmer the compositor and fixtures expect. The color stays normalized
/// to a peak of 1. A fixture or the display turns the pair into its own
/// emitters with [`crate::color_space::Gamut::split`].
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
    /// Where the head points, with the clip's alpha as weight. Never pan or
    /// tilt: a solver turns it into pan and tilt after compositing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aim: Option<crate::Aim>,
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
    pub fn writes(&self) -> [bool; 6] {
        [
            self.color.is_some(),
            self.dimmer.is_some(),
            self.position.is_some(),
            self.strobe.is_some(),
            self.speed.is_some(),
            self.aim.is_some(),
        ]
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
            || self.aim.is_some_and(|aim| {
                aim.direction.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&aim.weight)
            })
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
            (
                "aim".into(),
                Input {
                    description: "Where the head points in U, V, Z. The vector's length, \
                                  at most 1, is its weight over the aim under it"
                        .into(),
                    ..port("Aim", Value::Vector([0.0, 0.0, -1.0]))
                },
            ),
        ]),
        outputs: BTreeMap::from([(
            crate::clip_graph::OUTPUT.into(),
            Output {
                value_type: ValueType::Lighting,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(Primitive::Output),
    }
}
