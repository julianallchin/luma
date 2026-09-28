//! Old color form ids. `color.constant@1`, `color.time@1`, `color.space@1`
//! and `color.chase@1` merged into `color@1`. Stored clip rows are not
//! migrated: this table reads an old clip as `color@1` when it is loaded,
//! and the clip saves as `color@1`. Each conversion gives the same light.
use crate::*;
use std::collections::BTreeMap;

/// The form and inputs that replace an old form id, or `None` when `graph`
/// is not an old id or lacks an input the conversion needs.
pub fn upgrade(
    graph: &str,
    inputs: &BTreeMap<String, Value>,
) -> Option<(&'static str, BTreeMap<String, Value>)> {
    let get = |key: &str| inputs.get(key).cloned();
    let full = || Value::Proportion(1.0);
    let converted = match graph {
        "color.constant@1" => return Some(("color@1", inputs.clone())),
        // One pass through the gradient per hit: `color.time`'s `every`
        // clock is the hit clock of `color@1`.
        "color.time@1" => {
            let (Value::Gradient(gradient), Value::Envelope(curve)) =
                (get("colors")?, get("curve")?)
            else {
                return None;
            };
            [
                (
                    "color",
                    Value::Hit(SourceCurve::Gradient(GradientCurve { gradient, curve })),
                ),
                ("brightness", full()),
                ("every", get("every")?),
                ("alpha", get("alpha")?),
            ]
        }
        "color.space@1" => {
            let (Value::Gradient(gradient), Value::Mapping(axis)) = (get("colors")?, get("axis")?)
            else {
                return None;
            };
            [
                (
                    "color",
                    Value::Space(SpaceSource {
                        axis,
                        gradient: Some(gradient),
                        curve: None,
                        movement: None,
                    }),
                ),
                ("brightness", full()),
                ("every", Value::Beats(0.0)),
                ("alpha", get("alpha")?),
            ]
        }
        // The stroke becomes a moving brightness; its shape is the curve
        // across it. `every` stays the beats between strokes.
        "color.chase@1" => {
            let (
                Value::Mapping(axis),
                Value::Envelope(shape),
                Value::Envelope(path),
                Value::Boolean(width_relative),
                Value::Boundary(boundary),
            ) = (
                get("axis")?,
                get("shape")?,
                get("path")?,
                get("width_relative")?,
                get("boundary")?,
            )
            else {
                return None;
            };
            [
                ("color", get("color")?),
                (
                    "brightness",
                    Value::Space(SpaceSource {
                        axis,
                        gradient: None,
                        curve: Some(shape),
                        movement: Some(Box::new(Movement {
                            path,
                            travel: get("travel")?,
                            width: get("width")?,
                            width_relative,
                            boundary,
                        })),
                    }),
                ),
                ("every", get("every")?),
                ("alpha", get("alpha")?),
            ]
        }
        _ => return None,
    };
    Some((
        "color@1",
        converted
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    ))
}
