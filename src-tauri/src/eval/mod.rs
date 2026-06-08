//! Compiled lighting evaluator (eval IR). See `docs/eval-ir.md`.
//!
//! Schema lives here (`OpKind`, `Op`, `Plan`); kernels live in `ops/<category>.rs`
//! (PyTorch `native/` style). The whole API is [`eval`]: run a [`Plan`] over a
//! `times` axis. `times.len() == 1` is a realtime frame; a dense grid is a
//! bake/export. Authoring (`models::node_graph`) is unchanged.

pub mod ops;

use crate::models::universe::{PrimitiveState, UniverseState};
use ops::{InputView, KernelCtx};
use std::collections::HashMap;

pub type SlotId = u32;

/// Decoded audio resident for the track, windowed by ABSOLUTE time (never
/// streaming → seek-safe). Mono samples.
#[derive(Clone, Debug, Default)]
pub struct ResidentAudio {
    pub samples: std::sync::Arc<Vec<f32>>,
    pub sample_rate: u32,
}

/// Per-`(track, venue)` data that ops read but don't compute: fixture geometry,
/// the beat grid, resident audio, and compiler-frozen scalars (e.g. `Normalize`
/// global min/max, indexed by op). Built once at compile; borrowed by every
/// kernel via [`ops::KernelCtx`].
#[derive(Clone, Debug, Default)]
pub struct ResidentContext {
    /// Per-primitive world position `[x, y, z]`, length `n` (spatial ops).
    pub positions: Vec<[f32; 3]>,
    /// Beat grid for beat-synced generators.
    pub beat_grid: Option<crate::models::node_graph::BeatGrid>,
    /// Resident decoded audio for audio ops.
    pub audio: Option<ResidentAudio>,
    /// Per-stem decoded audio (key = `drums|bass|vocals|other`), same timeline as
    /// `audio`. Consumed by `StemSplit`; populated by the compiler from the stem cache.
    pub stems: std::collections::HashMap<String, ResidentAudio>,
    /// Per-primitive fixture attributes (key = attribute name, value length `n`).
    /// Consumed by `get_attribute`; populated by the compiler from fixture defs.
    pub attributes: std::collections::HashMap<String, Vec<f32>>,
    /// Frozen scalars (e.g. normalize `[min,max]` pairs), referenced by op params.
    pub frozen: Vec<f32>,
}

/// Shape of a slot's value region. `t` (time-axis length) is supplied at eval
/// time, so a slot holds `n * t * c` contiguous `f32`s (row-major `[i][k][ch]`).
#[derive(Clone, Copy, Debug)]
pub struct SlotSpec {
    pub n: u32,
    pub c: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// t-invariant; computed once at load (geometry, palettes, frozen stats).
    Prologue,
    /// time-varying; computed per `eval`.
    Kernel,
}

/// The frozen output capability set (authoring boundary — do not extend without
/// touching the editor + the apply nodes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Color,
    Dimmer,
    Position,
    Strobe,
    Speed,
}

/// Compositing blend modes for cross-annotation `Blend` ops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Replace,
    Add,
    Multiply,
    Max,
    Min,
}

/// Op schema. Each category variant wraps a sub-enum defined in its `ops/` file;
/// `Blend` is compiler-injected compositing. Closed enum → exhaustive `match`,
/// jump-table dispatch, and a backend-agnostic schema (the GPU door).
#[derive(Clone, Debug)]
pub enum OpKind {
    Math(ops::math::MathOp),
    Spatial(ops::spatial::SpatialOp),
    Signal(ops::signals::SignalOp),
    Color(ops::color::ColorOp),
    SelectApply(ops::select_apply::SelectApplyOp),
    Audio(ops::audio::AudioOp),
    /// Compiler-injected: combine two capability streams by z-order (deterministic).
    Blend { mode: BlendMode, z: i64 },
}

#[derive(Clone, Debug)]
pub struct Op {
    pub kind: OpKind,
    pub inputs: Vec<SlotId>,
    pub out: SlotId,
    pub phase: Phase,
}

/// Capability -> the slot whose value drives it (resolved at compile from `Apply`
/// sinks). `None` => capability uses its default.
#[derive(Clone, Debug, Default)]
pub struct OutputBinding {
    pub dimmer: Option<SlotId>,
    pub color: Option<SlotId>,
    pub position: Option<SlotId>,
    pub strobe: Option<SlotId>,
    pub speed: Option<SlotId>,
}

