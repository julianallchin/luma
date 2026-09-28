//! **One-shot.** Converts the light colors in a stored clip's inputs from
//! gamma-encoded sRGB, the old working space, to linear Rec. 2020, the
//! current one (see [`crate::color_space`]). It exists only for the one-time
//! conversion of stored clip rows and of `presets.json`, and is deleted after
//! that run.
//!
//! **Run it exactly once over each row.** A stored color carries no tag
//! that says which space it is in, so this cannot tell a converted color from
//! an old one: a second run converts again, and every color darkens and
//! shifts.
//!
//! It works on the stored JSON, not the typed [`Value`], so everything that
//! is not a light color stays exactly as stored.
use crate::color_space::from_srgb;
use crate::*;
use serde_json::Value as Json;

/// `inputs_json` (a clip's stored inputs: input name → tagged value) of a
/// clip of form `form`, with every light color converted from gamma sRGB to
/// linear Rec. 2020:
///
/// - `color`: the value;
/// - `gradient`: each stop's color (opacity and position stay);
/// - `color_field`: each head's color;
/// - `lighting`: each head's color and dimmer, as one light;
/// - `time` and `hit`: color keyframes, or the gradient of a gradient read;
/// - `space`: the gradient along the axis.
///
/// Numbers, vectors (an aim's direction or point, which are keyframed as
/// triples too), curves of numbers, axes and choices are untouched. An error
/// names a value this cannot convert safely: a signal with color channels,
/// or triple keyframes on an input of a form this does not know, which may
/// be a vector.
pub fn convert_clip_inputs(form: &str, inputs_json: &Json) -> Result<Json> {
    let inputs = inputs_json
        .as_object()
        .ok_or_else(|| Error("clip inputs must be an object".into()))?;
    let definitions = forms::definitions();
    let definition = definitions
        .iter()
        .find(|(id, _)| *id == form)
        .map(|(_, definition)| definition);
    // Rows may still hold an old color form id (see `upgrade`); their
    // `color` input is a color.
    let old = [
        "color.constant@1",
        "color.time@1",
        "color.space@1",
        "color.chase@1",
    ]
    .contains(&form);
    let mut converted = inputs.clone();
    for (name, value) in converted.iter_mut() {
        // Whether triple keyframes on this input are colors: `Some(false)`
        // for a vector, `None` when the form or the input is unknown.
        let color = match definition.and_then(|definition| definition.inputs.get(name)) {
            Some(spec) => {
                Some(spec.value_type.signal_type().and_then(|t| t.channels) == Some(Channels::Rgb))
            }
            None => (old && name == "color").then_some(true),
        };
        convert_value(value, color).map_err(|Error(error)| Error(format!("{name}: {error}")))?;
    }
    Ok(Json::Object(converted))
}

