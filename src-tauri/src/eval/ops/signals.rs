//! Temporal generators — pure functions of absolute time `t` (+ seed, + beat
//! grid). Always kernel phase. Seek-safe by construction (value depends only on
//! `t`, not on playback history). Ported from legacy `node_graph/nodes/signals.rs`
//! + `node_graph/nodes/audio.rs` (adsr / beat_envelope / beat_pulses).
//!
//! ## Time model (the big change vs. legacy)
//!
//! Legacy generators sampled a fixed `t_steps` grid spanning a pattern's
//! `[start_time, end_time]` and worked in *pattern-local* seconds. The eval IR
//! instead feeds **absolute track time** through `ctx.times`, and every kernel
//! here is a *pure function of that absolute time*. So where legacy computed
//! `beats = (i / t_steps) * total_beats`, we compute `beats = t * bpm / 60`
//! directly — identical instantaneous values, but seek-safe and grid-free.
//!
//! ## Conventions the compiler must honor (read this if you are agent C)
//!
//! - **`seed: u64`** — legacy hashed the node id with `DefaultHasher` to seed
//!   `Noise` / `Wander`. We have no node id here, so the compiler MUST hash the
//!   node id the same way and pass it as the `seed` param. The hashing/mixing
//!   below (`hash_combine`, octave seeds `i*12345` for noise / `i*7919` for
//!   wander, per-primitive `prim*2` / `prim*2+1` for wander U/V) is reproduced
//!   bit-for-bit, so a given (seed, t) is deterministic and matches legacy when
//!   `seed == DefaultHasher(node.id)`.
//! - **`Normalize { stat_idx }`** — we do NOT scan for a global min/max at
//!   runtime (that would be history-dependent and break seek-safety). The
//!   compiler runs the sub-DAG feeding the Normalize over the dense grid once and
//!   writes the pair `[min, max]` into `ctx.ctx.frozen` at `frozen[stat_idx]` and
//!   `frozen[stat_idx + 1]`. The kernel applies `(x - min) / (max - min)` clamped
//!   to `[0,1]` pointwise. Degenerate range (`<= EPSILON`) → 0.0, matching legacy.
//! - **`TimeDelay`** — per docs/eval-ir.md the compiler marks the upstream cone
//!   feeding the `in` port as a re-evaluable sub-kernel and evaluates it at
//!   `times - delay`. By the time the value reaches THIS kernel it is already
//!   time-shifted, so the kernel is a near-identity pass-through of input 0. We
//!   keep an optional constant `delay` param only for documentation / a v1
//!   fallback; if the compiler ever feeds un-shifted input it has a bug. Variable
//!   / per-fixture delay is v2 (see docs deferral).

use super::KernelCtx;

