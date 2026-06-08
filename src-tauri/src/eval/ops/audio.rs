//! Audio ops — windowed, RANDOM-ACCESS by ABSOLUTE time (never streaming, so
//! seeking/scrubbing stays deterministic). At every `ctx.times[k]` we take the
//! window of `ctx.ctx.audio.samples` ending at the sample nearest that absolute
//! time and transform *that* window — there is no rolling/streaming state carried
//! between frames, so evaluating the same `t` twice (forward, then backward) is
//! bit-identical. This is the whole seek-determinism contract for audio.
//!
//! ## Shared `Stft` (audio CSE) — design, not yet wired
//! Per `docs/eval-ir.md` §4 the compiler injects ONE `Stft` op per frame and
//! every spectrum consumer reads its output slot. `Stft` would emit the magnitude
//! spectrum for each `times[k]` as a slot of shape `n=1, c=FFT_SIZE/2+1`, indexed
//! `[time][bin]`; `FreqAmplitude` would then read `ctx.input(0)` (the Stft slot)
//! and reduce bins, instead of recomputing the FFT itself. That sharing is NOT
//! wired in the compiler yet (no Stft slot is injected, no inputs are routed), so
//! for v1 `FreqAmplitude` computes its own STFT window inline. The reduction is
//! factored into [`magnitude_spectrum`] so the body of `Stft` and the body of
//! `FreqAmplitude` already call the *same* windowing+FFT path — wiring the CSE
//! later is "make FreqAmplitude read input(0) when present" with zero math change.
//!
//! ## What the compiler / ResidentContext must still provide (see report)
//! - A shared `Stft` slot + input routing (CSE) — math is ready, plumbing isn't.
//! - Cached per-stem resident audio for `StemSplit` — there is NO field on
//!   `ResidentContext` for stems today, so `StemSplit` returns zeros (stub).

use super::KernelCtx;
use crate::audio::fft::{FftService, FFT_SIZE};

thread_local! {
    /// One FFT plan + hann window per worker thread. Building a `RealFft` plan is
    /// not free; the kernel is a pure fn of (audio, t) regardless of how many
    /// times it's called, so caching the plan is a pure perf concern.
    static FFT: FftService = FftService::new();
}

/// Magnitude-scaling factor matching legacy `calculate_frequency_amplitude`:
/// `(mean_bin_magnitude / FFT_SIZE) * 4.0`. Kept as a constant so `Stft` (raw
/// spectrum) and `FreqAmplitude` (band reduction) agree on normalization.
const MAG_SCALE: f32 = 4.0;

#[derive(Clone, Debug)]
pub enum AudioOp {
    /// Short-time Fourier transform window ending at the current absolute time.
    /// Compiler-injected, shared across spectrum consumers (CSE). Output slot is
    /// `c = FFT_SIZE/2 + 1` magnitude bins per time sample (`n = 1`). v1: computed
    /// here; consumers don't read it yet (see module docs).
    Stft,
    /// Band energy over a set of `[lo_hz, hi_hz]` ranges. Emits ONE energy value
    /// per time sample (`n = 1, c = 1`), broadcast to all primitives. Mean bin
    /// magnitude across every range, scaled like legacy `frequency_amplitude`.
    /// `stem` selects the resident audio: `None` = full mix, `Some(name)` = that
    /// preprocessed stem (the compiler resolves it by tracing `audio_in` back to a
    /// `stem_splitter`; `stem_splitter` itself lowers to nothing).
    FreqAmplitude { ranges: Vec<[f32; 2]>, stem: Option<String> },
    /// 2nd-order Butterworth lowpass of the window preceding `t`, reduced to the
    /// window's RMS energy (`n = 1, c = 1`). Random-access: the filter runs from a
    /// zeroed state at the window's absolute start every frame (no carried IIR
    /// state → seek-safe).
    Lowpass { cutoff_hz: f32 },
    /// 2nd-order Butterworth highpass, same windowed/RMS contract as `Lowpass`.
    Highpass { cutoff_hz: f32 },
    /// One-hot 12-channel chroma from `ResidentContext.chord_sections` (legacy
    /// `harmony_analysis` sections fallback): at each time the active section's
    /// root pitch-class is 1.0, the rest 0.0. `n = 1, c = 12`.
    Chroma,
}

