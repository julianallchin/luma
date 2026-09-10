use super::*;
use std::hash::{Hash, Hasher};

pub(super) fn lower(node: &NodeInstance) -> Result<Definition, String> {
    let mut b = Builder::default();
    let stops = b.input("stops", "Palette", ValueType::Gradient, None);
    let points = b.scalar(node, "num_points", "Point count", 6.);
    let softness = b.scalar(node, "softness", "Softness", 0.3);
    let vibrance = b.scalar(node, "vibrance", "Vibrance", 0.6);
    let speed = b.scalar(node, "wander_speed", "Wander speed", 0.3);
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    node.id.hash(&mut hash);
    let offset = node
        .params
        .get("seed_offset")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.) as f32 as u64;
    let seed = b.input(
        "seed",
        "Seed",
        ValueType::Seed,
        Some(Value::Seed(hash.finish() ^ offset)),
    );
    for input in b.inputs.values_mut() {
        input.rate = Rate::Fixed;
    }

    let points = b.math("maximum", points, number(1.));
    let points = b.math("minimum", points, number(64.));
    let points = b.math("add", points, number(0.5));
    let points = b.unary("floor", points);
    let position = b.call("fixture_geometry", [], "position");
    let minimum = b.call("core/field_minimum", [("value", position.clone())], "value");
    let maximum = b.call("core/field_maximum", [("value", position.clone())], "value");
    let range = b.math("subtract", maximum.clone(), minimum.clone());
    let square = b.math("multiply", range.clone(), range);
    let diagonal = b.call("core/channel_sum", [("value", square)], "value");
    let diagonal = b.unary("square_root", diagonal);
    let diagonal = b.math("maximum", diagonal, number(f64::from(0.0001_f32)));
    let temperature = b.math("multiply", softness, diagonal);
    let temperature = b.math("maximum", temperature, number(f64::from(0.0001_f32)));
    let time = b.call("core/track_time", [], "seconds");
    let time = b.math("multiply", time, speed);
    let sites = b.call(
        "wander_points",
        [
            ("time", time),
            ("count", points),
            ("seed", seed),
            ("minimum", minimum),
            ("maximum", maximum),
        ],
        "points",
    );
    let weights = b.call(
        "proximity_weights",
        [
            ("position", position),
            ("points", sites),
            ("temperature", temperature),
        ],
        "weights",
    );
    let mix = b.node(
        "mix_palette",
        [
            ("weights", weights),
            ("gradient", stops),
            ("perceptual", Value::Boolean(true).into()),
            ("vibrance", vibrance),
        ],
    );
    let rgba = b.join(wire(&mix, "color"), wire(&mix, "opacity"));
    let rgba = b.clamp(rgba);
    b.finish("Soft Voronoi", [("out", rgba)])
}
