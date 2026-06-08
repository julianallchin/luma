//! Lowering for audio-reactive nodes. Owns: frequency_amplitude, stem_splitter,
//! harmony_analysis, drum_events (agent fills in — C4).
//!
//! These map to `AudioOp` variants and read `ResidentContext.audio` / `.stems`.
//! The upstream `audio_input` node is a no-op (handled in `structural`); audio
//! ops take their spectrum from the resident buffer, not an input slot. The STFT
//! is shared (one compiler-injected `Stft`, CSE). NOTE: needs the runner/compiler
//! to populate `ResidentContext.audio` (+ stems) for these patterns to match —
//! that wiring is C4. Reference: `eval/ops/audio.rs`.

use super::{CompileError, LowerCtx, Lowerer};

pub fn lower_audio(_lc: &LowerCtx, _low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    None
}
