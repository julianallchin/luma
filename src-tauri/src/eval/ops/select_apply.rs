//! Selection ops (filter/mask) + the apply sinks.
//!
//! ## Apply sinks
//! An `Apply*` op routes a signal into a capability output. The *binding* of the
//! op's out slot into [`crate::eval::OutputBinding`] is resolved by the COMPILER;
//! at runtime the kernel is (almost) identity — it copies its input through to its
//! out slot, **adapting the input's channel count to the capability's shape** so
//! `assemble()` can read it directly. The output channel contract per capability
//! (this is what the compiler must give the out slot a `SlotSpec.c` of, and which
//! `OutputBinding` field to point at the out slot):
//!
//! | op              | capability | out `c` | reads from input              |
//! |-----------------|------------|---------|-------------------------------|
//! | `ApplyColor`    | Color      | 3       | RGB: ch 0,1,2 (clamped 0..1)  |
//! | `ApplyDimmer`   | Dimmer     | 1       | ch 0                          |
//! | `ApplyMovement` | Position   | 2       | pan = ch 0, tilt = ch 1       |
//! | `ApplyPosition` | Position   | 2       | pan = ch 0, tilt = ch 1       |
//! | `ApplyStrobe`   | Strobe     | 1       | ch 0 (clamped 0..1)           |
//! | `ApplySpeed`    | Speed      | 1       | ch 0, binarized: >0.5 -> 1    |
//!
//! Channel adaptation is "take what the capability needs; if the input has fewer
//! channels than required, broadcast the last available channel; missing -> 0".
//! The kernel does NOT do the legacy movement-pyramid geometry or the color->HSV
//! dimmer split — those are graph-level concerns lowered into upstream ops by the
//! compiler. `ApplyMovement` and `ApplyPosition` are kernel-identical here (both
//! pass a 2-channel pan/tilt signal); they remain distinct variants only so the
//! compiler can lower the two legacy node types separately (movement lowers its
//! pyramid into upstream ops, position is already raw pan/tilt).
//!
//! ## Selection / mask ops (prologue-eligible, `c == 1`, `n == plan.n`)
//! `FilterSelection` and `RandomSelectMask` each emit a per-primitive 0/1 gain
//! buffer over `n` (constant across `t`). Downstream they are multiplied into a
//! signal to gate it per primitive.
//!
//! - [`SelectApplyOp::FilterSelection`] carries `keep: Vec<f32>` (length `n`, 0/1)
//!   that the COMPILER precomputes: for each primitive, whether its fixture mode
//!   exposes the requested [`Capability`]. Capability membership needs the fixture
//!   definition DB, which is *not* in `ResidentContext`, so it is resolved at
//!   compile and frozen into the op. If `keep` is empty the op treats every
//!   primitive as kept (pass-through), matching legacy "missing DB" behavior.
//! - [`SelectApplyOp::RandomSelectMask`] selects `frac` of the `n` primitives
//!   deterministically from `seed`. **Seed convention:** the compiler passes the
//!   hashed node id (splitmix64 of the `NodeInstance.id` string) as `seed`; the
//!   kernel hash-scores each primitive index against the seed and keeps the lowest
//!   `round(frac * n)` scores. This is a static (t-invariant) variant of the
//!   legacy event-driven re-roll: v1 has no event input, so the subset is fixed.
//!   The per-index scoring (`hash_combine(seed, i)`) matches legacy `selection.rs`
//!   so a given seed yields the same ranking.
//!
//! Reference impls: legacy `node_graph/nodes/selection.rs` + `apply.rs`.

use super::KernelCtx;
use crate::eval::Capability;

#[derive(Clone, Debug)]
pub enum SelectApplyOp {
    /// Generic sink kept for callers that already adapt channels upstream: route
    /// input 0 into `capability`, copying it through verbatim to the out slot.
    /// Prefer the per-capability variants below, which clamp/adapt channels.
    Apply(Capability),

    /// Color sink. Out `c == 3` (RGB), input ch 0,1,2 clamped to `0..1`.
    ApplyColor,
    /// Dimmer sink. Out `c == 1`, input ch 0.
    ApplyDimmer,
    /// Movement sink. Out `c == 2` (pan, tilt) from input ch 0,1.
    ApplyMovement,
    /// Position sink. Out `c == 2` (pan, tilt) from input ch 0,1.
    ApplyPosition,
    /// Strobe sink. Out `c == 1`, input ch 0 clamped to `0..1`.
    ApplyStrobe,
    /// Speed sink. Out `c == 1`, input ch 0 binarized (`> 0.5 -> 1`, else `0`).
    ApplySpeed,

