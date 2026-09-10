//! Pitch signals and palette mixing remain ordinary editable numerical graphs.
use super::*;

fn rainbow() -> Value {
    let colors = [
        "#ff0000", "#ff8000", "#ffcc00", "#ffff00", "#80ff00", "#00ff00", "#00ff80", "#00ffff",
        "#0080ff", "#0000ff", "#8000ff", "#ff0080",
    ];
    gradient(&super::super::values::parse_stops(
        &serde_json::json!({"colors":colors}),
    ))
}

fn join(b: &mut Builder, channels: &[B]) -> B {
    if channels.len() == 1 {
        return channels[0].clone();
    }
    let middle = channels.len() / 2;
    let a = join(b, &channels[..middle]);
    let c = join(b, &channels[middle..]);
    b.join(a, c)
}

pub(super) fn lower(node: &NodeInstance) -> Result<Definition, String> {
    let mut b = Builder::default();
    if node.type_id == "spectral_shift" {
        let color = b.numeric(node, "in", "Color", None);
        let chroma = b.input(
            "chroma",
            "Pitch weights",
            ValueType::Signal(p::SignalType::new(
                p::Unit::Number,
                p::Channels::components(12).unwrap(),
            )),
            None,
        );
        // The old shift broadcast the first selected head, and ignored its
        // Strength parameter. Keep these choices explicit in the saved graph.
        let order = b.call("fixture_geometry", [], "index");
        let first = |b: &mut Builder, value| {
            b.call(
                "core/field_first",
                [("value", value), ("order", order.clone())],
                "value",
            )
        };
        let color = first(&mut b, color);
        let color: Vec<_> = (0..3).map(|c| b.channel_at(color.clone(), c)).collect();
        let color = join(&mut b, &color);
        let chroma = first(&mut b, chroma);
        let chroma = b.math("maximum", chroma, number(-1.));
        let pitch = b.call("core/channel_argmax", [("value", chroma)], "value");
        let turns = b.math("divide", pitch, number(12.));
        let color = b.call("rotate_hue", [("color", color), ("turns", turns)], "color");
        return b.finish("Spectral shift", [("out", color)]);
    }
    if node.type_id == "harmony_analysis" {
        let harmony = b.node("harmony", []);
        let pitch = wire(&harmony, "pitch_class");
        let present = wire(&harmony, "present");
        let mut channels = Vec::new();
        for pc in 0..12 {
            let distance = b.math("subtract", pitch.clone(), number(pc as f64));
            let distance = b.unary("absolute", distance);
            let selected = b.greater(number(0.5), distance);
            channels.push(b.math("multiply", selected, present.clone()));
        }
        let output = join(&mut b, &channels);
        return b.finish("Pitch signal", [("signal", output)]);
    }
    let chroma = b.numeric(node, "chroma", "Pitch weights", Some(0.));
    let default = node
        .params
        .get("fallback_palette")
        .map(|raw| super::super::values::parse_stops(&parse_json(raw.clone())))
        .filter(|s| !s.is_empty())
        .map(|s| gradient(&s))
        .unwrap_or_else(rainbow);
    let stops = b.input("stops", "Palette", ValueType::Gradient, Some(default));
    // A connected empty palette selected the rainbow, even when the node had
    // a different local fallback. Keep that distinction for shared overrides.
    let stops = b.call(
        "palette_fallback",
        [("gradient", stops), ("fallback", rainbow().into())],
        "gradient",
    );
    // The original palette used the first selected fixture's pitch weights and
    // broadcast the resulting color. Preserve that choice by explicit order.
    let order = b.call("fixture_geometry", [], "index");
    let chroma = b.call(
        "core/field_first",
        [("value", chroma), ("order", order)],
        "value",
    );
    let count = b.call("core/channel_count", [("value", chroma.clone())], "value");
    let difference = b.math("subtract", count, number(12.));
    let difference = b.unary("absolute", difference);
    let valid = b.greater(number(0.5), difference);
    let sum = b.call(
        "mix_palette",
        [("weights", chroma), ("gradient", stops)],
        "color",
    );
    let maximum = b.call("core/channel_maximum", [("value", sum.clone())], "value");
    let scale = b.math("maximum", maximum, number(0.001));
    let normalized = b.math("divide", sum, scale);
    let output = b.clamp(normalized);
    let output = b.choose(valid, output, Value::Color([0.; 3]).into());
    b.finish("Pitch palette", [("out", output)])
}
