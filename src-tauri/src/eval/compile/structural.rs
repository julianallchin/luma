//! Lowering for structural / non-compute nodes: `pattern_args` (its outputs are
//! resolved inline at consuming edges — args, selection), `audio_input` (the
//! audio source; audio ops read `ResidentContext.audio` directly, so this node
//! emits nothing), and the `view_*` preview taps. All are no-ops here.

use super::{CompileError, LowerCtx, Lowerer};

pub fn lower_structural(lc: &LowerCtx, _low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    match lc.type_id() {
        "pattern_args" | "audio_input" | "view_signal" | "view_uv" | "view_events" => Some(Ok(())),
        _ => None,
    }
}
