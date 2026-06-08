//! Lowering for selection + apply (sink) nodes. Owns: apply_color (seed) +
//! apply_dimmer, apply_strobe, apply_speed, apply_movement, filter_selection,
//! random_select_mask, select (agent extends).
//!
//! Apply sinks bind a slot into `Plan.outputs` (the capability the legacy apply
//! drove). `apply_color` is special: it HSV-splits via `ColorOp::HsvValue`
//! (-> dimmer) + `ColorOp::HsvNormalize` (-> color). The plain applies route their
//! input slot to the matching `OutputBinding` field (channel table in
//! `eval/ops/select_apply.rs`). Reference: legacy `node_graph/nodes/apply.rs`.

use super::{CompileError, LowerCtx, Lowerer};
use crate::eval::ops::color::ColorOp;
use crate::eval::ops::select_apply::SelectApplyOp;
use crate::eval::{OpKind, Phase};

pub fn lower_select_apply(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    if !matches!(
        lc.type_id(),
        "apply_color"
            | "apply_dimmer"
            | "apply_strobe"
            | "apply_speed"
            | "apply_movement"
            | "filter_selection"
            | "random_select_mask"
            | "select"
    ) {
        return None;
    }
    Some(go(lc, low))
}

fn go(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    let id = &lc.node.id;
    match lc.type_id() {
        "apply_color" => {
            let sig = lc.require(low, "signal")?;
            let n = low.slot_shape(sig).0;
            let dim = low.emit(OpKind::Color(ColorOp::HsvValue), vec![sig], n, 1, Phase::Kernel, id, "_dimmer");
            let col = low.emit(OpKind::Color(ColorOp::HsvNormalize), vec![sig], n, 3, Phase::Kernel, id, "_color");
            low.outputs.dimmer = Some(dim);
            low.outputs.color = Some(col);
        }

        // Plain apply sinks: route the input signal into a capability output. The
        // out slot's `n` follows the input slot (assemble broadcasts n=1 over all
        // primitives). The `selection` input scopes primitives; for v1 it is
        // ignored (whole selection) — per-primitive scoping is C-core.
        "apply_dimmer" => {
            let sig = lc.require(low, "signal")?;
            let n = low.slot_shape(sig).0;
            let out = low.emit(OpKind::SelectApply(SelectApplyOp::ApplyDimmer), vec![sig], n, 1, Phase::Kernel, id, "_out");
            low.outputs.dimmer = Some(out);
        }
        "apply_strobe" => {
            let sig = lc.require(low, "signal")?;
            let n = low.slot_shape(sig).0;
            let out = low.emit(OpKind::SelectApply(SelectApplyOp::ApplyStrobe), vec![sig], n, 1, Phase::Kernel, id, "_out");
            low.outputs.strobe = Some(out);
        }
        "apply_speed" => {
            let sig = lc.require(low, "speed")?;
            let n = low.slot_shape(sig).0;
            let out = low.emit(OpKind::SelectApply(SelectApplyOp::ApplySpeed), vec![sig], n, 1, Phase::Kernel, id, "_out");
            low.outputs.speed = Some(out);
        }
        "apply_movement" => {
            let sig = lc.require(low, "uv")?;
            let n = low.slot_shape(sig).0;
            let out = low.emit(OpKind::SelectApply(SelectApplyOp::ApplyMovement), vec![sig], n, 2, Phase::Kernel, id, "_out");
            low.outputs.position = Some(out);
        }

        // Per-primitive capability mask. v1: emit an empty `keep` (all-kept
        // pass-through). The real per-fixture capability membership needs the
        // fixture-definition DB and is resolved C-core / later in the compiler.
        "filter_selection" => {
            low.emit(
                OpKind::SelectApply(SelectApplyOp::FilterSelection { keep: vec![] }),
                vec![],
                low.n,
                1,
                Phase::Prologue,
                id,
                lc.out_port(),
            );
        }

        // Event-driven random subset mask: re-roll per event. The event times come
        // from the upstream events node (beat_pulses → grid pulses, drum_events →
        // onsets), baked at compile.
        "random_select_mask" => {
            let count = lc.const_input("count", 1.0).round().max(0.0) as u32;
            let avoid_repeat = lc.param_bool("avoid_repeat", true);
            let seed = lc.seed();
            let pulse_starts = lc.event_pulses("events_in").unwrap_or_default();
            low.emit(
                OpKind::SelectApply(SelectApplyOp::RandomSelectMask { seed, count, avoid_repeat, pulse_starts }),
                vec![],
                low.n,
                1,
                Phase::Kernel,
                id,
                lc.out_port(),
            );
        }

        // Selection-scoping node. v1: lowered as a no-op pass-through (claimed,
        // nothing emitted, no slot recorded). It changes WHICH primitives
        // downstream applies hit — real sub-selection scoping (spatial_reference,
        // tag_expression) is deferred to C-core. We do NOT fake per-primitive
        // scoping here.
        "select" => {}

        _ => unreachable!("claimed type not handled"),
    }
    Ok(())
}