/// SplitMix64-style mixer — bit-identical to legacy `hash_combine` used by the
/// `noise` / `wander` / `random_select_mask` nodes.
#[inline]
fn hash_combine(seed: u64, v: u64) -> u64 {
    let mut x = seed ^ v;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

#[inline]
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Curve shaper — bit-identical to legacy `executor::shape_curve`.
fn shape_curve(x: f32, curve: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if curve.abs() < 0.001 {
        x
    } else if curve > 0.0 {
        let p = 1.0 + curve * 5.0;
        x.powf(p)
    } else {
        let p = 1.0 + (-curve) * 5.0;
        1.0 - (1.0 - x).powf(p)
    }
}

// ---- 1D value noise (wander) — bit-identical to legacy ----------------------

#[inline]
fn noise_1d(pos: i64, seed: u64) -> f32 {
    let h = hash_combine(seed, pos as u64);
    (h as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
}

fn interp_1d(x: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i64;
    let x1 = x0 + 1;
    let t = smoothstep(x - x0 as f32);
    let n0 = noise_1d(x0, seed);
    let n1 = noise_1d(x1, seed);
    n0 + t * (n1 - n0)
}

fn fractal_1d(x: f32, seed: u64, octaves: u32) -> f32 {
    let mut total = 0.0f32;
    let mut freq = 1.0f32;
    let mut amp = 1.0f32;
    let mut max_val = 0.0f32;
    for i in 0..octaves {
        let oct_seed = hash_combine(seed, i as u64 * 7919);
        total += interp_1d(x * freq, oct_seed) * amp;
        max_val += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    total / max_val
}

// ---- 3D value noise (noise) — bit-identical to legacy -----------------------

#[inline]
fn noise_at_3d(x: i64, y: i64, z: i64, seed: u64) -> f32 {
    let h = hash_combine(
        hash_combine(hash_combine(seed, x as u64), y as u64),
        z as u64,
    );
    (h as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
}

fn value_noise_3d(x: f32, y: f32, z: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i64;
    let x1 = x0 + 1;
    let y0 = y.floor() as i64;
    let y1 = y0 + 1;
    let z0 = z.floor() as i64;
    let z1 = z0 + 1;

    let tx = smoothstep(x - x0 as f32);
    let ty = smoothstep(y - y0 as f32);
    let tz = smoothstep(z - z0 as f32);

    let n000 = noise_at_3d(x0, y0, z0, seed);
    let n100 = noise_at_3d(x1, y0, z0, seed);
    let n010 = noise_at_3d(x0, y1, z0, seed);
    let n110 = noise_at_3d(x1, y1, z0, seed);
    let n001 = noise_at_3d(x0, y0, z1, seed);
    let n101 = noise_at_3d(x1, y0, z1, seed);
    let n011 = noise_at_3d(x0, y1, z1, seed);
    let n111 = noise_at_3d(x1, y1, z1, seed);

    let nx00 = n000 + tx * (n100 - n000);
    let nx10 = n010 + tx * (n110 - n010);
    let nx01 = n001 + tx * (n101 - n001);
    let nx11 = n011 + tx * (n111 - n011);

    let nxy0 = nx00 + ty * (nx10 - nx00);
    let nxy1 = nx01 + ty * (nx11 - nx01);

    nxy0 + tz * (nxy1 - nxy0)
}

fn fractal_noise_3d(x: f32, y: f32, z: f32, seed: u64, octaves: u32) -> f32 {
    let mut total = 0.0f32;
    let mut frequency = 1.0f32;
    let mut amplitude_scale = 1.0f32;
    let mut max_value = 0.0f32;
    for i in 0..octaves {
        let octave_seed = hash_combine(seed, i as u64 * 12345);
        total += value_noise_3d(x * frequency, y * frequency, z * frequency, octave_seed)
            * amplitude_scale;
        max_value += amplitude_scale;
        amplitude_scale *= 0.5;
        frequency *= 2.0;
    }
    total / max_value
}

// ---- ADSR envelope — bit-identical to legacy `executor` ---------------------

/// Split a span into A/D/S/R seconds by their unitless weights (legacy
/// `adsr_durations`).
fn adsr_durations(span_sec: f32, attack: f32, decay: f32, sustain: f32, release: f32) -> (f32, f32, f32, f32) {
    let a_w = attack.clamp(0.0, 1.0);
    let d_w = decay.clamp(0.0, 1.0);
    let s_w = sustain.clamp(0.0, 1.0);
    let r_w = release.clamp(0.0, 1.0);
    let weight_sum = a_w + d_w + s_w + r_w;
    if weight_sum < 1e-6 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let scale = span_sec / weight_sum;
    (a_w * scale, d_w * scale, s_w * scale, r_w * scale)
}

/// ADSR shape value at `t` seconds since the shape began (legacy `calc_envelope`).
#[allow(clippy::too_many_arguments)]
fn calc_envelope(
    t: f32,
    att_s: f32,
    dec_s: f32,
    sus_s: f32,
    rel_s: f32,
    sustain_level: f32,
    a_curve: f32,
    d_curve: f32,
) -> f32 {
    if t < 0.0 {
        return 0.0;
    }
    if t < att_s {
        if att_s <= 0.0 {
            return 1.0;
        }
        return shape_curve(t / att_s, a_curve);
    }
    let decay_start = att_s;
    let decay_end = decay_start + dec_s;
    if t < decay_end {
        if dec_s <= 0.0 {
            return sustain_level;
        }
        let x = (t - decay_start) / dec_s;
        let shaped = shape_curve(1.0 - x, d_curve);
        return sustain_level + (1.0 - sustain_level) * shaped;
    }
    let sustain_end = decay_end + sus_s;
    if t < sustain_end + 1e-4 {
        return sustain_level;
    }
    let release_end = sustain_end + rel_s;
    if t < release_end + 1e-4 {
        if rel_s <= 0.0 {
            return 0.0;
        }
        let x = ((t - sustain_end) / rel_s).clamp(0.0, 1.0);
        return sustain_level * (1.0 - x);
    }
    0.0
}

/// ADSR shape parameters for the `Adsr` op. A/D/S/R are
/// unitless weights; `length_beats` + `bpm` (or the inter-pulse gap in
/// `fit_to_gap` mode) set the span. Mirrors legacy `AdsrParams`.
#[derive(Clone, Debug)]
pub struct AdsrParams {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub sustain_level: f32,
    pub a_curve: f32,
    pub d_curve: f32,
    pub amp: f32,
    pub fit_to_gap: bool,
    pub length_beats: f32,
    pub bpm: f32,
}

impl AdsrParams {
    fn fixed_length_sec(&self) -> f32 {
        let bpm = self.bpm.max(1e-3);
        (self.length_beats * 60.0 / bpm).max(1e-3)
    }
}

fn pulse_min_spacing(pulse_starts: &[f32]) -> Option<f32> {
    pulse_starts
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .filter(|d| *d > 1e-4)
        .fold(None, |acc: Option<f32>, d| Some(acc.map_or(d, |a| a.min(d))))
}

/// Pure-fn ADSR sample at absolute time `time` against precomputed `pulse_starts`
/// (sorted, absolute seconds). Mirrors legacy `sample_adsr_signal`'s per-step
/// body, including the `max(prev, current)` tail-overlap rule.
fn adsr_value_at(time: f32, pulse_starts: &[f32], att_s: f32, dec_s: f32, sus_s: f32, rel_s: f32, p: &AdsrParams) -> f32 {
    let shape_len = att_s + dec_s + sus_s + rel_s;
    let shape_eps = shape_len + 1e-3;
    let idx = pulse_starts.partition_point(|&ps| ps <= time + att_s);
    let val = if idx > 0 {
        let dt = time - pulse_starts[idx - 1] + att_s;
        let current = if dt <= shape_eps {
            calc_envelope(dt, att_s, dec_s, sus_s, rel_s, p.sustain_level, p.a_curve, p.d_curve)
        } else {
            0.0
        };
        if idx >= 2 {
            let dt_prev = time - pulse_starts[idx - 2] + att_s;
            if dt_prev <= shape_eps {
                let prev = calc_envelope(dt_prev, att_s, dec_s, sus_s, rel_s, p.sustain_level, p.a_curve, p.d_curve);
                current.max(prev)
            } else {
                current
            }
        } else {
            current
        }
    } else {
        0.0
    };
    val * p.amp
}

/// Subdivision-aligned pulse start times from the resident beat grid. Mirrors
/// legacy `beat_grid_pulses`, but anchored at absolute t=0 (the legacy
/// `context.start_time` is the pattern origin; in absolute time that is 0).
pub(crate) fn beat_grid_pulses(
    grid: &crate::models::node_graph::BeatGrid,
    subdivision: f32,
    offset: f32,
    only_downbeats: bool,
) -> Vec<f32> {
    let source_beats = if only_downbeats { &grid.downbeats } else { &grid.beats };
    let beat_len = if grid.bpm > 0.0 { 60.0 / grid.bpm } else { 0.5 };
    let beat_step_beats = if subdivision.abs() < 1e-3 { 1.0 } else { (1.0 / subdivision).abs() };

    let mut pulse_starts = Vec::new();
    if source_beats.is_empty() {
        return pulse_starts;
    }
    let beat_step = beat_step_beats.max(1e-4);
    let last_index = (source_beats.len() - 1) as f32;
    // Absolute time: pattern origin is t=0.
    let anchor_idx = source_beats.partition_point(|t| *t < -1e-4) as f32;
    let mut beat_pos = if subdivision.abs() < 1.0 {
        anchor_idx + beat_step * 0.5
    } else {
        0.0
    };
    while beat_pos - beat_step >= 0.0 {
        beat_pos -= beat_step;
    }
    while beat_pos <= last_index + 1e-4 {
        let base_idx = beat_pos.floor() as usize;
        let frac = beat_pos - base_idx as f32;
        let time = if base_idx + 1 < source_beats.len() {
            let t0 = source_beats[base_idx];
            let t1 = source_beats[base_idx + 1];
            t0 + (t1 - t0) * frac
        } else {
            source_beats[base_idx]
        };
        pulse_starts.push(time + offset * beat_len);
        beat_pos += beat_step;
    }
    pulse_starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pulse_starts
}

/// Beat length in seconds from the resident grid, defaulting to 120 BPM
/// (0.5 s/beat) when no grid is present — matches legacy movement-node fallback.
fn beat_len_sec(ctx: &KernelCtx) -> f32 {
    ctx.ctx
        .beat_grid
        .as_ref()
        .map(|g| if g.bpm > 0.0 { 60.0 / g.bpm } else { 0.5 })
        .unwrap_or(0.5)
}

fn bpm_or_default(ctx: &KernelCtx) -> f32 {
    ctx.ctx
        .beat_grid
        .as_ref()
        .map(|g| if g.bpm > 0.0 { g.bpm } else { 120.0 })
        .unwrap_or(120.0)
}

#[derive(Clone, Debug)]
pub enum SignalOp {
    /// Raw sine of absolute time: `sin(2π·freq·t)`. Output `n=1, c=1`.
    Sine { freq: f32 },

    /// Beat-synced sine (legacy `sine_wave`). `subdivision` = cycles per beat;
    /// freq_hz = subdivision · bpm/60. `offset + amplitude·sin(2π·freq_hz·t + phase)`.
    /// Reads `ctx.ctx.beat_grid` for bpm. Output `n=1, c=1`. (Zeros if no grid.)
    SineWave { subdivision: f32, phase_deg: f32, amplitude: f32, offset: f32 },

    /// Linear ramp in *beats elapsed* (legacy `ramp`): `t · bpm/60`. Reads bpm
    /// from the grid. Output `n=1, c=1`. (Zeros if no grid.)
    Ramp,

    /// 1D fractal value-noise of absolute time (legacy `noise` with only the
    /// `time` coord driven, x=y=0). `offset + amplitude · fractal3d(0,0, t·scale)`.
    /// PURE value-noise of t (NOT a walk). `seed` = hashed node id (see header).
    /// Output `n=1, c=1`.
    /// Value-noise field sampled per primitive. Optional `x`/`y`/`time` inputs
    /// (supplied to the kernel in that order, only the wired ones present) feed
    /// the 3 noise coords, each scaled by `scale`. Legacy defaults: absent `x` →
    /// `primitive_index * scale`, absent `y`/`time` → 0.
    Noise { scale: f32, octaves: u32, amplitude: f32, offset: f32, seed: u64, has_x: bool, has_y: bool, has_time: bool },

    /// Organic UV drift via 1D fractal noise (legacy `wander`), `c=2` (u,v).
    /// `noise_coord = speed·(t/beat_len)`, u/v from independent seeds, ·radius,
    /// clamped [-1,1]. `octaves = round(smoothness)`. `seed` = hashed node id.
    /// Output `n=1, c=2`.
    Wander { radius: f32, speed: f32, smoothness: f32, seed: u64 },

    /// Circular UV motion (legacy `circle`), `c=2`. angle = 2π·speed·(t/beat_len),
    /// (cos·radius, sin·radius). Output `n=1, c=2`.
    Circle { radius: f32, speed: f32 },

    /// Lissajous 2:1 figure-8 UV motion (legacy `figure_8`), `c=2`.
    /// (cos θ·width, sin 2θ·height), θ = 2π·speed·(t/beat_len). Output `n=1, c=2`.
    Figure8 { width: f32, height: f32, speed: f32 },

    /// Linear sweep at an angle in UV space (legacy `sweep`), `c=2`.
    /// val = sin θ·range, projected to (cos α·val, sin α·val). Output `n=1, c=2`.
    Sweep { angle_deg: f32, range: f32, speed: f32 },

    /// Subdivision-aligned beat pulses rendered as a 1.0-spike train (legacy
    /// `beat_pulses`; legacy emitted an event list, here a sampled signal that is
    /// 1.0 within `tol` of a pulse, else 0.0). `tol` defaults small. Output `n=1, c=1`.
    BeatPulses { subdivision: f32, offset: f32, only_downbeats: bool, tol: f32 },

    /// ADSR envelope over an explicit list of absolute trigger times (legacy
    /// `adsr`; the upstream `events_in` is compiled into `pulse_starts`). Output `n=1, c=1`.
    Adsr { pulse_starts: Vec<f32>, params: AdsrParams },

    /// Reflect a signal around its observed midpoint (legacy `invert`). The
    /// midpoint is taken over the *frozen* observed range, NOT recomputed at
    /// runtime (seek-safety): `frozen[stat_idx] = min, frozen[stat_idx+1] = max`,
    /// `out = clamp(2·mid − x, min, max)`. Elementwise over input 0.
    Invert { stat_idx: usize },

    /// Normalize input 0 to [0,1] using frozen `[min,max]` at `frozen[stat_idx]`
    /// and `frozen[stat_idx+1]` (see header). Elementwise; degenerate range → 0.
    Normalize { stat_idx: usize },

    /// Soft falloff on a normalized input (legacy `falloff`): `shape_curve(
    /// clamp(clamp01(x)·width, 0, 1), curve)`. Elementwise over input 0.
    Falloff { width: f32, curve: f32 },

    /// Time shift (legacy `time_delay`). v1: identity pass-through of input 0 —
    /// the compiler has already evaluated the upstream cone at `times - delay`
    /// (see header). `delay` is retained for documentation only.
    TimeDelay { delay: f32 },
}

pub fn run_signals(op: &SignalOp, ctx: &KernelCtx) -> Vec<f32> {
    let mut out = ctx.out_buf();
    let t = ctx.t();
    match op {
        SignalOp::Sine { freq } => {
            for (k, &time) in ctx.times.iter().enumerate() {
                out[k] = (std::f32::consts::TAU * freq * time).sin();
            }
        }

        SignalOp::SineWave { subdivision, phase_deg, amplitude, offset } => {
            let Some(grid) = ctx.ctx.beat_grid.as_ref() else { return out };
            let bpm = grid.bpm;
            let phase = phase_deg.to_radians();
            let freq_hz = subdivision * (bpm / 60.0);
            let omega = std::f32::consts::TAU * freq_hz;
            for (k, &time) in ctx.times.iter().enumerate() {
                out[k] = offset + amplitude * (omega * time + phase).sin();
            }
        }

        SignalOp::Ramp => {
            let Some(grid) = ctx.ctx.beat_grid.as_ref() else { return out };
            let rate = grid.bpm / 60.0; // beats per second
            // Beats elapsed since the annotation's span start (legacy `ramp`:
            // `beat_in_pattern = (t - span_start) * bpm/60`), NOT absolute beats —
            // otherwise the hue/phase is offset by `(span_start * bpm/60).fract()`.
            let span0 = ctx.ctx.span.0;
            for (k, &time) in ctx.times.iter().enumerate() {
                out[k] = (time - span0) * rate;
            }
        }

        SignalOp::Noise { scale, octaves, amplitude, offset, seed, has_x, has_y, has_time } => {
            let oc = (*octaves).clamp(1, 8);
            let n = ctx.n();
            // Inputs are supplied in [x, y, time] order, only the wired ones.
            let mut idx = 0;
            let x_in = if *has_x { let v = ctx.input(idx); idx += 1; Some(v) } else { None };
            let y_in = if *has_y { let v = ctx.input(idx); idx += 1; Some(v) } else { None };
            let time_in = if *has_time { Some(ctx.input(idx)) } else { None };
            for i in 0..n {
                for k in 0..t {
                    // Each coord is scaled by `scale`; legacy defaults x→i*scale, y/z→0.
                    let x_val = match x_in { Some(v) => v.at(i, k, 0, t) * scale, None => i as f32 * scale };
                    let y_val = match y_in { Some(v) => v.at(i, k, 0, t) * scale, None => 0.0 };
                    let z_val = match time_in { Some(v) => v.at(i, k, 0, t) * scale, None => 0.0 };
                    let noise_val = fractal_noise_3d(x_val, y_val, z_val, *seed, oc);
                    out[ctx.out_idx(i, k, 0)] = offset + amplitude * noise_val;
                }
            }
        }

        SignalOp::Wander { radius, speed, smoothness, seed } => {
            let beat_len = beat_len_sec(ctx);
            let octaves = smoothness.clamp(0.5, 8.0).round() as u32;
            // n=1 → primitive index 0; legacy seeds U/V with prim*2 / prim*2+1.
            let seed_u = hash_combine(*seed, 0);
            let seed_v = hash_combine(*seed, 1);
            let c = ctx.c();
            for (k, &time) in ctx.times.iter().enumerate() {
                let beats = time / beat_len;
                let noise_coord = speed * beats; // phase_offset is 0 (no port)
                let u = (fractal_1d(noise_coord, seed_u, octaves) * radius).clamp(-1.0, 1.0);
                let v = (fractal_1d(noise_coord, seed_v, octaves) * radius).clamp(-1.0, 1.0);
                out[k * c] = u;
                if c > 1 {
                    out[k * c + 1] = v;
                }
            }
        }

        SignalOp::Circle { radius, speed } => {
            let beat_len = beat_len_sec(ctx);
            let c = ctx.c();
            for (k, &time) in ctx.times.iter().enumerate() {
                let beats = time / beat_len;
                let angle = std::f32::consts::TAU * (speed * beats);
                out[k * c] = angle.cos() * radius;
                if c > 1 {
                    out[k * c + 1] = angle.sin() * radius;
                }
            }
        }

        SignalOp::Figure8 { width, height, speed } => {
            let beat_len = beat_len_sec(ctx);
            let c = ctx.c();
            for (k, &time) in ctx.times.iter().enumerate() {
                let beats = time / beat_len;
                let theta = std::f32::consts::TAU * (speed * beats);
                out[k * c] = theta.cos() * width;
                if c > 1 {
                    out[k * c + 1] = (2.0 * theta).sin() * height;
                }
            }
        }

        SignalOp::Sweep { angle_deg, range, speed } => {
            let beat_len = beat_len_sec(ctx);
            let angle_rad = angle_deg.to_radians();
            let cos_a = angle_rad.cos();
            let sin_a = angle_rad.sin();
            let c = ctx.c();
            for (k, &time) in ctx.times.iter().enumerate() {
                let beats = time / beat_len;
                let theta = std::f32::consts::TAU * (speed * beats);
                let sweep_val = theta.sin() * range;
                out[k * c] = cos_a * sweep_val;
                if c > 1 {
                    out[k * c + 1] = sin_a * sweep_val;
                }
            }
        }

        SignalOp::BeatPulses { subdivision, offset, only_downbeats, tol } => {
            let Some(grid) = ctx.ctx.beat_grid.as_ref() else { return out };
            let pulse_starts = beat_grid_pulses(grid, *subdivision, *offset, *only_downbeats);
            for (k, &time) in ctx.times.iter().enumerate() {
                // 1.0 if within tol of any pulse, else 0.0.
                let idx = pulse_starts.partition_point(|&p| p <= time + tol);
                let near = (idx > 0 && (time - pulse_starts[idx - 1]).abs() <= *tol)
                    || (idx < pulse_starts.len() && (pulse_starts[idx] - time).abs() <= *tol);
                out[k] = if near { 1.0 } else { 0.0 };
            }
        }

        SignalOp::Adsr { pulse_starts, params } => {
            let span_sec = if params.fit_to_gap {
                pulse_min_spacing(pulse_starts).unwrap_or_else(|| params.fixed_length_sec())
            } else {
                params.fixed_length_sec()
            };
            let (att_s, dec_s, sus_s, rel_s) =
                adsr_durations(span_sec, params.attack, params.decay, params.sustain, params.release);
            for (k, &time) in ctx.times.iter().enumerate() {
                out[k] = adsr_value_at(time, pulse_starts, att_s, dec_s, sus_s, rel_s, params);
            }
        }

        SignalOp::Invert { stat_idx } => {
            let inp = ctx.input(0);
            let min_v = ctx.ctx.frozen.get(*stat_idx).copied().unwrap_or(0.0);
            let max_v = ctx.ctx.frozen.get(stat_idx + 1).copied().unwrap_or(1.0);
            if !min_v.is_finite() || !max_v.is_finite() {
                return inp.data.to_vec();
            }
            let mid = (max_v + min_v) * 0.5;
            let n = ctx.n();
            let c = ctx.c();
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        let x = inp.at(i, k, ch, t);
                        out[ctx.out_idx(i, k, ch)] = (2.0 * mid - x).clamp(min_v, max_v);
                    }
                }
            }
        }

        SignalOp::Normalize { stat_idx } => {
            let inp = ctx.input(0);
            let min_v = ctx.ctx.frozen.get(*stat_idx).copied().unwrap_or(0.0);
            let max_v = ctx.ctx.frozen.get(stat_idx + 1).copied().unwrap_or(1.0);
            let range = max_v - min_v;
            let n = ctx.n();
            let c = ctx.c();
            if range.abs() <= f32::EPSILON {
                // Degenerate range → 0.0 (legacy behavior). out is already zeroed.
                return out;
            }
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        let x = inp.at(i, k, ch, t);
                        out[ctx.out_idx(i, k, ch)] = ((x - min_v) / range).clamp(0.0, 1.0);
                    }
                }
            }
        }

        SignalOp::Falloff { width, curve } => {
            let inp = ctx.input(0);
            let w = width.max(0.0).max(1e-6);
            let n = ctx.n();
            let c = ctx.c();
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        let x = inp.at(i, k, ch, t);
                        let norm = x.clamp(0.0, 1.0);
                        let tightened = (norm * w).clamp(0.0, 1.0);
                        out[ctx.out_idx(i, k, ch)] = shape_curve(tightened, *curve);
                    }
                }
            }
        }

        SignalOp::TimeDelay { .. } => {
            // v1: identity — upstream cone already evaluated at `times - delay`.
            return ctx.input(0).data.to_vec();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::ops::InputView;
    use crate::eval::{ResidentContext, SlotSpec};
    use crate::models::node_graph::BeatGrid;

    fn run(op: &SignalOp, times: &[f32], n: u32, c: u32, ctx: &ResidentContext, inputs: &[InputView]) -> Vec<f32> {
        let kctx = KernelCtx {
            inputs,
            out_spec: SlotSpec { n, c },
            times,
            ctx,
        };
        run_signals(op, &kctx)
    }

    fn grid(bpm: f32) -> ResidentContext {
        ResidentContext {
            beat_grid: Some(BeatGrid {
                beats: (0..16).map(|i| i as f32 * 60.0 / bpm).collect(),
                downbeats: (0..4).map(|i| i as f32 * 4.0 * 60.0 / bpm).collect(),
                bpm,
                downbeat_offset: 0.0,
                beats_per_bar: 4,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn sine_hand_values() {
        let ctx = ResidentContext::default();
        let out = run(&SignalOp::Sine { freq: 1.0 }, &[0.0, 0.25, 0.5], 1, 1, &ctx, &[]);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 1.0).abs() < 1e-5);
        assert!((out[2] - 0.0).abs() < 1e-5);
    }

    #[test]
    fn sine_wave_beat_synced() {
        // 120 bpm => 2 beats/sec. subdivision=1 => 2 Hz. At t=0 => offset.
        // At quarter period (t = 1/8 s) => offset + amplitude.
        let ctx = grid(120.0);
        let op = SignalOp::SineWave { subdivision: 1.0, phase_deg: 0.0, amplitude: 1.0, offset: 0.0 };
        let out = run(&op, &[0.0, 0.125], 1, 1, &ctx, &[]);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 1.0).abs() < 1e-4);
        // No grid => zeros.
        let out0 = run(&op, &[0.0, 0.5], 1, 1, &ResidentContext::default(), &[]);
        assert_eq!(out0, vec![0.0, 0.0]);
    }

    #[test]
    fn ramp_is_beats_elapsed() {
        // 120 bpm => 2 beats/sec. t=1.5s => 3 beats.
        let ctx = grid(120.0);
        let out = run(&SignalOp::Ramp, &[0.0, 1.5], 1, 1, &ctx, &[]);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 3.0).abs() < 1e-5);
    }

    #[test]
    fn noise_is_deterministic_and_bounded() {
        let ctx = ResidentContext::default();
        let op = SignalOp::Noise { scale: 1.0, octaves: 3, amplitude: 1.0, offset: 0.0, seed: 0xABCD, has_x: false, has_y: false, has_time: false };
        let a = run(&op, &[0.3, 0.7, 1.1], 1, 1, &ctx, &[]);
        let b = run(&op, &[0.3, 0.7, 1.1], 1, 1, &ctx, &[]);
        assert_eq!(a, b); // pure fn of t
        // Different seed => different output (overwhelmingly likely).
        let op2 = SignalOp::Noise { scale: 1.0, octaves: 3, amplitude: 1.0, offset: 0.0, seed: 0x1234, has_x: false, has_y: false, has_time: false };
        let c = run(&op2, &[0.3, 0.7, 1.1], 1, 1, &ctx, &[]);
        assert_ne!(a, c);
        for v in a {
            assert!(v.abs() <= 1.0 + 1e-6);
        }
    }

    #[test]
    fn noise_value_at_zero_is_zero_for_value_noise() {
        // value_noise at integer coord with x=y=0, t*scale=0 => lattice point.
        // Reproducibility is the real contract; just assert determinism here.
        let ctx = ResidentContext::default();
        let op = SignalOp::Noise { scale: 1.0, octaves: 1, amplitude: 1.0, offset: 0.5, seed: 7, has_x: false, has_y: false, has_time: false };
        let a = run(&op, &[0.0], 1, 1, &ctx, &[]);
        let b = run(&op, &[0.0], 1, 1, &ctx, &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn wander_uv_deterministic_bounded() {
        let ctx = grid(120.0);
        let op = SignalOp::Wander { radius: 0.5, speed: 0.25, smoothness: 2.0, seed: 99 };
        let a = run(&op, &[0.0, 1.0, 2.0], 1, 2, &ctx, &[]);
        let b = run(&op, &[0.0, 1.0, 2.0], 1, 2, &ctx, &[]);
        assert_eq!(a, b);
        assert_eq!(a.len(), 6); // 3 times * 2 channels
        for v in &a {
            assert!(v.abs() <= 1.0 + 1e-6);
        }
        // U and V channels differ (independent seeds).
        assert!((a[2] - a[3]).abs() > 0.0 || (a[0] - a[1]).abs() > 0.0);
    }

    #[test]
    fn circle_traces_unit_circle() {
        // speed=1 cycle/beat, 60bpm => 1 beat/sec. At t=0 => (radius, 0).
        let ctx = grid(60.0);
        let op = SignalOp::Circle { radius: 1.0, speed: 1.0 };
        let out = run(&op, &[0.0, 0.25], 1, 2, &ctx, &[]);
        assert!((out[0] - 1.0).abs() < 1e-5); // cos 0
        assert!((out[1] - 0.0).abs() < 1e-5); // sin 0
        // t=0.25s = 0.25 beat = quarter cycle => (0, 1).
        assert!((out[2] - 0.0).abs() < 1e-4);
        assert!((out[3] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn sweep_projects_along_angle() {
        // angle 90deg => projects onto V. At quarter cycle sin=1 => (0, range).
        let ctx = grid(60.0);
        let op = SignalOp::Sweep { angle_deg: 90.0, range: 1.0, speed: 1.0 };
        let out = run(&op, &[0.25], 1, 2, &ctx, &[]);
        assert!(out[0].abs() < 1e-4); // cos90 * val ~ 0
        assert!((out[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn figure8_at_origin() {
        let ctx = grid(60.0);
        let op = SignalOp::Figure8 { width: 1.0, height: 0.5, speed: 1.0 };
        let out = run(&op, &[0.0], 1, 2, &ctx, &[]);
        assert!((out[0] - 1.0).abs() < 1e-5); // cos0 * width
        assert!(out[1].abs() < 1e-5); // sin0 * height
    }

    #[test]
    fn adsr_peaks_at_pulse() {
        // Single pulse at t=1.0, full attack weight => peak (=1.0*amp) at the pulse.
        let ctx = ResidentContext::default();
        let params = AdsrParams {
            attack: 0.25,
            decay: 0.25,
            sustain: 0.25,
            release: 0.25,
            sustain_level: 0.5,
            a_curve: 0.0,
            d_curve: 0.0,
            amp: 1.0,
            fit_to_gap: false,
            length_beats: 1.0,
            bpm: 60.0, // 1 beat = 1s span
        };
        let op = SignalOp::Adsr { pulse_starts: vec![1.0], params };
        // Before the pulse-attack window: 0. At the pulse: attack complete => 1.0.
        let out = run(&op, &[0.0, 1.0, 5.0], 1, 1, &ctx, &[]);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 1.0).abs() < 1e-3); // peak at the event
        assert!((out[2] - 0.0).abs() < 1e-5); // long after release
    }

    #[test]
    fn adsr_from_grid_pulses_nonzero() {
        // beat_envelope = beat_pulses + adsr: compute grid pulses, feed the Adsr op.
        let ctx = grid(120.0);
        let params = AdsrParams {
            attack: 0.3,
            decay: 0.2,
            sustain: 0.3,
            release: 0.2,
            sustain_level: 0.7,
            a_curve: 0.0,
            d_curve: 0.0,
            amp: 1.0,
            fit_to_gap: true,
            length_beats: 1.0,
            bpm: 120.0,
        };
        let pulse_starts = beat_grid_pulses(ctx.beat_grid.as_ref().unwrap(), 1.0, 0.0, false);
        let op = SignalOp::Adsr { pulse_starts, params };
        // Sample across a couple beats; envelope must be non-trivial.
        let times: Vec<f32> = (0..40).map(|i| i as f32 * 0.05).collect();
        let out = run(&op, &times, 1, 1, &ctx, &[]);
        assert!(out.iter().any(|&v| v > 0.5));
    }

    #[test]
    fn beat_pulses_spikes_on_beats() {
        let ctx = grid(120.0); // beats at 0, 0.5, 1.0, ...
        let op = SignalOp::BeatPulses { subdivision: 1.0, offset: 0.0, only_downbeats: false, tol: 0.01 };
        let out = run(&op, &[0.0, 0.25, 0.5], 1, 1, &ctx, &[]);
        assert_eq!(out[0], 1.0); // on beat 0
        assert_eq!(out[1], 0.0); // between beats
        assert_eq!(out[2], 1.0); // on beat 1 (t=0.5)
    }

    #[test]
    fn normalize_uses_frozen_stats() {
        // input [0, 5, 10], frozen [min=0, max=10] => [0, 0.5, 1.0].
        let mut ctx = ResidentContext::default();
        ctx.frozen = vec![0.0, 10.0];
        let data = vec![0.0, 5.0, 10.0];
        let view = InputView { data: &data, spec: SlotSpec { n: 1, c: 1 } };
        let out = run(&SignalOp::Normalize { stat_idx: 0 }, &[0.0, 1.0, 2.0], 1, 1, &ctx, std::slice::from_ref(&view));
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_degenerate_range_is_zero() {
        let mut ctx = ResidentContext::default();
        ctx.frozen = vec![3.0, 3.0];
        let data = vec![3.0, 3.0];
        let view = InputView { data: &data, spec: SlotSpec { n: 1, c: 1 } };
        let out = run(&SignalOp::Normalize { stat_idx: 0 }, &[0.0, 1.0], 1, 1, &ctx, std::slice::from_ref(&view));
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn invert_reflects_around_frozen_mid() {
        // frozen [0,10] => mid 5. input 2 => 8, input 7 => 3.
        let mut ctx = ResidentContext::default();
        ctx.frozen = vec![0.0, 10.0];
        let data = vec![2.0, 7.0];
        let view = InputView { data: &data, spec: SlotSpec { n: 1, c: 1 } };
        let out = run(&SignalOp::Invert { stat_idx: 0 }, &[0.0, 1.0], 1, 1, &ctx, std::slice::from_ref(&view));
        assert!((out[0] - 8.0).abs() < 1e-5);
        assert!((out[1] - 3.0).abs() < 1e-5);
    }

    #[test]
    fn falloff_shapes_normalized_input() {
        let ctx = ResidentContext::default();
        // width=1, linear curve => identity on [0,1].
        let data = vec![0.0, 0.5, 1.0];
        let view = InputView { data: &data, spec: SlotSpec { n: 1, c: 1 } };
        let out = run(&SignalOp::Falloff { width: 1.0, curve: 0.0 }, &[0.0, 1.0, 2.0], 1, 1, &ctx, std::slice::from_ref(&view));
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
        // width=2 doubles before clamp: 0.5 -> 1.0.
        let out2 = run(&SignalOp::Falloff { width: 2.0, curve: 0.0 }, &[0.0], 1, 1, &ctx, &[InputView { data: &[0.5], spec: SlotSpec { n: 1, c: 1 } }]);
        assert!((out2[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn time_delay_is_identity_passthrough() {
        let ctx = ResidentContext::default();
        let data = vec![0.1, 0.2, 0.3];
        let view = InputView { data: &data, spec: SlotSpec { n: 1, c: 1 } };
        let out = run(&SignalOp::TimeDelay { delay: 0.5 }, &[0.0, 1.0, 2.0], 1, 1, &ctx, std::slice::from_ref(&view));
        assert_eq!(out, data);
    }
}