/// Resolve resident audio. `stem` selects a preprocessed stem from
/// `ctx.ctx.stems`; `None` (or a missing stem) falls back to the full mix.
/// Returns `None` when no audio is loaded — callers then emit zeros, never panic.
fn resident<'a>(ctx: &'a KernelCtx, stem: Option<&str>) -> Option<(&'a [f32], u32)> {
    let a = stem
        .and_then(|s| ctx.ctx.stems.get(s))
        .or(ctx.ctx.audio.as_ref())?;
    if a.samples.is_empty() || a.sample_rate == 0 {
        return None;
    }
    Some((a.samples.as_slice(), a.sample_rate))
}

/// Index of the last sample at-or-before absolute time `t` (seconds). Clamped to
/// the buffer. This anchor is a pure function of `t` and `sample_rate`, which is
/// what makes the windowing seek-deterministic.
#[inline]
fn anchor_sample(t: f32, sample_rate: u32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let idx = (t.max(0.0) * sample_rate as f32).round() as i64;
    idx.clamp(0, len as i64 - 1) as usize
}

/// Copy the `FFT_SIZE`-sample window ENDING at `anchor` (inclusive) into `dst`,
/// hann-windowed, zero-padding the front when `anchor < FFT_SIZE`. Window is
/// "preceding `t`" so the spectrum reflects audio up to the playhead — the same
/// causal convention the realtime decoder needs, and a deterministic fn of `t`.
fn fill_window(samples: &[f32], anchor: usize, window: &[f32], dst: &mut [f32]) {
    let n = FFT_SIZE;
    // First sample of the window in buffer coordinates (may be negative → pad).
    let start = anchor as i64 - (n as i64 - 1);
    for (j, slot) in dst.iter_mut().enumerate().take(n) {
        let src = start + j as i64;
        let s = if src >= 0 && (src as usize) < samples.len() {
            samples[src as usize]
        } else {
            0.0
        };
        *slot = s * window[j];
    }
}

/// Compute the magnitude spectrum (`FFT_SIZE/2 + 1` bins) of the window ending at
/// `anchor`. Shared by `Stft` and `FreqAmplitude` so the CSE wiring is math-free.
fn magnitude_spectrum(samples: &[f32], anchor: usize, out: &mut [f32]) {
    FFT.with(|fft| {
        let mut input = fft.plan.make_input_vec();
        let mut spectrum = fft.plan.make_output_vec();
        fill_window(samples, anchor, &fft.window, &mut input);
        if fft.plan.process(&mut input, &mut spectrum).is_ok() {
            for (o, c) in out.iter_mut().zip(spectrum.iter()) {
                *o = c.norm();
            }
        } else {
            out.iter_mut().for_each(|o| *o = 0.0);
        }
    });
}

/// Mean bin magnitude across the FFT bins covered by `ranges`, scaled to match
/// legacy `calculate_frequency_amplitude`. `spectrum` is raw `.norm()` magnitudes.
fn band_energy(spectrum: &[f32], ranges: &[[f32; 2]], sample_rate: u32) -> f32 {
    if ranges.is_empty() {
        return 0.0;
    }
    let freq_resolution = sample_rate as f32 / FFT_SIZE as f32;
    let spectrum_len = spectrum.len(); // FFT_SIZE/2 + 1
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for &[min_f, max_f] in ranges {
        let min_b = (min_f / freq_resolution).floor() as usize;
        let max_b = (max_f / freq_resolution).ceil() as usize;
        let min_b = min_b.min(spectrum_len - 1);
        let max_b = max_b.min(spectrum_len - 1).max(min_b);
        for bin in min_b..=max_b {
            sum += spectrum[bin];
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f32 / FFT_SIZE as f32) * MAG_SCALE
}

/// RMS energy of a windowed, filtered slice. Used by `Lowpass`/`Highpass` to turn
/// a filtered window into a single reactive scalar.
fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    let ss: f32 = buf.iter().map(|x| x * x).sum();
    (ss / buf.len() as f32).sqrt()
}

