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

/// Temporal generators this category claims. `adsr` is the ADSR primitive
/// (handled only when its events come from a `beat_pulses` node — drum_events-fed
/// `adsr` stays UnknownNode, no onset data in ResidentContext). `beat_envelope`
/// is the older composite and lowers to grid-pulses + the same `Adsr` op.
/// `time_delay` lowers to an identity pass (true per-primitive delay is C-core).
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
    "normalize",
    "invert",
    "adsr",
    "time_delay",
];

pub fn lower_signals(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
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
            low.emit(OpKind::Signal(SignalOp::Ramp), vec![], 1, 1, ph, &lc.node.id, lc.out_port());
        }
        "sine_wave" => {
            let op = SignalOp::SineWave {
                subdivision: lc.param_f32("subdivision", 1.0),
                phase_deg: lc.param_f32("phase_deg", 0.0),
                amplitude: lc.param_f32("amplitude", 1.0),
                offset: lc.param_f32("offset", 0.0),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 1, ph, &lc.node.id, lc.out_port());
        }
        "noise" => {
            // Optional per-primitive spatial inputs (x, y) + a time coordinate.
            let x = lc.input(low, "x");
            let y = lc.input(low, "y");
            let time = lc.input(low, "time");
            let inputs: Vec<_> = [x, y, time].into_iter().flatten().collect();
            // Output n follows the widest spatial input (time is broadcast n=1).
            let n = [x, y].into_iter().flatten().map(|s| low.slot_shape(s).0).max().unwrap_or(1);
            let op = SignalOp::Noise {
                scale: lc.param_f32("scale", 1.0),
                octaves: lc.param_f32("octaves", 1.0).clamp(1.0, 8.0) as u32,
                amplitude: lc.param_f32("amplitude", 1.0),
                offset: lc.param_f32("offset", 0.0),
                seed: lc.seed(),
                has_x: x.is_some(),
                has_y: y.is_some(),
                has_time: time.is_some(),
            };
            low.emit(OpKind::Signal(op), inputs, n, 1, ph, &lc.node.id, lc.out_port());
        }
        "wander" => {
            let op = SignalOp::Wander {
                radius: lc.param_f32("radius", 0.5),
                speed: lc.param_f32("speed", 0.25),
                smoothness: lc.param_f32("smoothness", 2.0),
                seed: lc.seed(),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, lc.out_port());
        }
        "sweep" => {
            let op = SignalOp::Sweep {
                angle_deg: lc.param_f32("angle", 0.0),
                range: lc.param_f32("range", 1.0),
                speed: lc.param_f32("speed", 0.5),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, lc.out_port());
        }
        "circle" => {
            let op = SignalOp::Circle {
                radius: lc.param_f32("radius", 1.0),
                speed: lc.param_f32("speed", 0.25),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, lc.out_port());
        }
        "figure_8" => {
            let op = SignalOp::Figure8 {
                width: lc.param_f32("width", 1.0),
                height: lc.param_f32("height", 0.5),
                speed: lc.param_f32("speed", 0.25),
            };
            low.emit(OpKind::Signal(op), vec![], 1, 2, ph, &lc.node.id, lc.out_port());
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
            low.emit(OpKind::Signal(op), vec![input], n, c, ph, &lc.node.id, lc.out_port());
        }
        // beat_envelope is the older composite of `beat_pulses` + `adsr`: bake the
        // grid pulses at compile and emit the `Adsr` primitive (do NOT special-case
        // it as its own op — adsr is the primitive, beat_envelope the composition).
        "beat_envelope" => {
            let subdivision = lc.const_input("subdivision", 1.0);
            let offset = lc.const_input("offset", 0.0);
            let only_downbeats = lc.param_bool("only_downbeats", false);
            let beat_step_beats = if subdivision.abs() < 1e-3 { 1.0 } else { (1.0 / subdivision).abs() };
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
                bpm: 120.0, // fallback only (fit_to_gap derives the span from pulses)
            };
            let pulse_starts = lc.pulses(subdivision, offset, only_downbeats);
            low.emit(OpKind::Signal(SignalOp::Adsr { pulse_starts, params }), vec![], 1, 1, ph, &lc.node.id, lc.out_port());
        }
        "beat_pulses" => {
            let subdivision = lc.const_input("subdivision", 1.0);
            let offset = lc.const_input("offset", 0.0);
            let only_downbeats = lc.param_bool("only_downbeats", false);
            let op = SignalOp::BeatPulses { subdivision, offset, only_downbeats, tol: 0.05 };
            low.emit(OpKind::Signal(op), vec![], 1, 1, ph, &lc.node.id, lc.out_port());
        }
        // Frozen reductions: register a global (min,max) over the input; the
        // compiler's stat pass fills ctx.frozen[stat_idx..]. Output shape follows input.
        "normalize" => {
            let input = lc.require(low, "in")?;
            let (n, c) = low.slot_shape(input);
            let stat_idx = low.alloc_frozen(input);
            low.emit(OpKind::Signal(SignalOp::Normalize { stat_idx }), vec![input], n, c, ph, &lc.node.id, lc.out_port());
        }
        "invert" => {
            let input = lc.require(low, "in")?;
            let (n, c) = low.slot_shape(input);
            let stat_idx = low.alloc_frozen(input);
            low.emit(OpKind::Signal(SignalOp::Invert { stat_idx }), vec![input], n, c, ph, &lc.node.id, lc.out_port());
        }
        // The ADSR primitive: shape an events stream into an envelope. The event
        // onset times come from the upstream events node (beat_pulses → grid
        // pulses, drum_events → onsets), baked at compile. Unrecognized source →
        // unlowered (UnknownNode / SKIP).
        "adsr" => {
            let Some(pulse_starts) = lc.event_pulses("events_in") else {
                return Err(CompileError::UnknownNode { id: lc.node.id.clone(), type_id: "adsr".to_string() });
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
                fit_to_gap: lc.param_f32("fit_to_gap", 1.0) > 0.5,
                length_beats: lc.param_f32("length_beats", 1.0),
                bpm: 120.0,
            };
            low.emit(OpKind::Signal(SignalOp::Adsr { pulse_starts, params }), vec![], 1, 1, ph, &lc.node.id, lc.out_port());
        }
        // v1: identity over the `in` cone. True per-primitive time-shift is C-core
        // (re-eval the upstream cone at `times - delay`); the delay input is small
        // in practice, so identity is a rough match.
        "time_delay" => {
            let input = lc.require(low, "in")?;
            let (n, c) = low.slot_shape(input);
            low.emit(OpKind::Signal(SignalOp::TimeDelay { delay: 0.0 }), vec![input], n, c, ph, &lc.node.id, lc.out_port());
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
