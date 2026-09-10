//! Preserve event bindings while translating old stage parameters into ordinary
//! envelope points. Curve construction is fixed; the shared sampler evaluates
//! event ages. Chase keeps its full active-event axis.
use super::*;
mod curve;

fn parameter(node: &NodeInstance, id: &str, default: f64) -> f64 {
    f64::from(
        node.params
            .get(id)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default) as f32,
    )
}

fn seconds(value: f64) -> B {
    Value::Seconds(f64::from(value as f32)).into()
}
fn f32_math(b: &mut Builder, op: &str, a: B, c: B) -> B {
    let value = b.math(op, a, c);
    b.unary("float32", value)
}
fn scalar(b: &mut Builder, node: &NodeInstance, id: &str, name: &str, default: f64) -> B {
    let value = b.scalar(node, id, name, default);
    b.inputs.get_mut(id).unwrap().rate = Rate::Fixed;
    b.unary("float32", value)
}
fn checkbox(b: &mut Builder, node: &NodeInstance, id: &str, name: &str, default: bool) -> B {
    let default = node
        .params
        .get(id)
        .and_then(|value| value.as_bool().or_else(|| value.as_f64().map(|n| n > 0.5)))
        .unwrap_or(default);
    let value = b.input(id, name, ValueType::Boolean, Some(Value::Boolean(default)));
    b.inputs.get_mut(id).unwrap().rate = Rate::Fixed;
    value
}
fn grid(b: &mut Builder, node: &NodeInstance) -> (B, B) {
    let subdivision = scalar(b, node, "subdivision", "Subdivision", 1.);
    let offset = scalar(b, node, "offset", "Beat offset", 0.);
    let downbeats = checkbox(b, node, "only_downbeats", "Only downbeats", false);
    let events = b.call(
        "core/grid_events",
        [
            ("subdivision", subdivision.clone()),
            ("offset", offset),
            ("downbeats", downbeats),
        ],
        "events",
    );
    (events, subdivision)
}

