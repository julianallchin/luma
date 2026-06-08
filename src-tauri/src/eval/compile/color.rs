//! Lowering for color nodes. Owns: sample_palette (seed) + color, gradient,
//! palette, rainbow, chroma_palette (agent extends). Inline upstream `Stops`
//! from `pattern_args` via `lc.resolve_stops(port)`. Output is usually `c=3`,
//! `n` follows the driving input. Reference: legacy `node_graph/nodes/color.rs`
//! + `eval/ops/color.rs`.

use super::{parse_stops, CompileError, LowerCtx, Lowerer};
use crate::eval::ops::color::{default_chroma_rainbow, ColorOp};
use crate::eval::{OpKind, Phase};
use crate::models::node_graph::Stops;

pub fn lower_color(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    if !matches!(
        lc.type_id(),
        "sample_palette" | "color" | "gradient" | "palette" | "rainbow" | "chroma_palette"
    ) {
        return None;
    }
    Some(go(lc, low))
}

fn go(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    let id = &lc.node.id;
    match lc.type_id() {
        "sample_palette" => {
            let u = lc.require(low, "u")?;
            let stops = lc.resolve_stops("stops")?;
            let n = low.slot_shape(u).0; // follows the position signal's n
            low.emit(OpKind::Color(ColorOp::SamplePalette { stops }), vec![u], n, 3, Phase::Kernel, id, "out");
        }
        "color" => {
            // param `"color"`: either a `{r,g,b,a}` object or a `"#hex"` string,
            // possibly carried as a JSON-encoded text param (legacy stores it as a
            // string). Parse to [f32;4] in 0..1. Constant -> Prologue (t-invariant).
            let rgba = parse_color_param(lc);
            low.emit(OpKind::Color(ColorOp::Constant(rgba)), vec![], 1, 3, Phase::Prologue, id, "out");
        }
        "gradient" => {
            // Legacy `gradient` node has no input ports — its Stops come from the
            // inline `value` text param. The eval seed wires gradients through a
            // `pattern_args` edge to `sample_palette`; here we also support a
            // `pattern_args`-fed `stops` port for parity. Gradient samples over
            // the time axis -> Kernel.
            let stops = resolve_node_stops(lc, "stops", "value");
            low.emit(OpKind::Color(ColorOp::Gradient(stops)), vec![], 1, 3, Phase::Kernel, id, "out");
        }
        "palette" => {
            // Legacy `palette` node: inline `value` text param of K colors. Baked
            // to one row per stop color, broadcast over time (t-invariant).
            let stops = resolve_node_stops(lc, "stops", "value");
            low.emit(OpKind::Color(ColorOp::Palette(stops)), vec![], low.n, 3, Phase::Prologue, id, "out");
        }
        "rainbow" => {
            let offset = lc.param_f32("offset", 0.0);
            let spread = lc.param_f32("spread", 1.0);
            let saturation = lc.param_f32("saturation", 1.0);
            // Input `in` drives the hue. If unwired, fall back to a constant 0
            // input slot is not available here; require the input (the seed wires
            // a ramp upstream just as the legacy preview synthesized one).
            let sig = lc.require(low, "in")?;
            let n = low.slot_shape(sig).0; // n follows the input
            low.emit(
                OpKind::Color(ColorOp::Rainbow { offset, spread, saturation }),
                vec![sig],
                n,
                3,
                Phase::Kernel,
                id,
                "out",
            );
        }
        "chroma_palette" => {
            // 12 palette colors: inline from the `stops` port (pattern_args) or the
            // `fallback_palette` text param, sampled at 12 uniform positions; else
            // the default rainbow.
            let colors = chroma_colors(lc);
            // The chroma input comes from `harmony_analysis` (an audio node not yet
            // lowered). If it isn't available we still emit the op — it returns
            // black without a 12-channel input (see run_color). Phase follows the
            // chroma input (Kernel, audio-derived); without it default to Kernel.
            let chroma = lc.input(low, "chroma");
            let (inputs, n, phase) = match chroma {
                Some(slot) => (vec![slot], low.slot_shape(slot).0, Phase::Kernel),
                None => (vec![], 1, Phase::Kernel),
            };
            low.emit(OpKind::Color(ColorOp::ChromaPalette { colors }), inputs, n, 3, phase, id, "out");
        }
        _ => unreachable!("claimed type not handled"),
    }
    Ok(())
}

