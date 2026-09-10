//! Historical held selections become targeted events queried at the current
//! time. No previous draws or per-frame random state survive migration.
use super::*;
use std::hash::{Hash, Hasher};

pub(super) fn lower(node: &NodeInstance) -> Result<Definition, String> {
    let mut b = Builder::default();
    let events = b.input(
        "events_in",
        "Events",
        ValueType::Events,
        Some(Value::Events(p::Events::Beats {
            times: p::EventTimes::new(Vec::new()).unwrap(),
        })),
    );
    let count = b.scalar(node, "count", "Count", 1.);
    let avoid = node
        .params
        .get("avoid_repeat")
        .and_then(|v| v.as_bool().or_else(|| v.as_f64().map(|v| v > 0.5)))
        .unwrap_or(true);
    let cycle = b.input(
        "cycle",
        "Shuffled cycle",
        ValueType::Boolean,
        Some(Value::Boolean(avoid)),
    );
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    node.id.hash(&mut hash);
    let seed = b.input(
        "seed",
        "Seed",
        ValueType::Seed,
        Some(Value::Seed(hash.finish())),
    );
    for input in b.inputs.values_mut() {
        input.rate = Rate::Fixed;
    }

    // Count used nearest-integer rounding, capped by the selected head count.
    let count = b.unary("float32", count);
    let count = b.math("maximum", count, number(0.));
    let count = b.math("add", count, number(0.5));
    let count = b.unary("floor", count);
    let geometry = b.call("fixture_geometry", [], "index");
    let heads = b.call("core/head_count", [("value", geometry)], "value");
    let proportion = b.math("divide", count, heads);
    let proportion = b.clamp(proportion);
    let selected = b.call(
        "random_subset",
        [
            ("events", events.clone()),
            ("proportion", proportion),
            ("seed", seed),
            ("cycle", cycle),
        ],
        "events",
    );
    let time = b.node("core/track_time", []);
    let current = b.node(
        "core/event_window",
        [("events", selected), ("time", wire(&time, "seconds"))],
    );

    // Old playback only emitted draws from the event active at clip start.
    // Keep that boundary using event indices, without replaying the draw chain.
    let first = b.call(
        "core/event_window",
        [("events", events), ("time", wire(&time, "clip_start"))],
        "index",
    );
    let before = b.greater(first, wire(&current, "index"));
    let mask = b.choose(before, number(0.), wire(&current, "weights"));
    b.finish("Random selection", [("out", mask)])
}
