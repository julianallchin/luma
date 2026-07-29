//! Compiled lighting evaluator (eval IR). See `docs/eval-ir.md`.
//!
//! Schema lives here (`OpKind`, `Op`, `Plan`); kernels live in `ops/<category>.rs`
//! (PyTorch `native/` style). The whole API is [`eval`]: run a [`Plan`] over a
//! `times` axis. `times.len() == 1` is a realtime frame; a dense grid is a
//! bake/export. Authoring (`models::node_graph`) is unchanged.

pub mod compile;
pub mod composite;
pub mod context;
pub mod graph_run;
pub mod ops;
pub mod scene;

pub use scene::{CompiledAnnotation, Scene, Scope};

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
    /// Drum-onset times per class (`kick|snare|hat|cymbal`), from the track's
    /// detected onsets. Consumed at compile by `drum_events`-fed `adsr` /
    /// `random_select_mask` (baked into `pulse_starts`, like beat-grid pulses).
    pub drum_onsets: std::collections::HashMap<String, Vec<f32>>,
    /// Detected chord sections `(start, end, root_pitch_class)` over absolute time
    /// (`root` is `0..11`, `None` = no chord). Consumed by `harmony_analysis`,
    /// which emits a one-hot 12-channel chroma signal per frame.
    pub chord_sections: Vec<(f32, f32, Option<u8>)>,
    /// Frozen scalars (e.g. normalize `[min,max]` pairs), referenced by op params.
    pub frozen: Vec<f32>,
    /// The annotation's absolute `[start, end]` time span. Span-relative temporal
    /// ops (`ramp_between`, ADSR-over-span) compute progress as
    /// `(t - start)/(end - start)` — a pure function of absolute time, so they are
    /// seek-safe and correct for single-frame (`t=1`) decode. (Per-segment in C5.)
    pub span: (f32, f32),
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

/// Compositing blend modes. The canonical set (9 modes incl. `Value` /
/// `Screen` / `Lighten` / `Subtract`) lives in the authoring model and is shared
/// with scores, MIDI cues, and the (now-deleted) legacy compositor; eval
/// re-exports it so the whole render path speaks one enum. The actual blend math
/// is in [`composite`].
pub use crate::models::node_graph::BlendMode;

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
    Blend {
        mode: BlendMode,
        z: i64,
    },
}

#[derive(Clone, Debug)]
pub struct Op {
    pub kind: OpKind,
    pub inputs: Vec<SlotId>,
    pub out: SlotId,
    pub phase: Phase,
}

/// A graph-editor preview tap, compiled from a `view_*` sink node. Surfaced by
/// [`eval_views`] as a `Signal` the editor renders; ignored by the render loop.
#[derive(Clone, Debug)]
pub enum ViewTap {
    /// Tap a slot's full `(n, t, c)` region (`view_signal` / `view_uv`).
    Slot(SlotId),
    /// Absolute event times baked at compile (`view_events` fed by
    /// `beat_pulses` / `drum_events`), rasterized to 0/1 pulses over `times`.
    Events(Vec<f32>),
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
    /// Channel names per slot, parallel to `slots` (`slot_channels[i].len() ==
    /// slots[i].c`). Recorded by the lowerer that produced the slot, so a view tap
    /// on *any* slot can say what its `c` axis means — the compiler is the only
    /// place that knowledge exists. Metadata only: no kernel reads it.
    pub slot_channels: Vec<Vec<String>>,
    /// Primitive (fixture-head) count — the `n` axis.
    pub n: u32,
    /// Output keys, one per primitive (`"fixture-uuid"` or `"…:head"`).
    pub primitive_ids: Vec<String>,
    pub outputs: OutputBinding,
    /// Per-track resident data read by kernels (geometry, beats, audio, stats).
    pub ctx: ResidentContext,
    /// Baked output of the prologue (t-invariant) ops: `(slot, per-primitive n*c
    /// values)`. Computed once by [`bake_prologue`] at compile so the geometry /
    /// circle-fit / palette ops don't rerun every frame. When non-empty, `eval`
    /// copies these into their slots and runs only the `Kernel`-phase ops.
    pub prologue_baked: Vec<(SlotId, Vec<f32>)>,
    /// Graph-editor preview taps: `view_*` node id -> what to surface. Only
    /// `run_graph` (the editor) reads these, via [`eval_views`].
    pub views: Vec<(String, ViewTap)>,
}

impl Plan {
    /// Channel names for a view tap's `c` axis. Slot taps read the producing
    /// slot's labels; an `Events` tap is a single rasterized 0/1 pulse train.
    pub fn view_channels(&self, tap: &ViewTap) -> Vec<String> {
        match tap {
            ViewTap::Slot(id) => self.slot_channels[*id as usize].clone(),
            ViewTap::Events(_) => vec!["events".to_string()],
        }
    }
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
    run_plan(plan, times, scratch);
    assemble(plan, times, scratch)
}