/// Window length for the time-domain filter ops (`Lowpass`/`Highpass`). One
/// FFT frame's worth of samples preceding `t`, so band reactivity tracks the
/// same temporal resolution as the spectral ops.
const FILTER_WINDOW: usize = FFT_SIZE;

/// Copy the `FILTER_WINDOW` samples ending at `anchor` (no windowing), zero-padded
/// at the front. Unlike `fill_window` this is the raw signal — the biquad does the
/// shaping. A pure fn of `anchor`, so the filtered result is seek-deterministic.
fn raw_window(samples: &[f32], anchor: usize) -> Vec<f32> {
    let mut buf = vec![0.0f32; FILTER_WINDOW];
    let start = anchor as i64 - (FILTER_WINDOW as i64 - 1);
    for (j, slot) in buf.iter_mut().enumerate() {
        let src = start + j as i64;
        if src >= 0 && (src as usize) < samples.len() {
            *slot = samples[src as usize];
        }
    }
    buf
}

pub fn run_audio(op: &AudioOp, ctx: &KernelCtx) -> Vec<f32> {
    match op {
        AudioOp::Stft => run_stft(ctx),
        AudioOp::FreqAmplitude { ranges, stem } => run_freq_amplitude(ctx, ranges, stem.as_deref()),
        AudioOp::Lowpass { cutoff_hz } => run_filter(ctx, *cutoff_hz, true),
        AudioOp::Highpass { cutoff_hz } => run_filter(ctx, *cutoff_hz, false),
        AudioOp::Chroma => run_chroma(ctx),
    }
}

/// One-hot 12-channel chroma per frame from the resident chord sections. The last
/// section whose `[start, end)` contains the time wins (matches legacy section
/// rasterization, where later sections overwrite earlier on overlap).
fn run_chroma(ctx: &KernelCtx) -> Vec<f32> {
    let mut out = ctx.out_buf(); // n=1, c=12
    let c = ctx.c();
    let sections = &ctx.ctx.chord_sections;
    for (k, &time) in ctx.times.iter().enumerate() {
        let root = sections
            .iter()
            .rev()
            .find(|&&(s, e, _)| time >= s && time < e)
            .and_then(|&(_, _, r)| r);
        if let Some(r) = root {
            let pc = (r as usize).min(c.saturating_sub(1));
            out[ctx.out_idx(0, k, pc)] = 1.0;
        }
    }
    out
}

/// Shared STFT: emit `c = FFT_SIZE/2+1` magnitude bins per time sample. `n = 1`
/// (one spectrum broadcast to every primitive). Consumers will read this slot
/// once the CSE plumbing lands; for now it stands alone and self-validates.
fn run_stft(ctx: &KernelCtx) -> Vec<f32> {
    let mut out = ctx.out_buf();
    let t = ctx.t();
    let c = ctx.c();
    let Some((samples, sr)) = resident(ctx, None) else {
        return out; // zeros
    };
    let mut spectrum = vec![0.0f32; FFT_SIZE / 2 + 1];
    for k in 0..t {
        let anchor = anchor_sample(ctx.times[k], sr, samples.len());
        magnitude_spectrum(samples, anchor, &mut spectrum);
        // n == 1: row 0 only. Write min(c, bins) channels.
        let chans = c.min(spectrum.len());
        for ch in 0..chans {
            out[ctx.out_idx(0, k, ch)] = spectrum[ch];
        }
    }
    out
}