    /// Per-primitive capability mask. `keep[i]` (0/1) precomputed by the compiler
    /// from fixture-capability data. Out `n == plan.n, c == 1`, constant over `t`.
    /// Empty `keep` => all-kept (legacy "no DB" pass-through).
    FilterSelection { keep: Vec<f32> },

    /// Event-driven random subset mask. On each event (`pulse_starts`, baked at
    /// compile from beat-grid pulses or drum onsets) re-roll a `count`-sized subset
    /// of the `n` primitives — `hash_combine(seed, event_idx)` seeds per-primitive
    /// scores, lowest `count` selected — held until the next event. `avoid_repeat`
    /// prefers primitives not in the previous selection. Out `n == plan.n, c == 1`.
    /// Mirrors legacy `selection.rs::random_select_mask`.
    RandomSelectMask {
        seed: u64,
        count: u32,
        avoid_repeat: bool,
        pulse_starts: Vec<f32>,
    },
}

/// splitmix64 finalizer — matches legacy `selection.rs::hash_combine` exactly so
/// a given seed reproduces the legacy per-index ranking.
#[inline]
fn hash_combine(seed: u64, v: u64) -> u64 {
    let mut x = seed ^ v;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Channel value from input 0 at `(i, k, ch)`, broadcasting the last available
/// channel when `ch` is past the input's width, and `0.0` when the input is empty.
#[inline]
fn in_ch(ctx: &KernelCtx, i: usize, k: usize, ch: usize, t: usize) -> f32 {
    let view = ctx.input(0);
    let c = view.spec.c as usize;
    if c == 0 {
        return 0.0;
    }
    view.at(i, k, ch.min(c - 1), t)
}

pub fn run_select_apply(op: &SelectApplyOp, ctx: &KernelCtx) -> Vec<f32> {
    let (t, n) = (ctx.t(), ctx.n());
    match op {
        // Compiler resolved the binding (Plan.outputs); pass through verbatim.
        SelectApplyOp::Apply(_) => ctx.input(0).data.to_vec(),

        SelectApplyOp::ApplyColor => {
            // Out c == 3: copy RGB, clamping to 0..1.
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..3 {
                        out[ctx.out_idx(i, k, ch)] = in_ch(ctx, i, k, ch, t).clamp(0.0, 1.0);
                    }
                }
            }
            out
        }

        SelectApplyOp::ApplyDimmer => {
            // Out c == 1: copy ch 0 through.
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    out[ctx.out_idx(i, k, 0)] = in_ch(ctx, i, k, 0, t);
                }
            }
            out
        }

        SelectApplyOp::ApplyMovement | SelectApplyOp::ApplyPosition => {
            // Out c == 2: pan = ch 0, tilt = ch 1.
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    out[ctx.out_idx(i, k, 0)] = in_ch(ctx, i, k, 0, t);
                    out[ctx.out_idx(i, k, 1)] = in_ch(ctx, i, k, 1, t);
                }
            }
            out
        }

        SelectApplyOp::ApplyStrobe => {
            // Out c == 1: ch 0 clamped to 0..1.
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    out[ctx.out_idx(i, k, 0)] = in_ch(ctx, i, k, 0, t).clamp(0.0, 1.0);
                }
            }
            out
        }

        SelectApplyOp::ApplySpeed => {
            // Out c == 1: binarize ch 0 (legacy: 0 = frozen, 1 = fast).
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    let v = in_ch(ctx, i, k, 0, t);
                    out[ctx.out_idx(i, k, 0)] = if v > 0.5 { 1.0 } else { 0.0 };
                }
            }
            out
        }

        SelectApplyOp::FilterSelection { keep } => {
            // Out n=plan.n, c=1, constant over t. Empty keep => all-kept.
            let mut out = ctx.out_buf();
            for i in 0..n {
                let g = if keep.is_empty() {
                    1.0
                } else {
                    keep.get(i).copied().unwrap_or(0.0)
                };
                for k in 0..t {
                    out[ctx.out_idx(i, k, 0)] = g;
                }
            }
            out
        }

        SelectApplyOp::RandomSelectMask {
            seed,
            count,
            avoid_repeat,
            pulse_starts,
        } => {
            // Out n=plan.n, c=1, time-varying. Re-roll the selected subset on each
            // event; held until the next event.
            let mut out = ctx.out_buf();
            if n == 0 || *count == 0 {
                return out;
            }
            let count = (*count as usize).min(n);
            let pulses = pulse_starts;

            // event_idx at time t = number of pulses at/before t. Legacy iterates
            // only the pattern's [start, end] window and starts the avoid_repeat
            // chain at the FIRST event in that window with an empty history — so we
            // anchor the chain at `event_start` (the event active at the span start),
            // not at the absolute first event. Build selections event_start..=max.
            let event_start = pulses.partition_point(|&p| p <= ctx.ctx.span.0);
            let max_event = ctx
                .times
                .iter()
                .map(|&tt| pulses.partition_point(|&p| p <= tt))
                .max()
                .unwrap_or(0)
                .max(event_start);
            // selections[ev - event_start] holds the subset for absolute event `ev`.
            let mut selections: Vec<Vec<usize>> = Vec::with_capacity(max_event - event_start + 1);
            let mut prev: Vec<usize> = Vec::new();
            for ev in event_start..=max_event {
                let sel: Vec<usize> = if ev == 0 {
                    Vec::new() // no pulse has occurred yet
                } else {
                    let step_seed = hash_combine(*seed, ev as u64);
                    // Stable sort by score → ties keep index order (matches legacy).
                    let mut scores: Vec<(usize, u64)> = (0..n)
                        .map(|i| (i, hash_combine(step_seed, i as u64)))
                        .collect();
                    scores.sort_by_key(|&(_, s)| s);
                    if *avoid_repeat && !prev.is_empty() {
                        let mut avail: Vec<(usize, u64)> = scores
                            .iter()
                            .filter(|(i, _)| !prev.contains(i))
                            .copied()
                            .collect();
                        if avail.len() < count {
                            avail.extend(scores.iter().filter(|(i, _)| prev.contains(i)).copied());
                        }
                        avail.into_iter().take(count).map(|(i, _)| i).collect()
                    } else {
                        scores.into_iter().take(count).map(|(i, _)| i).collect()
                    }
                };
                prev = sel.clone();
                selections.push(sel);
            }

            for k in 0..t {
                let ev = pulses.partition_point(|&p| p <= ctx.times[k]);
                if ev >= event_start {
                    if let Some(sel) = selections.get(ev - event_start) {
                        for &i in sel {
                            out[ctx.out_idx(i, k, 0)] = 1.0;
                        }
                    }
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::ops::InputView;
    use crate::eval::{ResidentContext, SlotSpec};

    /// Build a `KernelCtx` over one input slab and an output spec.
    fn ctx<'a>(
        input: &'a [f32],
        in_spec: SlotSpec,
        out_spec: SlotSpec,
        times: &'a [f32],
        rctx: &'a ResidentContext,
        views: &'a mut Vec<InputView<'a>>,
    ) -> KernelCtx<'a> {
        views.push(InputView {
            data: input,
            spec: in_spec,
        });
        KernelCtx {
            inputs: views,
            out_spec,
            times,
            ctx: rctx,
        }
    }

    #[test]
    fn apply_color_passes_three_channels() {
        // n=2, t=1, c=3 input. Row-major [i][k][ch].
        let input = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let in_spec = SlotSpec { n: 2, c: 3 };
        let out_spec = SlotSpec { n: 2, c: 3 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplyColor, &kc);
        assert_eq!(out, input);
    }

    #[test]
    fn apply_color_clamps() {
        let input = vec![-0.5, 0.5, 1.5];
        let in_spec = SlotSpec { n: 1, c: 3 };
        let out_spec = SlotSpec { n: 1, c: 3 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplyColor, &kc);
        assert_eq!(out, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn apply_dimmer_copies_channel_zero() {
        // c=1 input, n=3.
        let input = vec![0.2, 0.4, 0.6];
        let in_spec = SlotSpec { n: 3, c: 1 };
        let out_spec = SlotSpec { n: 3, c: 1 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplyDimmer, &kc);
        assert_eq!(out, input);
    }

    #[test]
    fn apply_position_takes_two_channels() {
        // n=1, t=2, c=2: pan/tilt over two times.
        let input = vec![10.0, 20.0, 30.0, 40.0];
        let in_spec = SlotSpec { n: 1, c: 2 };
        let out_spec = SlotSpec { n: 1, c: 2 };
        let times = [0.0, 1.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplyPosition, &kc);
        assert_eq!(out, input);
        // Movement is kernel-identical.
        let mut views2 = Vec::new();
        let kc2 = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views2);
        assert_eq!(run_select_apply(&SelectApplyOp::ApplyMovement, &kc2), input);
    }

    #[test]
    fn apply_speed_binarizes() {
        let input = vec![0.0, 0.49, 0.51, 1.0];
        let in_spec = SlotSpec { n: 4, c: 1 };
        let out_spec = SlotSpec { n: 4, c: 1 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplySpeed, &kc);
        assert_eq!(out, vec![0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn apply_strobe_clamps() {
        let input = vec![-1.0, 0.3, 2.0];
        let in_spec = SlotSpec { n: 3, c: 1 };
        let out_spec = SlotSpec { n: 3, c: 1 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplyStrobe, &kc);
        assert_eq!(out, vec![0.0, 0.3, 1.0]);
    }

    #[test]
    fn apply_adapts_fewer_channels_by_broadcast() {
        // c=1 input fed to a color sink (out c=3): last channel broadcast.
        let input = vec![0.7];
        let in_spec = SlotSpec { n: 1, c: 1 };
        let out_spec = SlotSpec { n: 1, c: 3 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::ApplyColor, &kc);
        assert_eq!(out, vec![0.7, 0.7, 0.7]);
    }

    #[test]
    fn filter_selection_gates_per_primitive() {
        let input: Vec<f32> = vec![];
        let in_spec = SlotSpec { n: 1, c: 1 };
        let out_spec = SlotSpec { n: 4, c: 1 };
        let times = [0.0, 1.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let keep = vec![1.0, 0.0, 1.0, 0.0];
        let out = run_select_apply(&SelectApplyOp::FilterSelection { keep: keep.clone() }, &kc);
        // n=4, t=2, c=1 -> each primitive's gain repeated across both times.
        assert_eq!(out, vec![1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn filter_selection_empty_keep_is_all_kept() {
        let input: Vec<f32> = vec![];
        let in_spec = SlotSpec { n: 1, c: 1 };
        let out_spec = SlotSpec { n: 3, c: 1 };
        let times = [0.0];
        let rctx = ResidentContext::default();
        let mut views = Vec::new();
        let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);

        let out = run_select_apply(&SelectApplyOp::FilterSelection { keep: vec![] }, &kc);
        assert_eq!(out, vec![1.0, 1.0, 1.0]);
    }

    fn beat_grid(bpm: f32) -> crate::models::node_graph::BeatGrid {
        crate::models::node_graph::BeatGrid {
            beats: (0..16).map(|i| i as f32 * 60.0 / bpm).collect(),
            downbeats: vec![],
            bpm,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        }
    }

    #[test]
    fn random_mask_event_driven_reproducible_and_selects_count() {
        let input: Vec<f32> = vec![];
        let in_spec = SlotSpec { n: 1, c: 1 };
        let n = 100usize;
        let out_spec = SlotSpec { n: n as u32, c: 1 };
        // Two frames in different events (pulses every 0.5s).
        let times = [0.1f32, 2.1f32];
        let pulses: Vec<f32> = (0..16).map(|i| i as f32 * 0.5).collect();
        let rctx = ResidentContext::default();
        let t = times.len();

        let run = |seed: u64, count: u32| {
            let mut views = Vec::new();
            let kc = ctx(&input, in_spec, out_spec, &times, &rctx, &mut views);
            run_select_apply(
                &SelectApplyOp::RandomSelectMask {
                    seed,
                    count,
                    avoid_repeat: true,
                    pulse_starts: pulses.clone(),
                },
                &kc,
            )
        };

        let a = run(12345, 3);
        assert_eq!(a, run(12345, 3), "same seed must reproduce");
        // Each frame selects exactly `count` = 3 primitives.
        for k in 0..t {
            let sel = (0..n).filter(|&i| a[i * t + k] == 1.0).count();
            assert_eq!(sel, 3, "frame {k} selects count");
        }
        assert_ne!(a, run(99999, 3), "different seed -> different set");
        assert!(a.iter().all(|&v| v == 0.0 || v == 1.0));
    }

    #[test]
    fn random_mask_no_events_or_zero_count_selects_nothing() {
        let input: Vec<f32> = vec![];
        let in_spec = SlotSpec { n: 1, c: 1 };
        let out_spec = SlotSpec { n: 8, c: 1 };
        let times = [1.0f32];
        let rctx = ResidentContext::default();

        // No events → nothing selected.
        let op = SelectApplyOp::RandomSelectMask {
            seed: 7,
            count: 3,
            avoid_repeat: true,
            pulse_starts: vec![],
        };
        let mut v1 = Vec::new();
        let out = run_select_apply(&op, &ctx(&input, in_spec, out_spec, &times, &rctx, &mut v1));
        assert_eq!(out.iter().filter(|&&v| v == 1.0).count(), 0);

        // count = 0 → nothing selected even with events.
        let zero = SelectApplyOp::RandomSelectMask {
            seed: 7,
            count: 0,
            avoid_repeat: true,
            pulse_starts: vec![0.0, 0.5, 1.0],
        };
        let mut v2 = Vec::new();
        let out0 = run_select_apply(
            &zero,
            &ctx(&input, in_spec, out_spec, &times, &rctx, &mut v2),
        );
        assert_eq!(out0.iter().filter(|&&v| v == 1.0).count(), 0);
    }
}