/// Lay out the arena and run every op into `scratch` (no output assembly). When
/// the plan has a baked prologue, its t-invariant slots are filled from the cache
/// and the `Prologue`-phase ops are skipped (their expensive geometry/circle-fit
/// work was done once at compile, not per frame).
fn run_plan(plan: &Plan, times: &[f32], scratch: &mut Arena) {
    let t = times.len();
    scratch.offsets.clear();
    let mut total = 0usize;
    for spec in &plan.slots {
        scratch.offsets.push(total);
        total += slot_len(*spec, t);
    }
    scratch.buf.clear();
    scratch.buf.resize(total, 0.0);

    // Fill baked prologue slots, broadcasting the per-primitive value over t.
    for (id, vals) in &plan.prologue_baked {
        let i = *id as usize;
        let spec = plan.slots[i];
        let off = scratch.offsets[i];
        let (n, c) = (spec.n as usize, spec.c as usize);
        for pi in 0..n {
            let src = &vals[pi * c..pi * c + c];
            for k in 0..t {
                let dst = off + pi * t * c + k * c;
                scratch.buf[dst..dst + c].copy_from_slice(src);
            }
        }
    }

    let skip_prologue = !plan.prologue_baked.is_empty();
    for op in &plan.ops {
        if skip_prologue && op.phase == Phase::Prologue {
            continue;
        }
        run_op(op, plan, times, scratch);
    }
}

/// Compute the prologue (t-invariant) ops once and cache them on the plan, so the
/// per-frame eval can skip them. Call at compile, after the frozen-stat pass.
pub fn bake_prologue(plan: &mut Plan) {
    let prologue_slots: Vec<SlotId> = plan
        .ops
        .iter()
        .filter(|o| o.phase == Phase::Prologue)
        .map(|o| o.out)
        .collect();
    if prologue_slots.is_empty() {
        return;
    }
    // Run the full plan once at a single time (prologue output is t-invariant, so
    // any t works) — `prologue_baked` is still empty here, so all ops run.
    let times = [plan.ctx.span.0];
    let mut scratch = Arena::default();
    run_plan(plan, &times, &mut scratch);

    let mut baked = Vec::with_capacity(prologue_slots.len());
    for id in prologue_slots {
        let i = id as usize;
        let spec = plan.slots[i];
        let off = scratch.offsets[i];
        let (n, c) = (spec.n as usize, spec.c as usize);
        let mut v = vec![0.0f32; n * c];
        for pi in 0..n {
            for ch in 0..c {
                // slot layout is [pi][k][ch] with k=0 (t-invariant).
                v[pi * c + ch] = scratch.buf[off + pi * 1 * c + ch];
            }
        }
        baked.push((id, v));
    }
    plan.prologue_baked = baked;
}

/// Run the plan over `times` and return the (min, max) over all elements of each
/// slot in `slot_ids`. Used by the compiler's frozen-stat pass (Normalize/Invert
/// global stats — eval-mode batchnorm). Non-finite values are skipped; an
/// all-non-finite slot yields `(0.0, 0.0)`.
pub fn slot_stats(
    plan: &Plan,
    times: &[f32],
    slot_ids: &[SlotId],
    scratch: &mut Arena,
) -> Vec<(f32, f32)> {
    let t = times.len();
    run_plan(plan, times, scratch);
    slot_ids
        .iter()
        .map(|&id| {
            let i = id as usize;
            let off = scratch.offsets[i];
            let len = slot_len(plan.slots[i], t);
            let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in &scratch.buf[off..off + len] {
                if v.is_finite() {
                    mn = mn.min(v);
                    mx = mx.max(v);
                }
            }
            if mn.is_finite() {
                (mn, mx)
            } else {
                (0.0, 0.0)
            }
        })
        .collect()
}

