//! Lowering for structural / non-compute nodes.
//!
//! `pattern_args` exposes the pattern's args on named output ports. Numeric args
//! (`start_value`, `speed`, …) are materialized here as `Scalar` ops so downstream
//! nodes can read them as signal inputs; object args (Stops/gradient, selection)
//! are resolved at the consuming edge instead (`resolve_stops`, selection scoping)
//! and produce no slot here. `audio_input` is a no-op (audio ops read
//! `ResidentContext.audio`); `view_*` are preview taps.

use super::{parse_hex, CompileError, LowerCtx, Lowerer};
use crate::eval::ops::color::ColorOp;
use crate::eval::ops::math::MathOp;
use crate::eval::{OpKind, Phase};
use serde_json::Value;

pub fn lower_structural(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    match lc.type_id() {
        "pattern_args" => {
            // Materialize a slot for each value arg, keyed by arg name (the
            // pattern_args output port a downstream edge references): numeric ->
            // Scalar, color -> Constant. Stops/gradient/selection args have no
            // slot (resolved at the consuming edge / ignored).
            for (name, val) in lc.args.iter() {
                if let Some(v) = val
                    .as_f64()
                    .or_else(|| val.get("value").and_then(|x| x.as_f64()))
                {
                    low.emit(
                        OpKind::Math(MathOp::Scalar(v as f32)),
                        vec![],
                        1,
                        1,
                        Phase::Prologue,
                        &lc.node.id,
                        name,
                    );
                } else if let Some(rgba) = parse_color_arg(val) {
                    low.emit(
                        OpKind::Color(ColorOp::Constant(rgba)),
                        vec![],
                        1,
                        3,
                        Phase::Prologue,
                        &lc.node.id,
                        name,
                    );
                }
            }
            Some(Ok(()))
        }
        "audio_input" | "view_signal" | "view_uv" | "view_events" => Some(Ok(())),
        _ => None,
    }
}

/// Parse a plain color arg (`"#hex"` or `{r,g,b,a}` with r/g/b in 0..255) into
/// RGBA. Returns None for non-color args (Stops/gradient/palette/selection) so
/// they fall through to edge-time resolution.
fn parse_color_arg(val: &Value) -> Option<[f32; 4]> {
    match val {
        Value::String(s) if s.trim_start().starts_with('#') => Some(parse_hex(s.trim())),
        Value::Object(o) if o.contains_key("r") => {
            let ch = |k: &str, d: f64| o.get(k).and_then(|x| x.as_f64()).unwrap_or(d);
            Some([
                (ch("r", 255.0) / 255.0) as f32,
                (ch("g", 0.0) / 255.0) as f32,
                (ch("b", 0.0) / 255.0) as f32,
                ch("a", 1.0) as f32,
            ])
        }
        _ => None,
    }
}