// The saved curve is ordinary data. Only event timing remains a composition.
fn envelope(b: &mut Builder, age: B, end: B, node: &NodeInstance) -> Result<B, String> {
    let shape = b.input(
        "shape",
        "Envelope",
        ValueType::Envelope,
        Some(Value::Envelope(curve::convert(node)?)),
    );
    b.inputs.get_mut("shape").unwrap().rate = Rate::Fixed;
    let progress = b.math("divide", age.clone(), end);
    let value = b.call(
        "envelope",
        [("shape", shape), ("progress", progress.clone())],
        "value",
    );
    let level = parameter(node, "sustain_level", 0.7);
    let low = level.min(0.);
    let range = level.max(1.) - low;
    let value = if range == 1. {
        value
    } else {
        b.math("multiply", value, number(range))
    };
    let value = if low == 0. {
        value
    } else {
        b.math("add", value, number(low))
    };
    let negative = b.greater(seconds(0.), age);
    let before_end = b.greater(number(1.), progress);
    let active = b.math("subtract", before_end, negative);
    Ok(b.math("multiply", value, active))
}
pub(in crate::node_graph::migration) fn lower(node: &NodeInstance) -> Result<Definition, String> {
    let mut b = Builder::default();
    if node.type_id == "drum_events" {
        let mut out = Vec::new();
        for (port, drum) in [
            ("kick_out", p::Drum::Kick),
            ("snare_out", p::Drum::Snare),
            ("hat_out", p::Drum::Hihat),
            ("cymbal_out", p::Drum::Cymbal),
        ] {
            let events = b.call(
                "drum_trigger",
                [("drum", Value::Drum(drum).into())],
                "trigger",
            );
            let events = b.call(
                "core/thin_events",
                [("events", events), ("minimum", seconds(0.025))],
                "events",
            );
            out.push((port, events));
        }
        return b.finish("Drum events", out);
    }
    let (events, subdivision) = if node.type_id == "adsr" {
        (
            b.input("events_in", "Events", ValueType::Events, None),
            number(1.),
        )
    } else {
        grid(&mut b, node)
    };
    if let Some(events) = b.inputs.get_mut("events_in") {
        events.rate = Rate::Fixed;
    }
    let time = b.call("core/track_time", [], "seconds");
    let time = b.unary("float32", time);
    if node.type_id == "beat_pulses" {
        let query = f32_math(&mut b, "add", time.clone(), seconds(0.05));
        let recent = b.node(
            "core/event_window",
            [("events", events.clone()), ("time", query)],
        );
        let age = f32_math(&mut b, "subtract", time, wire(&recent, "times"));
        let age = b.unary("absolute", age);
        let outside = b.greater(age, seconds(0.05));
        let inside = b.math("subtract", number(1.), outside);
        let value = b.math("multiply", inside, wire(&recent, "present"));
        return b.finish("Beat pulses", [("events_out", value), ("trigger", events)]);
    }
    let fixed_length = parameter(node, "length_beats", 1.) as f32;
    let length = if node.type_id == "beat_envelope" {
        let abs = b.unary("absolute", subdivision.clone());
        let nearzero = b.greater(number(f64::from(0.001_f32)), abs);
        let reciprocal = f32_math(&mut b, "divide", number(1.), subdivision);
        let reciprocal = b.unary("absolute", reciprocal);
        let beats = b.choose(nearzero, number(1.), reciprocal);
        let seconds = f32_math(&mut b, "multiply", beats, seconds(0.5));
        b.math("maximum", seconds, self::seconds(0.001))
    } else {
        seconds(f64::from((fixed_length * 60. / 120.).max(0.001)))
    };
    let span = if node.type_id == "beat_envelope" || parameter(node, "fit_to_gap", 1.) > 0.5 {
        let gap = b.call(
            "core/event_spacing",
            [("events", events.clone()), ("minimum", seconds(0.0001))],
            "spacing",
        );
        let present = b.greater(gap.clone(), seconds(0.));
        b.choose(present, gap, length)
    } else {
        length
    };
    let weights = [
        ("attack", 0.3),
        ("decay", 0.2),
        ("sustain", 0.3),
        ("release", 0.2),
    ]
    .map(|(id, default)| parameter(node, id, default).clamp(0., 1.) as f32);
    let total = weights.iter().sum::<f32>();
    let anticipate = node
        .params
        .get("anticipate")
        .and_then(|v| v.as_bool().or_else(|| v.as_f64().map(|v| v > 0.5)))
        .unwrap_or(false);
    let lead = if anticipate && total >= 1e-6 {
        b.math(
            "multiply",
            span.clone(),
            number(f64::from(weights[0] / total)),
        )
    } else {
        seconds(0.)
    };
    let query = f32_math(&mut b, "add", time.clone(), lead.clone());
    let recent = b.node(
        "core/event_window",
        [("events", events), ("time", query), ("count", number(2.))],
    );
    let age = f32_math(&mut b, "subtract", time, wire(&recent, "times"));
    let age = f32_math(&mut b, "add", age, lead);
    let value = envelope(&mut b, age, span, node)?;
    let value = b.math("multiply", value, wire(&recent, "present"));
    let current = b.channel_at(value.clone(), 0);
    let previous = b.channel_at(value, 1);
    let value = b.math("maximum", current.clone(), previous);
    let previous_present = b.channel_at(wire(&recent, "present"), 1);
    let value = b.choose(previous_present, value, current);
    let amplitude = parameter(node, "amplitude", 1.);
    let amplitude = if total < 1e-6 { 0. } else { amplitude };
    let value = if amplitude == 1. {
        value
    } else {
        f32_math(&mut b, "multiply", value, number(amplitude))
    };
    b.finish(
        "Envelope",
        [(
            if node.type_id == "adsr" {
                "signal_out"
            } else {
                "out"
            },
            value,
        )],
    )
}