/// Evaluate `plan` over `times` and extract the `view_*` preview taps as
/// editor-renderable `Signal`s (`n*t*c` row-major `[i][k][ch]` — the slot layout
/// is already the wire format). Event taps are rasterized to 0/1 pulses on the
/// `times` grid, matching the legacy `view_events` rendering.
pub fn eval_views(
    plan: &Plan,
    times: &[f32],
    scratch: &mut Arena,
) -> HashMap<String, crate::models::node_graph::Signal> {
    use crate::models::node_graph::Signal;
    if plan.views.is_empty() {
        return HashMap::new();
    }
    run_plan(plan, times, scratch);
    let t = times.len();
    let (s0, s1) = plan.ctx.span;
    let dur = (s1 - s0).max(1e-3);
    plan.views
        .iter()
        .map(|(node_id, tap)| {
            let signal = match tap {
                ViewTap::Slot(id) => {
                    let i = *id as usize;
                    let spec = plan.slots[i];
                    let off = scratch.offsets[i];
                    let len = slot_len(spec, t);
                    Signal {
                        n: spec.n as usize,
                        t,
                        c: spec.c as usize,
                        data: scratch.buf[off..off + len].to_vec(),
                    }
                }
                ViewTap::Events(events) => {
                    let mut data = vec![0.0f32; t];
                    for ev in events {
                        let rel = (ev - s0) / dur;
                        if (0.0..=1.0).contains(&rel) {
                            let bin = ((rel * t as f32).floor() as usize).min(t - 1);
                            data[bin] = 1.0;
                        }
                    }
                    Signal {
                        n: 1,
                        t,
                        c: 1,
                        data,
                    }
                }
            };
            (node_id.clone(), signal)
        })
        .collect()
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
            (
                scratch.offsets[i],
                slot_len(plan.slots[i], t),
                plan.slots[i],
            )
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
fn slot_at(
    slot: SlotId,
    plan: &Plan,
    scratch: &Arena,
    t: usize,
    i: usize,
    k: usize,
    ch: usize,
) -> f32 {
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
            // Unset speed defaults to 1.0 (fast/unfrozen) to match legacy render_frame.
            let speed = plan
                .outputs
                .speed
                .map(|s| slot_at(s, plan, scratch, t, i, k, 0))
                .unwrap_or(1.0);
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
            slot_channels: vec![vec!["value".into()]; 3],
            n: 4,
            primitive_ids: vec!["p0".into(), "p1".into(), "p2".into(), "p3".into()],
            outputs: OutputBinding {
                dimmer: Some(2),
                ..Default::default()
            },
            ctx: ResidentContext::default(),
            prologue_baked: Vec::new(),
            views: Vec::new(),
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

    /// A selection resolving to ZERO fixtures (no venue / unmatched expression)
    /// must evaluate to an empty frame, not panic: an n=0 per-primitive slot
    /// feeding an n=1 op (config math over `get_attribute`-style data) reads 0.0.
    /// Regression: live `run_graph` panicked at `InputView::at` (len 0, index 0).
    #[test]
    fn zero_primitive_selection_does_not_panic() {
        let plan = Plan {
            ops: vec![
                Op {
                    kind: OpKind::Spatial(SpatialOp::NormalizedIndex),
                    inputs: vec![],
                    out: 0,
                    phase: Phase::Kernel,
                },
                Op {
                    kind: OpKind::Math(MathOp::Scalar(1.0)),
                    inputs: vec![],
                    out: 1,
                    phase: Phase::Kernel,
                },
                Op {
                    kind: OpKind::Math(MathOp::Binary(BinOp::Mul)),
                    inputs: vec![0, 1],
                    out: 2,
                    phase: Phase::Kernel,
                },
            ],
            slots: vec![
                SlotSpec { n: 0, c: 1 },
                SlotSpec { n: 1, c: 1 },
                SlotSpec { n: 1, c: 1 },
            ],
            slot_channels: vec![vec!["value".into()]; 3],
            n: 0,
            primitive_ids: vec![],
            outputs: OutputBinding {
                dimmer: Some(2),
                ..Default::default()
            },
            ctx: ResidentContext::default(),
            prologue_baked: Vec::new(),
            views: vec![("v".into(), ViewTap::Slot(2))],
        };

        let mut arena = Arena::default();
        let frames = eval(&plan, &[0.0], &mut arena);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].primitives.is_empty());

        let views = eval_views(&plan, &[0.0, 0.5], &mut arena);
        assert_eq!(views["v"].t, 2);
        assert!(views["v"].data.iter().all(|v| v.is_finite()));
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
            slot_channels: vec![vec!["value".into()]],
            n: 1,
            primitive_ids: vec!["p0".into()],
            outputs: OutputBinding {
                dimmer: Some(0),
                ..Default::default()
            },
            ctx: ResidentContext::default(),
            prologue_baked: Vec::new(),
            views: Vec::new(),
        };

        let times = [0.0, 0.25, 0.5];
        let mut arena = Arena::default();
        let batch = eval(&plan, &times, &mut arena);
        assert!((batch[0].primitives["p0"].dimmer - 0.0).abs() < 1e-5);
        assert!((batch[1].primitives["p0"].dimmer - 1.0).abs() < 1e-5);
        assert!((batch[2].primitives["p0"].dimmer - 0.0).abs() < 1e-5);

        let single = eval(&plan, &[0.25], &mut arena);
        assert!(
            (single[0].primitives["p0"].dimmer - batch[1].primitives["p0"].dimmer).abs() < 1e-6
        );
    }
}
