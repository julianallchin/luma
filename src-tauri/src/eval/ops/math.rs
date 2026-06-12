//! Math / elementwise ops. Sources (constants) and pointwise unary/binary.
//! Phase follows inputs (a `Scalar` is prologue; `Binary` of a kernel input is
//! kernel). Port agent D1 extends this file. Reference impls: legacy
//! `node_graph/nodes/signals.rs` + `math` handling.
//!
//! Numeric semantics are matched to the legacy executor (`signals.rs`):
//!   - `Div` / `Mod` by zero -> `0.0` (legacy `divide` / `math`-node `modulo`).
//!   - `Mod` is the truncating remainder `a % b` (legacy `math` node), NOT the
//!     always-positive standalone `modulo` node — that one is `MathOp::Modulo`
//!     (`rem_euclid`), a unary-with-param op.
//!   - `Remap` linear-maps `[in_min,in_max] -> [out_min,out_max]` with optional
//!     input clamp, and a degenerate (|in_max-in_min| < 1e-6) denom guard.
//!   - `Threshold` binarizes: `x >= cutoff ? 1 : 0`.
//!   - `RampBetween` is a span-relative temporal generator: `start + (end-start)
//!     * progress`, `progress = (t_abs - span_start)/(span_end - span_start)`
//!     (pure fn of absolute time → seek-safe, correct for t=1 decode).

use super::KernelCtx;

#[derive(Clone, Copy, Debug)]
pub enum UnaryOp {
    Abs,
    Floor,
    Ceil,
    Round,
    Neg,
    Invert, // 1 - x (pointwise). NOTE: legacy standalone `invert` node reflects
            // around the observed series midpoint — that is a *reduction*, not a
            // pointwise op, and is deferred to the reduction category.
}

#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    /// Truncating remainder `a % b` (div-by-zero -> 0). Matches the legacy
    /// `math` node `modulo`.
    Mod,
    /// `(a - b).abs()`. Matches the legacy `math` node `abs_diff`.
    AbsDiff,
    /// Shortest distance on the unit circle between two `0..1` phases:
    /// `let d = (a - b).abs().rem_euclid(1.0); d.min(1.0 - d)`. Matches the legacy
    /// `math` node `circular_distance` (deferred here by both math+spatial agents).
    CircularDistance,
}

#[derive(Clone, Debug)]
pub enum MathOp {
    /// Constant source. Output `n=1, c=1`.
    Scalar(f32),
    /// Elementwise unary over input 0.
    Unary(UnaryOp),
    /// Elementwise binary over inputs 0,1; broadcasts `n` (1 -> N).
    Binary(BinOp),
    /// Binarize input 0: `x >= cutoff ? 1.0 : 0.0`.
    Threshold { cutoff: f32 },
    /// Always-positive wrap of input 0 into `[0, divisor)` — the standalone
    /// `modulo` node (`v.rem_euclid(divisor)`, i.e. `((v % d) + d) % d`). Unlike
    /// `BinOp::Mod` (truncating remainder, which is negative for negative inputs),
    /// this keeps looping animations in range. `divisor <= 0` -> `0.0`.
    Modulo { divisor: f32 },
    /// Linear remap of input 0 from `[in_min, in_max]` to `[out_min, out_max]`,
    /// optionally clamping the input to the (ordered) input range first.
    Remap {
        in_min: f32,
        in_max: f32,
        out_min: f32,
        out_max: f32,
        clamp: bool,
    },
    /// Temporal linear interpolation across the time axis: at time index `k`,
    /// `start + (end - start) * (k / t)`. Inputs 0 = start, 1 = end. Output c=1.
    RampBetween,
}

#[inline]
fn unary(op: UnaryOp, v: f32) -> f32 {
    match op {
        UnaryOp::Abs => v.abs(),
        UnaryOp::Floor => v.floor(),
        UnaryOp::Ceil => v.ceil(),
        UnaryOp::Round => v.round(),
        UnaryOp::Neg => -v,
        UnaryOp::Invert => 1.0 - v,
    }
}

#[inline]
fn binary(op: BinOp, a: f32, b: f32) -> f32 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => {
            if b == 0.0 {
                0.0
            } else {
                a / b
            }
        }
        BinOp::Min => a.min(b),
        BinOp::Max => a.max(b),
        BinOp::Mod => {
            if b == 0.0 {
                0.0
            } else {
                a % b
            }
        }
        BinOp::AbsDiff => (a - b).abs(),
        BinOp::CircularDistance => {
            let d = (a - b).abs().rem_euclid(1.0);
            d.min(1.0 - d)
        }
    }
}

