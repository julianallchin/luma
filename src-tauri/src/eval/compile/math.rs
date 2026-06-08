//! Lowering for math / elementwise nodes. Owns: scalar, ramp_between (seed) +
//! math, threshold, remap, round, modulo, abs, abs_diff, etc. (agent extends).
//! Map each legacy `type_id` (+ its "operation" param) to a `MathOp` variant;
//! ports are `a`/`b` for binaries, `in` for unaries. Output shape: `c` follows
//! the inputs; `n = max(input n)` for broadcast. Reference: legacy
//! `node_graph/nodes/signals.rs` (`math` node) + `eval/ops/math.rs`.

use super::{CompileError, LowerCtx, Lowerer};
use crate::eval::ops::math::{BinOp, MathOp, UnaryOp};
use crate::eval::{OpKind, Phase};

pub fn lower_math(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    // NOTE: the standalone `modulo` node is intentionally NOT claimed here. Its
    // legacy semantics are always-positive `((v % d) + d) % d` (signals.rs ~600),
    // which `BinOp::Mod` (truncating remainder) does not match for negative inputs.
    // There is no exact `MathOp` for it, so it is left unclaimed (reported as a gap)
    // rather than faked. (The `math` node's `"modulo"` op IS truncating, so that one
    // maps cleanly to `BinOp::Mod`.)
    if !matches!(
        lc.type_id(),
        "scalar" | "ramp_between" | "math" | "threshold" | "remap" | "round"
    ) {
        return None;
    }
    Some(go(lc, low))
}

fn go(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    let id = &lc.node.id;
    match lc.type_id() {
        "scalar" => {
            let v = lc.param_f32("value", 0.0);
            low.emit(OpKind::Math(MathOp::Scalar(v)), vec![], 1, 1, Phase::Prologue, id, lc.out_port());
        }
        "ramp_between" => {
            let start = lc.require(low, "start")?;
            let end = lc.require(low, "end")?;
            low.emit(OpKind::Math(MathOp::RampBetween), vec![start, end], 1, 1, Phase::Kernel, id, lc.out_port());
        }
        "math" => {
            // Binary op over ports a/b. Output `c` follows the inputs (use the
            // wider input's shape via slot_shape), `n = max(input_n(a), input_n(b))`.
            let a = lc.require(low, "a")?;
            let b = lc.require(low, "b")?;
            let op = lc.param_str("operation").unwrap_or_else(|| "add".into());
            let bin = match op.as_str() {
                "add" => BinOp::Add,
                "subtract" => BinOp::Sub,
                "multiply" => BinOp::Mul,
                "divide" => BinOp::Div,
                "min" => BinOp::Min,
                "max" => BinOp::Max,
                "modulo" => BinOp::Mod, // truncating remainder — matches the `math` node
                "abs_diff" => BinOp::AbsDiff,
                "circular_distance" => BinOp::CircularDistance,
                other => {
                    return Err(CompileError::Graph(format!(
                        "math node '{id}': unsupported operation '{other}'"
                    )))
                }
            };
            // c follows the inputs; pick the wider channel count of the two slots.
            let (_, ca) = low.slot_shape(a);
            let (_, cb) = low.slot_shape(b);
            let c = ca.max(cb);
            let n = lc.input_n(low, "a").max(lc.input_n(low, "b"));
            low.emit(OpKind::Math(MathOp::Binary(bin)), vec![a, b], n, c, Phase::Kernel, id, lc.out_port());
        }
        "threshold" => {
            let input = lc.require(low, "in")?;
            let cutoff = lc.param_f32("threshold", 0.5);
            let (n, c) = low.slot_shape(input);
            low.emit(OpKind::Math(MathOp::Threshold { cutoff }), vec![input], n, c, Phase::Kernel, id, lc.out_port());
        }
        "remap" => {
            let input = lc.require(low, "in")?;
            let in_min = lc.param_f32("in_min", -1.0);
            let in_max = lc.param_f32("in_max", 1.0);
            let out_min = lc.param_f32("out_min", 0.0);
            let out_max = lc.param_f32("out_max", 180.0);
            // Legacy default is clamp=1.0 (true) — see signals.rs `remap`.
            let clamp = lc.param_f32("clamp", 1.0) > 0.5;
            let (n, c) = low.slot_shape(input);
            low.emit(
                OpKind::Math(MathOp::Remap { in_min, in_max, out_min, out_max, clamp }),
                vec![input],
                n,
                c,
                Phase::Kernel,
                id,
                lc.out_port(),
            );
        }
        "round" => {
            let input = lc.require(low, "in")?;
            let op = lc.param_str("operation").unwrap_or_else(|| "round".into());
            let unary = match op.as_str() {
                "floor" => UnaryOp::Floor,
                "ceil" => UnaryOp::Ceil,
                "round" => UnaryOp::Round,
                _ => UnaryOp::Round, // legacy falls back to round
            };
            let (n, c) = low.slot_shape(input);
            low.emit(OpKind::Math(MathOp::Unary(unary)), vec![input], n, c, Phase::Kernel, id, lc.out_port());
        }
        _ => unreachable!("claimed type not handled"),
    }
    Ok(())
}
