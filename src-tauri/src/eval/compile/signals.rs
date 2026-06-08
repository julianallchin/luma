//! Lowering for temporal generator + reduction nodes. Owns: ramp, sine_wave,
//! noise, wander, sweep, circle, figure_8, beat_envelope, beat_pulses, adsr,
//! falloff, normalize, invert, time_delay (agent fills in).
//!
//! Most map straight to a `SignalOp` variant with params read off the node
//! (`lc.param_f32(...)`); beat-synced ones use `ResidentContext.beat_grid`.
//! Conventions the agent must honor:
//!   - seed: pass `lc.seed()` (DefaultHasher of node id) to Noise/Wander.
//!   - config ports fed by a `scalar` (e.g. beat_envelope:subdivision,
//!     sweep:phase): use `lc.const_input("subdivision", default)`.
//!   - normalize / invert: allocate two `ResidentContext.frozen` slots and pass
//!     `stat_idx`; the dense-grid stat pass that fills them is C3 (compiler-side,
//!     not yet wired — lowering can emit the op with a stat_idx now).
//!   - time_delay: emit identity over input 0 (the compiler re-evals the upstream
//!     cone at times-delay — C-core, later).
//! Reference: legacy `node_graph/nodes/signals.rs` + `eval/ops/signals.rs`.

use super::{CompileError, LowerCtx, Lowerer};
use crate::eval::ops::signals::{AdsrParams, SignalOp};
use crate::eval::{OpKind, Phase};

/// Temporal generators this category claims. The reduction / cone-eval ops
/// (`normalize`, `invert`, `adsr`, `time_delay`) are intentionally left
/// UNCLAIMED — they need compiler C-core work not yet wired (frozen-stat pass /
/// pulse_starts-from-beats / upstream cone re-eval), so they stay UnknownNode
/// rather than be faked here.
const CLAIMED: &[&str] = &[
    "ramp",
    "sine_wave",
    "noise",
    "wander",
    "sweep",
    "circle",
    "figure_8",
    "falloff",
    "beat_envelope",
    "beat_pulses",
];

pub fn lower_signals(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    if std::env::var("DISABLE_SIGNALS").is_ok() {
        return None;
    }
    if !CLAIMED.contains(&lc.type_id()) {
        return None;
    }
    Some(go(lc, low))
}

fn go(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    // All temporal generators are Kernel phase.
    let ph = Phase::Kernel;
    match lc.type_id() {
        "ramp" => {
            low.emit(OpKind::Signal(SignalOp::Ramp), vec![], 1, 1, ph, &lc.node.id, "out");
        }
        "sine_wave" => {
            let op = SignalOp::SineWave {
                subdivision: lc.param_f32("subdivision", 1.0),
                phase_deg: lc.param_f32("phase_deg", 0.0),
                amplitude: lc.param_f32("amplitude", 1.0),
                offset: lc.param_f32("offset", 0.0),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 1, ph, &lc.node.id, "out");
        }
        "noise" => {
            let op = SignalOp::Noise {
                scale: lc.param_f32("scale", 1.0),
                octaves: lc.param_f32("octaves", 1.0).clamp(1.0, 8.0) as u32,
                amplitude: lc.param_f32("amplitude", 1.0),
                offset: lc.param_f32("offset", 0.0),
                seed: lc.seed(),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 1, ph, &lc.node.id, "out");
        }
        "wander" => {
            let op = SignalOp::Wander {
                radius: lc.param_f32("radius", 0.5),
                speed: lc.param_f32("speed", 0.25),
                smoothness: lc.param_f32("smoothness", 2.0),
                seed: lc.seed(),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, "uv");
        }
        "sweep" => {
            let op = SignalOp::Sweep {
                angle_deg: lc.param_f32("angle", 0.0),
                range: lc.param_f32("range", 1.0),
                speed: lc.param_f32("speed", 0.5),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, "uv");
        }
        "circle" => {
            let op = SignalOp::Circle {
                radius: lc.param_f32("radius", 1.0),
                speed: lc.param_f32("speed", 0.25),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, "uv");
        }
        "figure_8" => {
            let op = SignalOp::Figure8 {
                width: lc.param_f32("width", 1.0),
                height: lc.param_f32("height", 0.5),
                speed: lc.param_f32("speed", 0.25),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, "uv");
        }
        "falloff" => {
            let input = lc.require(low, "in")?;
            let (n, c) = low.slot_shape(input);
            let op = SignalOp::Falloff {
                width: lc.param_f32("width", 1.0),
                curve: lc.param_f32("curve", 0.0),
            };
            // Phase follows the input cone; but as a temporal-category op we keep
            // Kernel — falloff over a kernel input is kernel, and pure-prologue
            // input is rare here. Shape follows input.
            low.emit(OpKind::Signal(op), vec![input], n, c, ph, &lc.node.id, "out");
        }
        "beat_envelope" => {
            let subdivision = lc.const_input("subdivision", 1.0);
            let offset = lc.const_input("offset", 0.0);
            let only_downbeats = lc.param_bool("only_downbeats", false);
            let beat_step_beats = if subdivision.abs() < 1e-3 {
                1.0
            } else {
                (1.0 / subdivision).abs()
            };
            let params = AdsrParams {
                attack: lc.param_f32("attack", 0.3),
                decay: lc.param_f32("decay", 0.2),
                sustain: lc.param_f32("sustain", 0.3),
                release: lc.param_f32("release", 0.2),
                sustain_level: lc.param_f32("sustain_level", 0.7),
                a_curve: lc.param_f32("attack_curve", 0.0),
                d_curve: lc.param_f32("decay_curve", 0.0),
                amp: lc.param_f32("amplitude", 1.0),
                fit_to_gap: true,
                length_beats: beat_step_beats,
                // bpm filled at runtime from the grid; the kernel reads the grid
                // for pulse times, but AdsrParams::fixed_length_sec needs a bpm
                // for the no-gap fallback. Use a neutral default.
                bpm: 120.0,
            };
            let op = SignalOp::BeatEnvelope { subdivision, offset, only_downbeats, params };
            low.emit(OpKind::Signal(op), vec![], 1, 1, ph, &lc.node.id, "out");
        }
        "beat_pulses" => {
            let subdivision = lc.const_input("subdivision", 1.0);
            let offset = lc.const_input("offset", 0.0);
            let only_downbeats = lc.param_bool("only_downbeats", false);
            let op = SignalOp::BeatPulses { subdivision, offset, only_downbeats, tol: 0.05 };
            low.emit(OpKind::Signal(op), vec![], 1, 1, ph, &lc.node.id, "out");
        }
        other => {
            return Err(CompileError::UnknownNode {
                id: lc.node.id.clone(),
                type_id: other.to_string(),
            });
        }
    }
    Ok(())
}
