//! Original random patterns become coordinate math and shared lattice kernels.
//! Preserve each authored node's exact key independently of names or clip seeds.
use super::*;
use std::hash::{Hash, Hasher};

pub(super) fn lower(node: &NodeInstance) -> Result<Definition, String> {
    let mut key = std::collections::hash_map::DefaultHasher::new();
    node.id.hash(&mut key);
    let seed = key.finish();
    let mut b = Builder::default();
    let seed = b.input("seed", "Seed", ValueType::Seed, Some(Value::Seed(seed)));
    b.inputs.get_mut("seed").unwrap().rate = Rate::Fixed;
    if node.type_id == "noise" {
        let kind = ValueType::Signal(p::SignalType::ANY);
        let x = b.input("x", "X", kind, Some(Value::Number(0.)));
        let x = b.channel(x);
        let y = b.input("y", "Y", kind, Some(Value::Number(0.)));
        let y = b.channel(y);
        let time = b.input("time", "Time", kind, Some(Value::Number(0.)));
        let time = b.channel(time);
        let reference = b.math("add", x.clone(), y.clone());
        let order = b.call("fixture_geometry", [], "index");
        let time = b.call(
            "core/align_domain",
            [("value", time), ("reference", reference), ("order", order)],
            "value",
        );
        let scale = b.scalar(node, "scale", "Scale", 1.);
        let x = b.math("multiply", x, scale.clone());
        let y = b.math("multiply", y, scale.clone());
        let z = b.math("multiply", time, scale);
        let octaves = b.scalar(node, "octaves", "Octaves", 1.);
        let operation = b.node(
            "core/value_noise_3d",
            [
                ("x", x),
                ("y", y),
                ("z", z),
                ("octaves", octaves),
                ("seed", seed),
            ],
        );
        let amplitude = b.scalar(node, "amplitude", "Amplitude", 1.);
        let offset = b.scalar(node, "offset", "Offset", 0.);
        let scaled = b.math("multiply", wire(&operation, "value"), amplitude);
        let output = b.math("add", scaled, offset);
        b.finish("Noise", [("out", output)])
    } else {
        let radius = b.scalar(node, "radius", "Radius", 0.5);
        let speed = b.scalar(node, "speed", "Cycles per beat", 0.25);
        let smoothness = b.scalar(node, "smoothness", "Smoothness", 2.);
        let smoothness = b.math("maximum", smoothness, number(0.5));
        let smoothness = b.math("minimum", smoothness, number(8.));
        let octaves = b.round(smoothness);
        let beat = b.historical_beats(false);
        let position = b.math("multiply", speed, beat);
        let mut axes = Vec::new();
        for axis in 0..2 {
            let stream = b.call(
                "core/seed_stream",
                [("seed", seed.clone()), ("stream", Value::Seed(axis).into())],
                "seed",
            );
            let operation = b.node(
                "core/value_noise_1d",
                [
                    ("position", position.clone()),
                    ("octaves", octaves.clone()),
                    ("seed", stream),
                ],
            );
            let value = b.math("multiply", wire(&operation, "value"), radius.clone());
            let value = b.math("maximum", value, number(-1.));
            let value = b.math("minimum", value, number(1.));
            axes.push(value);
        }
        let uv = b.join(axes.remove(0), axes.remove(0));
        b.finish("Wander", [("uv", uv)])
    }
}