/// Parse the legacy `color` node's `"color"` param into normalized RGBA. The
/// value may be a `"#hex"` string, a `{r,g,b,a}` object (0..255 channels), or a
/// JSON-encoded text param holding either of those.
fn parse_color_param(lc: &LowerCtx) -> [f32; 4] {
    let Some(v) = lc.param("color") else {
        return [1.0, 0.0, 0.0, 1.0];
    };
    parse_color_value(v)
}

fn parse_color_value(v: &serde_json::Value) -> [f32; 4] {
    match v {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('#') {
                return super::parse_hex(trimmed);
            }
            // Legacy stores the object as a JSON-encoded text param.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                return parse_color_value(&parsed);
            }
            [1.0, 0.0, 0.0, 1.0]
        }
        serde_json::Value::Object(_) => {
            let chan = |k: &str, d: f64| v.get(k).and_then(|x| x.as_f64()).unwrap_or(d);
            [
                (chan("r", 255.0) / 255.0) as f32,
                (chan("g", 0.0) / 255.0) as f32,
                (chan("b", 0.0) / 255.0) as f32,
                // alpha is 0..1 in the legacy node (see color.rs:466).
                chan("a", 1.0) as f32,
            ]
        }
        _ => [1.0, 0.0, 0.0, 1.0],
    }
}

/// Resolve a node's `Stops`: prefer a `pattern_args`-fed `port` (inlined), else
/// the inline text/JSON `param`. Returns empty `Stops` if neither is present.
fn resolve_node_stops(lc: &LowerCtx, port: &str, param: &str) -> Stops {
    if let Ok(stops) = lc.resolve_stops(port) {
        return stops;
    }
    if let Some(v) = lc.param(param) {
        return stops_from_param(v);
    }
    Stops::default()
}

/// Parse a Stops param that may be a JSON-encoded text string or an inline
/// object, in either the `{stops:[{color,t}]}` or `{colors:[hex]}` shape.
fn stops_from_param(v: &serde_json::Value) -> Stops {
    let owned;
    let v = match v {
        serde_json::Value::String(s) => {
            owned = serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::json!({}));
            &owned
        }
        other => other,
    };
    // `{colors:[hex]}` -> uniform-spaced stops; `{stops:[...]}` -> parse_stops.
    if v.get("stops").is_some() {
        return parse_stops(v);
    }
    if let Some(arr) = v.get("colors").and_then(|c| c.as_array()) {
        let k = arr.len();
        let stops = arr
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let t = if k <= 1 { 0.0 } else { i as f32 / (k - 1) as f32 };
                let rgba = c.as_str().map(super::parse_hex).unwrap_or([0.0, 0.0, 0.0, 1.0]);
                (t, rgba)
            })
            .collect();
        return Stops { stops };
    }
    Stops::default()
}

/// Resolve the 12 chroma palette colors: a `pattern_args`-fed `stops` port, or
/// the `fallback_palette` text param, sampled at 12 uniform positions; else the
/// default rainbow.
fn chroma_colors(lc: &LowerCtx) -> [[f32; 4]; 12] {
    let stops = if let Ok(s) = lc.resolve_stops("stops") {
        Some(s)
    } else {
        lc.param("fallback_palette").map(stops_from_param)
    };
    match stops {
        Some(s) if !s.is_empty() => {
            let v = s.sample_uniform(12);
            let mut out = [[0.0; 4]; 12];
            for (i, c) in v.iter().enumerate().take(12) {
                out[i] = *c;
            }
            out
        }
        _ => default_chroma_rainbow(),
    }
}
