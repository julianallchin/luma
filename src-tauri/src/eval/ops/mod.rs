//! Op kernels, grouped by category (PyTorch `native/` style: many ops per file,
//! schema in `eval::OpKind`, kernels here). Each category file owns one sub-enum
//! and one `run_<cat>` function + its unit tests. **Touch only your own file.**
//!
//! Kernel contract (v1): a kernel is a pure function `(&CatOp, &KernelCtx) ->
//! Vec<f32>` returning the output slot's buffer (`n*t*c` row-major as
//! `[primitive][time][channel]`). v1 returns an owned buffer for simplicity and
//! parallelizability; the later zero-alloc pass swaps this for write-in-place
//! views without changing kernel logic. Pure-fn shape also keeps the GPU door open.

pub mod audio;
pub mod color;
pub mod math;
pub mod select_apply;
pub mod signals;
pub mod spatial;

use crate::eval::{ResidentContext, SlotSpec};

/// A resolved, read-only view of an input slot. Broadcasts the `n` axis when the
/// slot is `n == 1` (a scalar/global feeding `N` primitives).
pub struct InputView<'a> {
    pub data: &'a [f32],
    pub spec: SlotSpec,
}

impl InputView<'_> {
    /// Value at `(primitive i, time k, channel ch)`, broadcasting the `n` axis
    /// (n=1 -> all primitives) AND the `c` axis (a narrower input's last channel
    /// feeds wider output channels, e.g. a c=1 scalar into a c=3 color op).
    #[inline]
    pub fn at(&self, i: usize, k: usize, ch: usize, t: usize) -> f32 {
        let ni = if self.spec.n == 1 { 0 } else { i };
        let c = self.spec.c as usize;
        let ch = if ch >= c { c.saturating_sub(1) } else { ch };
        self.data[ni * t * c + k * c + ch]
    }
}

/// Everything a kernel needs: its inputs, its output shape, and the time axis.
/// Resident context (beat grid, audio, frozen stats) gets added here as those
/// pieces land — kernels that don't need them are unaffected.
pub struct KernelCtx<'a> {
    pub inputs: &'a [InputView<'a>],
    pub out_spec: SlotSpec,
    pub times: &'a [f32],
    /// Per-track resident data: fixture positions, beat grid, audio, frozen stats.
    pub ctx: &'a ResidentContext,
}

impl KernelCtx<'_> {
    /// Time-axis length.
    #[inline]
    pub fn t(&self) -> usize {
        self.times.len()
    }
    /// Output primitive count.
    #[inline]
    pub fn n(&self) -> usize {
        self.out_spec.n as usize
    }
    /// Output channel count.
    #[inline]
    pub fn c(&self) -> usize {
        self.out_spec.c as usize
    }
    #[inline]
    pub fn input(&self, idx: usize) -> &InputView<'_> {
        &self.inputs[idx]
    }
    /// A zeroed output buffer of the correct length (`n*t*c`).
    #[inline]
    pub fn out_buf(&self) -> Vec<f32> {
        vec![0.0; self.n() * self.t() * self.c()]
    }
    /// Row-major index into an output buffer for `(i, k, ch)`.
    #[inline]
    pub fn out_idx(&self, i: usize, k: usize, ch: usize) -> usize {
        let c = self.c();
        i * self.t() * c + k * c + ch
    }
}