pub fn run_math(op: &MathOp, ctx: &KernelCtx) -> Vec<f32> {
    let (t, n, c) = (ctx.t(), ctx.n(), ctx.c());
    let mut out = ctx.out_buf();
    match op {
        MathOp::Scalar(v) => out.iter_mut().for_each(|x| *x = *v),
        MathOp::Unary(u) => {
            let a = ctx.input(0);
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = unary(*u, a.at(i, k, ch, t));
                    }
                }
            }
        }
        MathOp::Binary(b) => {
            let (x, y) = (ctx.input(0), ctx.input(1));
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] =
                            binary(*b, x.at(i, k, ch, t), y.at(i, k, ch, t));
                    }
                }
            }
        }
        MathOp::Threshold { cutoff } => {
            let a = ctx.input(0);
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        let v = a.at(i, k, ch, t);
                        out[ctx.out_idx(i, k, ch)] = if v >= *cutoff { 1.0 } else { 0.0 };
                    }
                }
            }
        }
        MathOp::Modulo { divisor } => {
            let a = ctx.input(0);
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        let v = a.at(i, k, ch, t);
                        out[ctx.out_idx(i, k, ch)] = if *divisor <= 0.0 {
                            0.0
                        } else {
                            v.rem_euclid(*divisor)
                        };
                    }
                }
            }
        }
        MathOp::Remap {
            in_min,
            in_max,
            out_min,
            out_max,
            clamp,
        } => {
            let a = ctx.input(0);
            let denom = in_max - in_min;
            let safe_denom = if denom.abs() < 1e-6 { 1.0 } else { denom };
            let (lo, hi) = (in_min.min(*in_max), in_min.max(*in_max));
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        let v0 = a.at(i, k, ch, t);
                        let v = if *clamp { v0.clamp(lo, hi) } else { v0 };
                        let u = (v - in_min) / safe_denom;
                        out[ctx.out_idx(i, k, ch)] = out_min + u * (out_max - out_min);
                    }
                }
            }
        }
        MathOp::RampBetween => {
            // Span-relative: progress = (t_abs - span_start)/(span_end - span_start),
            // a pure function of absolute time so a single-frame (t=1) decode is
            // correct and seeking is deterministic. Inputs 0=start, 1=end values.
            let (start, end) = (ctx.input(0), ctx.input(1));
            let (s0, s1) = ctx.ctx.span;
            let dur = (s1 - s0).max(1e-6);
            for i in 0..n {
                for k in 0..t {
                    let progress = ((ctx.times[k] - s0) / dur).clamp(0.0, 1.0);
                    for ch in 0..c {
                        let s = start.at(i, k, ch, t);
                        let e = end.at(i, k, ch, t);
                        out[ctx.out_idx(i, k, ch)] = s + (e - s) * progress;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::ops::InputView;
    use crate::eval::{ResidentContext, SlotSpec};

    /// Build an `InputView` from raw `[n,t,c]` row-major data.
    fn view(data: Vec<f32>, n: u32, c: u32) -> (Vec<f32>, SlotSpec) {
        (data, SlotSpec { n, c })
    }

    /// Run a `MathOp` against the given input views, producing an `n*t*c` buffer.
    fn run(
        op: &MathOp,
        out_n: u32,
        out_c: u32,
        t: usize,
        inputs: &[(Vec<f32>, SlotSpec)],
    ) -> Vec<f32> {
        let ctx_resident = ResidentContext::default();
        let views: Vec<InputView> = inputs
            .iter()
            .map(|(d, s)| InputView { data: d, spec: *s })
            .collect();
        let times = vec![0.0f32; t];
        let ctx = KernelCtx {
            inputs: &views,
            out_spec: SlotSpec { n: out_n, c: out_c },
            times: &times,
            ctx: &ctx_resident,
        };
        run_math(op, &ctx)
    }

    /// Run with explicit absolute `times` + annotation `span` (for span-relative
    /// temporal ops like `RampBetween`).
    fn run_spanned(
        op: &MathOp,
        out_n: u32,
        out_c: u32,
        times: &[f32],
        span: (f32, f32),
        inputs: &[(Vec<f32>, SlotSpec)],
    ) -> Vec<f32> {
        let ctx_resident = ResidentContext {
            span,
            ..Default::default()
        };
        let views: Vec<InputView> = inputs
            .iter()
            .map(|(d, s)| InputView { data: d, spec: *s })
            .collect();
        let ctx = KernelCtx {
            inputs: &views,
            out_spec: SlotSpec { n: out_n, c: out_c },
            times,
            ctx: &ctx_resident,
        };
        run_math(op, &ctx)
    }

    fn approx(a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len(), "length mismatch: {a:?} vs {b:?}");
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 1e-6, "got {a:?} expected {b:?}");
        }
    }

    #[test]
    fn scalar_fills() {
        let out = run(&MathOp::Scalar(0.5), 1, 1, 1, &[]);
        approx(&out, &[0.5]);
    }

    #[test]
    fn binary_add_sub_mul() {
        let a = view(vec![1.0, 2.0, 3.0], 1, 3);
        let b = view(vec![10.0, 20.0, 30.0], 1, 3);
        approx(
            &run(
                &MathOp::Binary(BinOp::Add),
                1,
                3,
                1,
                &[a.clone(), b.clone()],
            ),
            &[11.0, 22.0, 33.0],
        );
        approx(
            &run(
                &MathOp::Binary(BinOp::Sub),
                1,
                3,
                1,
                &[a.clone(), b.clone()],
            ),
            &[-9.0, -18.0, -27.0],
        );
        approx(
            &run(&MathOp::Binary(BinOp::Mul), 1, 3, 1, &[a, b]),
            &[10.0, 40.0, 90.0],
        );
    }

    #[test]
    fn binary_div_zero_is_zero() {
        let a = view(vec![6.0, 5.0], 1, 2);
        let b = view(vec![2.0, 0.0], 1, 2);
        // 6/2 = 3, 5/0 -> 0
        approx(
            &run(&MathOp::Binary(BinOp::Div), 1, 2, 1, &[a, b]),
            &[3.0, 0.0],
        );
    }

    #[test]
    fn binary_min_max() {
        let a = view(vec![1.0, 9.0], 1, 2);
        let b = view(vec![4.0, 2.0], 1, 2);
        approx(
            &run(
                &MathOp::Binary(BinOp::Min),
                1,
                2,
                1,
                &[a.clone(), b.clone()],
            ),
            &[1.0, 2.0],
        );
        approx(
            &run(&MathOp::Binary(BinOp::Max), 1, 2, 1, &[a, b]),
            &[4.0, 9.0],
        );
    }

    #[test]
    fn binary_mod_truncating_and_div0() {
        let a = view(vec![5.0, -5.0, 7.0], 1, 3);
        let b = view(vec![3.0, 3.0, 0.0], 1, 3);
        // 5 % 3 = 2 ; -5 % 3 = -2 (truncating) ; 7 % 0 -> 0
        approx(
            &run(&MathOp::Binary(BinOp::Mod), 1, 3, 1, &[a, b]),
            &[2.0, -2.0, 0.0],
        );
    }

    #[test]
    fn modulo_is_always_positive_and_div0_zero() {
        // standalone `modulo` node: rem_euclid. 5%3=2 ; -1%3=2 (NOT -1) ; 3%3=0.
        let a = view(vec![5.0, -1.0, 3.0], 1, 3);
        approx(
            &run(&MathOp::Modulo { divisor: 3.0 }, 1, 3, 1, &[a.clone()]),
            &[2.0, 2.0, 0.0],
        );
        // divisor <= 0 -> 0
        approx(
            &run(&MathOp::Modulo { divisor: 0.0 }, 1, 3, 1, &[a]),
            &[0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn binary_abs_diff() {
        let a = view(vec![1.0, 5.0], 1, 2);
        let b = view(vec![4.0, 2.0], 1, 2);
        approx(
            &run(&MathOp::Binary(BinOp::AbsDiff), 1, 2, 1, &[a, b]),
            &[3.0, 3.0],
        );
    }

    #[test]
    fn binary_broadcasts_n() {
        // scalar a (n=1) * vector b (n=3)
        let a = view(vec![2.0], 1, 1);
        let b = view(vec![1.0, 2.0, 3.0], 3, 1);
        approx(
            &run(&MathOp::Binary(BinOp::Mul), 3, 1, 1, &[a, b]),
            &[2.0, 4.0, 6.0],
        );
    }

    #[test]
    fn unary_abs_neg() {
        let a = view(vec![-1.5, 2.0], 1, 2);
        approx(
            &run(&MathOp::Unary(UnaryOp::Abs), 1, 2, 1, &[a.clone()]),
            &[1.5, 2.0],
        );
        approx(
            &run(&MathOp::Unary(UnaryOp::Neg), 1, 2, 1, &[a]),
            &[1.5, -2.0],
        );
    }

    #[test]
    fn unary_floor_ceil_round() {
        let a = view(vec![1.2, 1.8, 2.5], 1, 3);
        approx(
            &run(&MathOp::Unary(UnaryOp::Floor), 1, 3, 1, &[a.clone()]),
            &[1.0, 1.0, 2.0],
        );
        approx(
            &run(&MathOp::Unary(UnaryOp::Ceil), 1, 3, 1, &[a.clone()]),
            &[2.0, 2.0, 3.0],
        );
        // round: 2.5 -> 3 (round-half-away-from-zero, f32::round)
        approx(
            &run(&MathOp::Unary(UnaryOp::Round), 1, 3, 1, &[a]),
            &[1.0, 2.0, 3.0],
        );
    }

    #[test]
    fn unary_invert_one_minus_x() {
        let a = view(vec![0.0, 0.25, 1.0], 1, 3);
        approx(
            &run(&MathOp::Unary(UnaryOp::Invert), 1, 3, 1, &[a]),
            &[1.0, 0.75, 0.0],
        );
    }

    #[test]
    fn threshold_binarizes() {
        let a = view(vec![0.2, 0.5, 0.8], 1, 3);
        // >= 0.5 -> 1
        approx(
            &run(&MathOp::Threshold { cutoff: 0.5 }, 1, 3, 1, &[a]),
            &[0.0, 1.0, 1.0],
        );
    }

    #[test]
    fn remap_linear_with_clamp() {
        // map [-1,1] -> [0,180], clamp on. Inputs -2 (clamped to -1), 0, 1.
        let a = view(vec![-2.0, 0.0, 1.0], 1, 3);
        let op = MathOp::Remap {
            in_min: -1.0,
            in_max: 1.0,
            out_min: 0.0,
            out_max: 180.0,
            clamp: true,
        };
        // -1 -> 0 ; 0 -> 90 ; 1 -> 180
        approx(&run(&op, 1, 3, 1, &[a]), &[0.0, 90.0, 180.0]);
    }

    #[test]
    fn remap_no_clamp_overshoots() {
        let a = view(vec![-2.0, 2.0], 1, 2);
        let op = MathOp::Remap {
            in_min: -1.0,
            in_max: 1.0,
            out_min: 0.0,
            out_max: 100.0,
            clamp: false,
        };
        // u(-2) = (-2 - -1)/2 = -0.5 -> -50 ; u(2) = (2 - -1)/2 = 1.5 -> 150
        approx(&run(&op, 1, 2, 1, &[a]), &[-50.0, 150.0]);
    }

    #[test]
    fn remap_degenerate_denom() {
        // in_min == in_max -> safe_denom = 1.0, output = out_min + (v - in_min)*range
        let a = view(vec![5.0], 1, 1);
        let op = MathOp::Remap {
            in_min: 5.0,
            in_max: 5.0,
            out_min: 7.0,
            out_max: 9.0,
            clamp: false,
        };
        // u = (5-5)/1 = 0 -> out_min = 7
        approx(&run(&op, 1, 1, 1, &[a]), &[7.0]);
    }

    #[test]
    fn ramp_between_is_span_relative() {
        // span [0,4]; absolute times 0,1,2,3 -> progress 0,0.25,0.5,0.75.
        // start=0, end=10 -> 0, 2.5, 5, 7.5.
        let t = 4;
        let start = view(vec![0.0; t], 1, 1);
        let end = view(vec![10.0; t], 1, 1);
        approx(
            &run_spanned(
                &MathOp::RampBetween,
                1,
                1,
                &[0.0, 1.0, 2.0, 3.0],
                (0.0, 4.0),
                &[start, end],
            ),
            &[0.0, 2.5, 5.0, 7.5],
        );
    }

    #[test]
    fn ramp_between_single_frame_decode() {
        // The realtime case: t=1 at an absolute time mid-span must give the right
        // progress (the old k/t impl returned `start` here — the bug this fixes).
        // span [10,20], time 15 -> progress 0.5; start=0 end=8 -> 4.
        let start = view(vec![0.0], 1, 1);
        let end = view(vec![8.0], 1, 1);
        approx(
            &run_spanned(
                &MathOp::RampBetween,
                1,
                1,
                &[15.0],
                (10.0, 20.0),
                &[start, end],
            ),
            &[4.0],
        );
    }

    #[test]
    fn ramp_between_broadcasts_n_axis() {
        // span [0,2]; times 0,1 -> progress 0,0.5. start n=1 broadcasts; end per-primitive.
        let start = view(vec![0.0, 0.0], 1, 1); // n=1,t=2,c=1
        let end = view(vec![10.0, 10.0, 20.0, 20.0], 2, 1); // n=2,t=2,c=1
        approx(
            &run_spanned(
                &MathOp::RampBetween,
                2,
                1,
                &[0.0, 1.0],
                (0.0, 2.0),
                &[start, end],
            ),
            &[0.0, 5.0, 0.0, 10.0],
        );
    }
}