fn run_freq_amplitude(ctx: &KernelCtx, ranges: &[[f32; 2]], stem: Option<&str>) -> Vec<f32> {
    let mut out = ctx.out_buf(); // n=1, c=1
    let t = ctx.t();
    let Some((samples, sr)) = resident(ctx, stem) else {
        return out;
    };
    let mut spectrum = vec![0.0f32; FFT_SIZE / 2 + 1];
    for k in 0..t {
        let anchor = anchor_sample(ctx.times[k], sr, samples.len());
        // CSE NOTE: when the compiler routes a shared `Stft` slot as input(0),
        // replace this `magnitude_spectrum` call with a read of input(0) at time
        // k. The `band_energy` reduction below is unchanged.
        magnitude_spectrum(samples, anchor, &mut spectrum);
        out[ctx.out_idx(0, k, 0)] = band_energy(&spectrum, ranges, sr);
    }
    out
}

fn run_filter(ctx: &KernelCtx, cutoff_hz: f32, lowpass: bool) -> Vec<f32> {
    let mut out = ctx.out_buf(); // n=1, c=1
    let t = ctx.t();
    let Some((samples, sr)) = resident(ctx, None) else {
        return out;
    };
    for k in 0..t {
        let anchor = anchor_sample(ctx.times[k], sr, samples.len());
        let window = raw_window(samples, anchor);
        // Fresh filter state per window (no carried IIR state across frames) is
        // what makes scrubbing deterministic.
        let filtered = if lowpass {
            crate::audio::lowpass_filter(&window, cutoff_hz, sr as f32)
        } else {
            crate::audio::highpass_filter(&window, cutoff_hz, sr as f32)
        };
        out[ctx.out_idx(0, k, 0)] = rms(&filtered);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{ResidentAudio, ResidentContext, SlotSpec};
    use std::sync::Arc;

    const SR: u32 = 44100;

    /// Build a mono sine tone of `freq` Hz, `secs` seconds long.
    fn sine(freq: f32, secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    /// Construct a single-output `KernelCtx` over borrowed resident context.
    fn ctx_with<'a>(
        rc: &'a ResidentContext,
        out_spec: SlotSpec,
        times: &'a [f32],
    ) -> KernelCtx<'a> {
        KernelCtx {
            inputs: &[],
            out_spec,
            times,
            ctx: rc,
        }
    }

    fn audio_ctx(samples: Vec<f32>) -> ResidentContext {
        ResidentContext {
            audio: Some(ResidentAudio {
                samples: Arc::new(samples),
                sample_rate: SR,
            }),
            ..Default::default()
        }
    }

    /// Seek-determinism: evaluating FreqAmplitude at [a, b, a] must give an
    /// identical value at both `a`s. This is the scrub-safety canary.
    #[test]
    fn freq_amplitude_is_seek_deterministic() {
        let rc = audio_ctx(sine(440.0, 4.0));
        let op = AudioOp::FreqAmplitude {
            ranges: vec![[300.0, 600.0]],
                stem: None,
        };
        let times = [1.0f32, 3.0, 1.0];
        let ctx = ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times);
        let out = run_audio(&op, &ctx);
        assert_eq!(out.len(), 3);
        // value at the two `a` evaluations must be bit-identical.
        assert_eq!(
            out[0].to_bits(),
            out[2].to_bits(),
            "scrub back to the same t must reproduce the exact value"
        );
    }

    /// A band containing the tone reads higher energy than an empty band.
    #[test]
    fn band_with_tone_beats_empty_band() {
        let rc = audio_ctx(sine(440.0, 4.0));
        let times = [2.0f32];

        let hit = run_audio(
            &AudioOp::FreqAmplitude {
                ranges: vec![[400.0, 480.0]],
                stem: None,
            },
            &ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times),
        )[0];
        let miss = run_audio(
            &AudioOp::FreqAmplitude {
                ranges: vec![[5000.0, 8000.0]],
                stem: None,
            },
            &ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times),
        )[0];

        assert!(
            hit > miss * 4.0,
            "band over the tone ({hit}) should dominate an empty band ({miss})"
        );
        assert!(hit > 0.0);
    }

    /// Stft emits the magnitude spectrum and peaks in the bin nearest the tone.
    #[test]
    fn stft_peaks_at_tone_bin() {
        let rc = audio_ctx(sine(1000.0, 4.0));
        let bins = FFT_SIZE / 2 + 1;
        let times = [2.0f32];
        let ctx = ctx_with(
            &rc,
            SlotSpec {
                n: 1,
                c: bins as u32,
            },
            &times,
        );
        let out = run_audio(&AudioOp::Stft, &ctx);
        assert_eq!(out.len(), bins);
        let peak_bin = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        let expected = (1000.0 / (SR as f32 / FFT_SIZE as f32)).round() as usize;
        assert!(
            (peak_bin as i64 - expected as i64).abs() <= 2,
            "spectral peak {peak_bin} should be near tone bin {expected}"
        );
    }

    /// Stft is also seek-deterministic per bin.
    #[test]
    fn stft_is_seek_deterministic() {
        let rc = audio_ctx(sine(660.0, 4.0));
        let bins = FFT_SIZE / 2 + 1;
        let times = [1.5f32, 3.2, 1.5];
        let ctx = ctx_with(
            &rc,
            SlotSpec {
                n: 1,
                c: bins as u32,
            },
            &times,
        );
        let out = run_audio(&AudioOp::Stft, &ctx);
        for ch in 0..bins {
            assert_eq!(out[ch].to_bits(), out[2 * bins + ch].to_bits());
        }
    }

    /// Lowpass passes a low tone (high RMS) and rejects a high tone (low RMS).
    #[test]
    fn lowpass_passes_low_rejects_high() {
        let times = [2.0f32];
        let rc_low = audio_ctx(sine(100.0, 4.0));
        let rc_high = audio_ctx(sine(8000.0, 4.0));
        let low = run_audio(
            &AudioOp::Lowpass { cutoff_hz: 300.0 },
            &ctx_with(&rc_low, SlotSpec { n: 1, c: 1 }, &times),
        )[0];
        let high = run_audio(
            &AudioOp::Lowpass { cutoff_hz: 300.0 },
            &ctx_with(&rc_high, SlotSpec { n: 1, c: 1 }, &times),
        )[0];
        assert!(low > high * 4.0, "lowpass: low tone {low} should beat high tone {high}");
    }

    /// Highpass does the opposite, and is seek-deterministic.
    #[test]
    fn highpass_passes_high_and_is_deterministic() {
        let rc = audio_ctx(sine(8000.0, 4.0));
        let rc_low = audio_ctx(sine(100.0, 4.0));
        let times = [1.0f32, 2.5, 1.0];
        let hp_high = run_audio(
            &AudioOp::Highpass { cutoff_hz: 2000.0 },
            &ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times),
        );
        let hp_low = run_audio(
            &AudioOp::Highpass { cutoff_hz: 2000.0 },
            &ctx_with(&rc_low, SlotSpec { n: 1, c: 1 }, &times),
        );
        assert!(hp_high[0] > hp_low[0] * 4.0);
        assert_eq!(hp_high[0].to_bits(), hp_high[2].to_bits(), "highpass must be seek-safe");
    }

    /// No resident audio → zeros, never a panic.
    #[test]
    fn no_audio_yields_zeros() {
        let rc = ResidentContext::default();
        let times = [0.0f32, 1.0];
        let out = run_audio(
            &AudioOp::FreqAmplitude {
                ranges: vec![[100.0, 200.0]],
                stem: None,
            },
            &ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times),
        );
        assert_eq!(out, vec![0.0, 0.0]);
    }

    /// FreqAmplitude with a stem reads ctx.stems; missing stem falls back to mix.
    #[test]
    fn freq_amplitude_stem_falls_back_to_mix_when_absent() {
        let rc = audio_ctx(sine(440.0, 1.0));
        let times = [0.5f32];
        let with_stem = run_audio(
            &AudioOp::FreqAmplitude { ranges: vec![[400.0, 480.0]], stem: Some("bass".into()) },
            &ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times),
        );
        let mix = run_audio(
            &AudioOp::FreqAmplitude { ranges: vec![[400.0, 480.0]], stem: None },
            &ctx_with(&rc, SlotSpec { n: 1, c: 1 }, &times),
        );
        // No "bass" stem present → falls back to the full mix, same result.
        assert_eq!(with_stem, mix);
    }
}