/// A compiled, evaluable lighting program for one `(track, venue)`.
///
/// v1 spine: a flat op list + slot table. The compiler (agent C) produces this
/// from `Vec<NodeInstance>`; segments / prologue-base / resident context land as
/// those pieces come online (see `docs/eval-ir.md`).
#[derive(Clone, Debug)]
pub struct Plan {
    /// Topologically ordered ops.
    pub ops: Vec<Op>,
    /// Slot shapes, indexed by `SlotId`.
    pub slots: Vec<SlotSpec>,
    /// Primitive (fixture-head) count — the `n` axis.
    pub n: u32,
    /// Output keys, one per primitive (`"fixture-uuid"` or `"…:head"`).
    pub primitive_ids: Vec<String>,
    pub outputs: OutputBinding,
    /// Per-track resident data read by kernels (geometry, beats, audio, stats).
    pub ctx: ResidentContext,
}

/// Reusable scratch. Held across frames so the hot path stays warm (v1 still
/// `resize`s + kernels return owned buffers; true zero-alloc is a later pass).
#[derive(Default)]
pub struct Arena {
    buf: Vec<f32>,
    offsets: Vec<usize>,
}

#[inline]
fn slot_len(spec: SlotSpec, t: usize) -> usize {
    spec.n as usize * t * spec.c as usize
}

/// Evaluate `plan` over `times`, one [`UniverseState`] per time sample.
/// `times.len() == 1` collapses to a single realtime frame.
pub fn eval(plan: &Plan, times: &[f32], scratch: &mut Arena) -> Vec<UniverseState> {
    let t = times.len();

    // Lay out the arena: prefix-sum of slot sizes for this `t`.
    scratch.offsets.clear();
    let mut total = 0usize;
    for spec in &plan.slots {
        scratch.offsets.push(total);
        total += slot_len(*spec, t);
    }
    scratch.buf.clear();
    scratch.buf.resize(total, 0.0);

    for op in &plan.ops {
        run_op(op, plan, times, scratch);
    }

    assemble(plan, times, scratch)
}

fn run_op(op: &Op, plan: &Plan, times: &[f32], scratch: &mut Arena) {
    let t = times.len();
    let out_id = op.out as usize;
    let out_off = scratch.offsets[out_id];
    let out_spec = plan.slots[out_id];

    // Read input region metadata before borrowing the buffer.
    let in_meta: Vec<(usize, usize, SlotSpec)> = op
        .inputs
        .iter()
        .map(|&id| {
            let i = id as usize;
            (scratch.offsets[i], slot_len(plan.slots[i], t), plan.slots[i])
        })
        .collect();

    let out_buf = {
        let views: Vec<InputView> = in_meta
            .iter()
            .map(|&(off, len, spec)| InputView {
                data: &scratch.buf[off..off + len],
                spec,
            })
            .collect();
        let ctx = KernelCtx {
            inputs: &views,
            out_spec,
            times,
            ctx: &plan.ctx,
        };
        dispatch(&op.kind, &ctx)
    };

    scratch.buf[out_off..out_off + out_buf.len()].copy_from_slice(&out_buf);
}

fn dispatch(kind: &OpKind, ctx: &KernelCtx) -> Vec<f32> {
    match kind {
        OpKind::Math(o) => ops::math::run_math(o, ctx),
        OpKind::Spatial(o) => ops::spatial::run_spatial(o, ctx),
        OpKind::Signal(o) => ops::signals::run_signals(o, ctx),
        OpKind::Color(o) => ops::color::run_color(o, ctx),
        OpKind::SelectApply(o) => ops::select_apply::run_select_apply(o, ctx),
        OpKind::Audio(o) => ops::audio::run_audio(o, ctx),
        // TODO(compositor): real z-ordered blend over segments. Placeholder passes
        // input 0 through so single-segment plans evaluate correctly.
        OpKind::Blend { .. } => ctx.input(0).data.to_vec(),
    }
}

/// Read one channel of an output slot at `(i, k, ch)`, broadcasting `n`.
fn slot_at(slot: SlotId, plan: &Plan, scratch: &Arena, t: usize, i: usize, k: usize, ch: usize) -> f32 {
    let id = slot as usize;
    let spec = plan.slots[id];
    let off = scratch.offsets[id];
    let ni = if spec.n == 1 { 0 } else { i };
    let c = spec.c as usize;
    scratch.buf[off + ni * t * c + k * c + ch]
}