/// Convert one tagged value in place.
fn convert_value(value: &mut Json, color_input: Option<bool>) -> Result<()> {
    let kind = value
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();
    let Some(inner) = value.get_mut("value") else {
        return Ok(());
    };
    match kind.as_str() {
        "color" => convert_triple(inner),
        "gradient" => convert_gradient(inner),
        "color_field" => each_entry(inner, convert_triple),
        "lighting" => each_entry(inner, convert_light),
        "time" | "hit" => {
            if let Some(gradient) = inner.get_mut("gradient") {
                return convert_gradient(gradient);
            }
            let Some(points) = inner.get_mut("points").and_then(Json::as_array_mut) else {
                return Ok(());
            };
            let triples = points
                .iter()
                .any(|point| point.get(1).is_some_and(Json::is_array));
            match (triples, color_input) {
                (false, _) | (true, Some(false)) => Ok(()),
                (true, Some(true)) => {
                    points
                        .iter_mut()
                        .try_for_each(|point| match point.get_mut(1) {
                            Some(key) => convert_triple(key),
                            None => Err(Error("a keyframe needs a value".into())),
                        })
                }
                (true, None) => Err(Error(
                    "triple keyframes on an unknown input: a color or a vector?".into(),
                )),
            }
        }
        "space" => match inner.get_mut("gradient") {
            Some(gradient) if !gradient.is_null() => convert_gradient(gradient),
            _ => Ok(()),
        },
        "signal" => {
            let rgb = inner.get("channels").and_then(Json::as_str) == Some("rgb");
            if rgb {
                Err(Error("a stored color signal cannot be converted".into()))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

fn triple(value: &Json) -> Result<[f64; 3]> {
    serde_json::from_value(value.clone()).map_err(|_| Error("a color needs [r, g, b]".into()))
}

fn convert_triple(value: &mut Json) -> Result<()> {
    *value = serde_json::json!(from_srgb(triple(value)?));
    Ok(())
}

/// A gradient's stops, each `{"t", "color", "alpha"?}`.
pub(crate) fn convert_gradient(gradient: &mut Json) -> Result<()> {
    gradient
        .get_mut("stops")
        .and_then(Json::as_array_mut)
        .ok_or_else(|| Error("a gradient needs stops".into()))?
        .iter_mut()
        .try_for_each(|stop| match stop.get_mut("color") {
            Some(color) => convert_triple(color),
            None => Err(Error("a gradient stop needs a color".into())),
        })
}

fn each_entry(map: &mut Json, convert: fn(&mut Json) -> Result<()>) -> Result<()> {
    map.as_object_mut()
        .ok_or_else(|| Error("expected an object of heads".into()))?
        .values_mut()
        .try_for_each(convert)
}

/// A [`FixtureOutput`]: its light (color × dimmer) is converted, then split
/// again into a normalized color and a dimmer.
fn convert_light(output: &mut Json) -> Result<()> {
    let Some(color) = output.get("color").filter(|c| !c.is_null()) else {
        return Ok(());
    };
    let color = triple(color)?;
    let dimmer = output.get("dimmer").and_then(Json::as_f64);
    let light = from_srgb(color.map(|v| v * dimmer.unwrap_or(1.)));
    let split = FixtureOutput::from_rgb(light);
    output["color"] = serde_json::json!(split.color);
    if dimmer.is_some() {
        output["dimmer"] = serde_json::json!(split.dimmer);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn srgb(rgb: [f64; 3]) -> Json {
        json!(from_srgb(rgb))
    }

    /// A clip of each form that holds colors, in every place a color can be.
    #[test]
    fn converts_every_light_color_of_each_form() {
        let orange = [1., 0.5, 0.];
        let blue = [0., 0.3, 1.];
        let gradient = json!({"stops": [
            {"t": 0.0, "color": orange},
            {"t": 1.0, "color": blue, "alpha": 0.5}
        ]});
        let converted_gradient = json!({"stops": [
            {"t": 0.0, "color": srgb(orange)},
            {"t": 1.0, "color": srgb(blue), "alpha": 0.5}
        ]});
        let brightness =
            json!({"type": "hit", "value": {"points": [[0, 1, "hold"], [0.5, 1], [1, 0]]}});
        let cases = [
            (
                "color@1",
                json!({
                    "color": {"type": "time", "value": {"points": [[0, orange, "ease-in"], [1, blue]]}},
                    "brightness": brightness,
                    "every": {"type": "beats", "value": 1.0},
                    "alpha": {"type": "proportion", "value": 0.5}
                }),
                json!({
                    "color": {"type": "time", "value": {"points": [[0, srgb(orange), "ease-in"], [1, srgb(blue)]]}},
                    "brightness": brightness,
                    "every": {"type": "beats", "value": 1.0},
                    "alpha": {"type": "proportion", "value": 0.5}
                }),
            ),
            (
                "color@1",
                json!({"color": {"type": "hit", "value": {"gradient": gradient, "curve": {"points": [[0, 0], [1, 1]]}}}}),
                json!({"color": {"type": "hit", "value": {"gradient": converted_gradient, "curve": {"points": [[0, 0], [1, 1]]}}}}),
            ),
            (
                "color@1",
                json!({"color": {"type": "space", "value": {"axis": {"source": {"kind": "u"}}, "gradient": gradient}}}),
                json!({"color": {"type": "space", "value": {"axis": {"source": {"kind": "u"}}, "gradient": converted_gradient}}}),
            ),
            (
                "color.sparkle@1",
                json!({"color": {"type": "color", "value": orange}, "grain": {"type": "number", "value": 1.0}}),
                json!({"color": {"type": "color", "value": srgb(orange)}, "grain": {"type": "number", "value": 1.0}}),
            ),
            (
                "color.noise@1",
                json!({"color": {"type": "hit", "value": {"points": [[0, orange], [1, blue]]}}}),
                json!({"color": {"type": "hit", "value": {"points": [[0, srgb(orange)], [1, srgb(blue)]]}}}),
            ),
            // Old ids are read as color@1 on load; their rows convert too.
            (
                "color.time@1",
                json!({"colors": {"type": "gradient", "value": gradient}}),
                json!({"colors": {"type": "gradient", "value": converted_gradient}}),
            ),
            (
                "color.chase@1",
                json!({"color": {"type": "color", "value": blue}}),
                json!({"color": {"type": "color", "value": srgb(blue)}}),
            ),
            (
                "color.constant@1",
                json!({"color": {"type": "time", "value": {"points": [[0, orange], [1, blue]]}}}),
                json!({"color": {"type": "time", "value": {"points": [[0, srgb(orange)], [1, srgb(blue)]]}}}),
            ),
        ];
        for (form, before, after) in cases {
            assert_eq!(convert_clip_inputs(form, &before).unwrap(), after, "{form}");
        }
    }

    /// An aim's vectors are keyframed as triples too; they are not colors.
    #[test]
    fn leaves_vectors_and_numbers_alone() {
        let aim = json!({
            "direction": {"type": "time", "value": {"points": [[0, [0.0, 0.766, -0.643]], [1, [0.0, 0.0, -1.0]]]}},
            "point": {"type": "vector", "value": [1.0, 0.5, 0.0]},
            "fan": {"type": "time", "value": {"points": [[0, 0], [1, 40]]}}
        });
        assert_eq!(convert_clip_inputs("aim@1", &aim).unwrap(), aim);
        let strobe = json!({"rate": {"type": "proportion", "value": 0.9}});
        assert_eq!(
            convert_clip_inputs("strobe.constant@1", &strobe).unwrap(),
            strobe
        );
    }

    #[test]
    fn refuses_what_it_cannot_tell() {
        let keys =
            json!({"x": {"type": "time", "value": {"points": [[0, [1, 0, 0]], [1, [0, 0, 1]]]}}});
        assert!(convert_clip_inputs("unknown@1", &keys).is_err());
        assert!(convert_clip_inputs("color@1", &json!([])).is_err());
    }

    #[test]
    fn a_light_converts_as_one() {
        let lit =
            json!({"type": "lighting", "value": {"a": {"color": [1.0, 0.5, 0.0], "dimmer": 0.5}}});
        let out = convert_clip_inputs("x@1", &json!({ "l": lit })).unwrap();
        let head = &out["l"]["value"]["a"];
        let color: [f64; 3] = serde_json::from_value(head["color"].clone()).unwrap();
        let dimmer = head["dimmer"].as_f64().unwrap();
        let light = from_srgb([0.5, 0.25, 0.]);
        for (a, b) in color.map(|v| v * dimmer).iter().zip(light) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    /// `presets.json` is the old presets converted by this module; the
    /// `rec2020_presets` example wrote it. Every stored color converted from
    /// sRGB lies in sRGB.
    #[test]
    fn shipped_presets_are_converted() {
        let presets = crate::presets();
        let fire = presets.gradients.iter().find(|g| g.name == "Fire").unwrap();
        // The old first stop was sRGB [0.15, 0, 0]: far darker as linear light.
        let first = fire.gradient.stops[0].color;
        assert!(first[0] < 0.05 && color_space::in_srgb(first), "{first:?}");
        for preset in &presets.presets {
            for value in preset.inputs.values() {
                if let Value::Color(color) = value {
                    assert!(color_space::in_srgb(*color), "{}: {color:?}", preset.name);
                }
            }
        }
    }
}