fn assemble(plan: &Plan, times: &[f32], scratch: &Arena) -> Vec<UniverseState> {
    let t = times.len();
    let mut frames = Vec::with_capacity(t);
    for k in 0..t {
        let mut primitives = HashMap::with_capacity(plan.n as usize);
        for i in 0..plan.n as usize {
            let dimmer = plan
                .outputs
                .dimmer
                .map(|s| slot_at(s, plan, scratch, t, i, k, 0))
                .unwrap_or(0.0);
            let color = plan
                .outputs
                .color
                .map(|s| {
                    [
                        slot_at(s, plan, scratch, t, i, k, 0),
                        slot_at(s, plan, scratch, t, i, k, 1),
                        slot_at(s, plan, scratch, t, i, k, 2),
                    ]
                })
                .unwrap_or([1.0, 1.0, 1.0]);
            let strobe = plan
                .outputs
                .strobe
                .map(|s| slot_at(s, plan, scratch, t, i, k, 0))
                .unwrap_or(0.0);
            let speed = plan
                .outputs
                .speed
                .map(|s| slot_at(s, plan, scratch, t, i, k, 0))
                .unwrap_or(0.0);
            let position = plan
                .outputs
                .position
                .map(|s| {
                    [
                        slot_at(s, plan, scratch, t, i, k, 0),
                        slot_at(s, plan, scratch, t, i, k, 1),
                    ]
                })
                .unwrap_or([0.0, 0.0]);
            primitives.insert(
                plan.primitive_ids[i].clone(),
                PrimitiveState {
                    dimmer,
                    color,
                    strobe,
                    position,
                    speed,
                },
            );
        }
        frames.push(UniverseState { primitives });
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::ops::math::{BinOp, MathOp};
    use super::ops::select_apply::SelectApplyOp;
    use super::ops::signals::SignalOp;
    use super::ops::spatial::SpatialOp;
    use super::*;

    /// scalar(0.5) * normalized_index -> dimmer, over n=4. Expect 0.5·i/3.
    /// Exercises broadcast + spatial + elementwise + apply + assembly.
    #[test]
    fn scalar_times_index_drives_dimmer() {
        let plan = Plan {
            ops: vec![
                Op {
                    kind: OpKind::Math(MathOp::Scalar(0.5)),
                    inputs: vec![],
                    out: 0,
                    phase: Phase::Prologue,
                },
                Op {
                    kind: OpKind::Spatial(SpatialOp::NormalizedIndex),
                    inputs: vec![],
                    out: 1,
                    phase: Phase::Prologue,
                },
                Op {
                    kind: OpKind::Math(MathOp::Binary(BinOp::Mul)),
                    inputs: vec![0, 1],
                    out: 2,
                    phase: Phase::Prologue,
                },
                Op {
                    kind: OpKind::SelectApply(SelectApplyOp::Apply(Capability::Dimmer)),
                    inputs: vec![2],
                    out: 2,
                    phase: Phase::Kernel,
                },
            ],
            slots: vec![
                SlotSpec { n: 1, c: 1 },
                SlotSpec { n: 4, c: 1 },
                SlotSpec { n: 4, c: 1 },
            ],
            n: 4,
            primitive_ids: vec!["p0".into(), "p1".into(), "p2".into(), "p3".into()],
            outputs: OutputBinding {
                dimmer: Some(2),
                ..Default::default()
            },
            ctx: ResidentContext::default(),
        };

        let mut arena = Arena::default();
        let frames = eval(&plan, &[0.0], &mut arena);
        assert_eq!(frames.len(), 1);
        let d = |id: &str| frames[0].primitives[id].dimmer;
        assert!((d("p0") - 0.0).abs() < 1e-6);
        assert!((d("p1") - 0.5 * (1.0 / 3.0)).abs() < 1e-6);
        assert!((d("p2") - 0.5 * (2.0 / 3.0)).abs() < 1e-6);
        assert!((d("p3") - 0.5).abs() < 1e-6);
    }

    /// Sine -> dimmer over a time axis, and decode (t=1) == prefill sample.
    #[test]
    fn sine_varies_over_time_and_is_pointwise() {
        let plan = Plan {
            ops: vec![
                Op {
                    kind: OpKind::Signal(SignalOp::Sine { freq: 1.0 }),
                    inputs: vec![],
                    out: 0,
                    phase: Phase::Kernel,
                },
                Op {
                    kind: OpKind::SelectApply(SelectApplyOp::Apply(Capability::Dimmer)),
                    inputs: vec![0],
                    out: 0,
                    phase: Phase::Kernel,
                },
            ],
            slots: vec![SlotSpec { n: 1, c: 1 }],
            n: 1,
            primitive_ids: vec!["p0".into()],
            outputs: OutputBinding {
                dimmer: Some(0),
                ..Default::default()
            },
            ctx: ResidentContext::default(),
        };

        let times = [0.0, 0.25, 0.5];
        let mut arena = Arena::default();
        let batch = eval(&plan, &times, &mut arena);
        assert!((batch[0].primitives["p0"].dimmer - 0.0).abs() < 1e-5);
        assert!((batch[1].primitives["p0"].dimmer - 1.0).abs() < 1e-5);
        assert!((batch[2].primitives["p0"].dimmer - 0.0).abs() < 1e-5);

        let single = eval(&plan, &[0.25], &mut arena);
        assert!((single[0].primitives["p0"].dimmer - batch[1].primitives["p0"].dimmer).abs() < 1e-6);
    }
}
