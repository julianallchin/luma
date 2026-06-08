//! Color ops — palettes, gradients, OKLab interpolation, chroma mapping. Output
//! is typically `c=3` (RGB). Port agent D2 extends this file. Reference impls:
//! legacy `node_graph/nodes/color.rs` + `node_graph/oklab.rs` (reuse the OKLab
//! conversions — do not reimplement).
//!
//! Stops-producing legacy nodes (`palette`, `gradient`) don't emit a per-frame
//! signal slot by themselves — they author a `Stops` color function consumed by
//! `sample_palette` / `chroma_palette`. In the eval IR there is no `Stops` slot
//! type, so the compiler inlines the upstream node's `Stops` into the consuming
//! op's variant (`SamplePalette { stops }`, `ChromaPalette { colors }`). For
//! parity / preview the `Palette` and `Gradient` ops are still provided: they
//! bake the `Stops` into an RGB(A) signal — `Palette` emits one row per stop
//! color (the discrete K-color set), `Gradient` emits the color function sampled
//! uniformly across the time axis.

use super::KernelCtx;
use crate::models::node_graph::Stops;
// OKLab color science is reused transitively via `Stops::sample` /
// `Stops::sample_uniform` (see `models::node_graph::Stops`), which call
// `node_graph::oklab::{srgb_to_oklab, oklab_to_srgb}`. We do not reimplement it.

#[derive(Clone, Debug)]
pub enum ColorOp {
    /// Constant RGBA broadcast to all primitives/times. Output `c` channels
    /// (take the first `c` of the rgba). Legacy `color` node.
    Constant([f32; 4]),

    /// Legacy `palette` node — an ordered set of K colors as uniformly-spaced
    /// `Stops`. Baked here to an RGB(A) signal of `n == K` rows (one per stop
    /// color, positions discarded), broadcast across the time axis. Lets a
    /// palette drive a per-primitive discrete color set directly.
    Palette(Stops),

    /// Legacy `gradient` node — a continuous color function. Baked to an RGB(A)
    /// signal sampled uniformly across the time axis (`u = k / (t-1)`), `n == 1`.
    /// For single-frame eval (`t == 1`) this is the color at `u = 0`.
    Gradient(Stops),

    /// Legacy `sample_palette` node — sample a `Stops` function (inlined by the
    /// compiler from the upstream `palette`/`gradient`) at the position carried
    /// by input 0 (`u`, channel 0). OKLab interpolation via `Stops::sample`.
    /// Output `c` channels (RGB, or RGBA when `c == 4`), shape follows `u`.
    SamplePalette { stops: Stops },

    /// Legacy `rainbow` node — map input 0 (channel 0) through a full HSL hue
    /// cycle: `hue = fract(v * spread + offset)`, lightness 0.5. Output RGB(A),
    /// shape follows the input. With no input wired the compiler emits a `Ramp`
    /// feeding this op, so the kernel itself always has an input.
    Rainbow {
        offset: f32,
        spread: f32,
        saturation: f32,
    },

    /// Legacy `chroma_palette` ("Harmonic Palette") node — map a 12-channel
    /// chroma vector (input 0) onto 12 palette colors and emit the
    /// probability-weighted, max-normalized RGB(A) mix. The 12 colors are
    /// inlined by the compiler (12 uniform samples of the upstream `Stops`, or
    /// the default rainbow). Output `n == 1` (chroma is a global signal), shape
    /// follows the chroma input's time axis.
    ///
    /// DEPENDENCY: needs a 12-channel chroma signal slot as input 0. The audio /
    /// compiler agent must provide it (legacy sourced it from a `chroma`
    /// analysis node). If the input is not 12-channel the op emits black.
    ChromaPalette { colors: [[f32; 4]; 12] },

    /// Compiler-emitted from the `apply_color` lowering. HSV "Value" of the c=3
    /// input color: `v = max(r,g,b)` → c=1. Becomes the dimmer signal (legacy
    /// `apply.rs:165`). Shape follows the input's `n`/`t`.
    HsvValue,

    /// Compiler-emitted from the `apply_color` lowering. Normalize the c=3 input
    /// color by its HSV value so the max channel is 1.0 — `(r,g,b)/v`, or black
    /// when `v <= 1e-5` (legacy `apply.rs:166`). Brightness moves to the dimmer
    /// (via `HsvValue`); this carries the saturation-preserving color. Output c=3.
    HsvNormalize,
}

pub fn run_color(op: &ColorOp, ctx: &KernelCtx) -> Vec<f32> {
    let (t, n, c) = (ctx.t(), ctx.n(), ctx.c());
    match op {
        ColorOp::Constant(rgba) => {
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = rgba[ch.min(3)];
                    }
                }
            }
            out
        }

        ColorOp::Palette(stops) => {
            // One row per stop color (positions discarded), broadcast over time.
            // Output `n` rows; if `n` exceeds the stop count we sample uniformly
            // so the op still fills its slot deterministically.
            let mut out = ctx.out_buf();
            let colors = stops.colors();
            for i in 0..n {
                let rgba = pick_palette_color(&colors, stops, i, n);
                for k in 0..t {
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = rgba[ch.min(3)];
                    }
                }
            }
            out
        }

        ColorOp::Gradient(stops) => {
            // Color function sampled uniformly across the time axis.
            let mut out = ctx.out_buf();
            for k in 0..t {
                let u = if t <= 1 { 0.0 } else { k as f32 / (t - 1) as f32 };
                let rgba = stops.sample(u);
                for i in 0..n {
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = rgba[ch.min(3)];
                    }
                }
            }
            out
        }

        ColorOp::SamplePalette { stops } => {
            let mut out = ctx.out_buf();
            let u_in = ctx.input(0);
            for i in 0..n {
                for k in 0..t {
                    let u = u_in.at(i, k, 0, t);
                    let rgba = stops.sample(u);
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = rgba[ch.min(3)];
                    }
                }
            }
            out
        }

        ColorOp::Rainbow {
            offset,
            spread,
            saturation,
        } => {
            let mut out = ctx.out_buf();
            let sig = ctx.input(0);
            let sat = saturation.clamp(0.0, 1.0);
            for i in 0..n {
                for k in 0..t {
                    let v = sig.at(i, k, 0, t);
                    let mut hue = (v * spread + offset).fract();
                    if hue < 0.0 {
                        hue += 1.0;
                    }
                    let (r, g, b) = hsl_to_rgb(hue, sat, 0.5);
                    let rgba = [r, g, b, 1.0];
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = rgba[ch.min(3)];
                    }
                }
            }
            out
        }

        ColorOp::ChromaPalette { colors } => {
            let mut out = ctx.out_buf();
            let chroma = ctx.input(0);
            // Chroma must be a 12-channel signal; otherwise emit black.
            if chroma.spec.c != 12 {
                return out;
            }
            for k in 0..t {
                let (mut r_sum, mut g_sum, mut b_sum) = (0.0f32, 0.0f32, 0.0f32);
                for pc in 0..12 {
                    let prob = chroma.at(0, k, pc, t);
                    r_sum += prob * colors[pc][0];
                    g_sum += prob * colors[pc][1];
                    b_sum += prob * colors[pc][2];
                }
                let max_val = r_sum.max(g_sum).max(b_sum).max(0.001);
                let scale = 1.0 / max_val;
                let rgba = [
                    (r_sum * scale).clamp(0.0, 1.0),
                    (g_sum * scale).clamp(0.0, 1.0),
                    (b_sum * scale).clamp(0.0, 1.0),
                    1.0,
                ];
                // n == 1 for chroma (global), but broadcast defensively over n.
                for i in 0..n {
                    for ch in 0..c {
                        out[ctx.out_idx(i, k, ch)] = rgba[ch.min(3)];
                    }
                }
            }
            out
        }

        ColorOp::HsvValue => {
            // dimmer = max(r,g,b) of the c=3 input color. Output c=1.
            let sig = ctx.input(0);
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    let v = sig
                        .at(i, k, 0, t)
                        .max(sig.at(i, k, 1, t))
                        .max(sig.at(i, k, 2, t));
                    out[ctx.out_idx(i, k, 0)] = v;
                }
            }
            out
        }

        ColorOp::HsvNormalize => {
            // color / v, max channel -> 1.0; black when v <= 1e-5. Output c=3.
            let sig = ctx.input(0);
            let mut out = ctx.out_buf();
            for i in 0..n {
                for k in 0..t {
                    let (r, g, b) = (sig.at(i, k, 0, t), sig.at(i, k, 1, t), sig.at(i, k, 2, t));
                    let v = r.max(g).max(b);
                    let (nr, ng, nb) = if v > 1e-5 {
                        (r / v, g / v, b / v)
                    } else {
                        (0.0, 0.0, 0.0)
                    };
                    out[ctx.out_idx(i, k, 0)] = nr;
                    if c > 1 {
                        out[ctx.out_idx(i, k, 1)] = ng;
                    }
                    if c > 2 {
                        out[ctx.out_idx(i, k, 2)] = nb;
                    }
                }
            }
            out
        }
    }
}

/// Pick the color for palette row `i` of `n`. When `n` matches the stop count,
/// return stop `i` verbatim (discrete K-color set). Otherwise sample the Stops
/// function uniformly so larger/smaller `n` still fills deterministically.
fn pick_palette_color(colors: &[[f32; 4]], stops: &Stops, i: usize, n: usize) -> [f32; 4] {
    if colors.is_empty() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if n == colors.len() {
        return colors[i];
    }
    let u = if n <= 1 { 0.0 } else { i as f32 / (n - 1) as f32 };
    stops.sample(u)
}

/// HSL → RGB (matches legacy `rainbow` exactly: sRGB-space, lightness mix).
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    (
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0),
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

/// Default 12-color chroma rainbow used by `ChromaPalette` when no `Stops` is
/// wired. Mirrors legacy `DEFAULT_CHROMA_RAINBOW_HEX`.
pub fn default_chroma_rainbow() -> [[f32; 4]; 12] {
    const HEX: [&str; 12] = [
        "#ff0000", "#ff8000", "#ffcc00", "#ffff00", "#80ff00", "#00ff00", "#00ff80", "#00ffff",
        "#0080ff", "#0000ff", "#8000ff", "#ff0080",
    ];
    let mut out = [[0.0; 4]; 12];
    for (i, hex) in HEX.iter().enumerate() {
        out[i] = parse_hex_rgba(hex);
    }
    out
}

/// Parse a `#RRGGBB` / `#RRGGBBAA` hex string into normalized RGBA. Mirrors
/// `node_graph::context::parse_hex_color` (which is in a private module we can't
/// reach from `eval`); this is plain byte arithmetic, not color science.
fn parse_hex_rgba(hex: &str) -> [f32; 4] {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f32 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f32 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f32 / 255.0;
        let a = if hex.len() >= 8 {
            u8::from_str_radix(&hex[6..8], 16).unwrap_or(255) as f32 / 255.0
        } else {
            1.0
        };
        [r, g, b, a]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::ops::InputView;
    use crate::eval::{ResidentContext, SlotSpec};

    fn ctx<'a>(
        inputs: &'a [InputView<'a>],
        times: &'a [f32],
        n: u32,
        c: u32,
        rc: &'a ResidentContext,
    ) -> KernelCtx<'a> {
        KernelCtx {
            inputs,
            out_spec: SlotSpec { n, c },
            times,
            ctx: rc,
        }
    }

    fn stops(pairs: &[(f32, [f32; 4])]) -> Stops {
        Stops {
            stops: pairs.to_vec(),
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn constant_fills_rgb() {
        let rc = ResidentContext::default();
        let times = [0.0];
        let kc = ctx(&[], &times, 2, 3, &rc);
        let out = run_color(&ColorOp::Constant([0.25, 0.5, 0.75, 1.0]), &kc);
        // n=2 rows, t=1, c=3.
        assert_eq!(out.len(), 6);
        for row in 0..2 {
            assert!(approx(out[row * 3], 0.25));
            assert!(approx(out[row * 3 + 1], 0.5));
            assert!(approx(out[row * 3 + 2], 0.75));
        }
    }

    #[test]
    fn palette_emits_one_row_per_color() {
        let rc = ResidentContext::default();
        let times = [0.0];
        // 2 colors, n == 2: rows are the stop colors verbatim.
        let p = stops(&[
            (0.0, [1.0, 0.0, 0.0, 1.0]),
            (1.0, [0.0, 0.0, 1.0, 1.0]),
        ]);
        let kc = ctx(&[], &times, 2, 3, &rc);
        let out = run_color(&ColorOp::Palette(p), &kc);
        assert_eq!(out.len(), 6);
        // row 0 = red.
        assert!(approx(out[0], 1.0) && approx(out[1], 0.0) && approx(out[2], 0.0));
        // row 1 = blue.
        assert!(approx(out[3], 0.0) && approx(out[4], 0.0) && approx(out[5], 1.0));
    }

    #[test]
    fn gradient_black_to_white_midpoint_grey() {
        // 2-stop black→white gradient, t=3 → u = 0, 0.5, 1.
        // OKLab midpoint of black/white is mid-lightness grey (~0.5 L). The
        // sRGB value is the OKLab-linear mix round-tripped; assert monotonic +
        // endpoints + grey neutrality at the midpoint.
        let rc = ResidentContext::default();
        let times = [0.0, 1.0, 2.0];
        let g = stops(&[
            (0.0, [0.0, 0.0, 0.0, 1.0]),
            (1.0, [1.0, 1.0, 1.0, 1.0]),
        ]);
        let kc = ctx(&[], &times, 1, 3, &rc);
        let out = run_color(&ColorOp::Gradient(g), &kc);
        assert_eq!(out.len(), 9); // n=1, t=3, c=3
        // k=0 → black.
        assert!(approx(out[0], 0.0) && approx(out[1], 0.0) && approx(out[2], 0.0));
        // k=2 → white.
        assert!(approx(out[6], 1.0) && approx(out[7], 1.0) && approx(out[8], 1.0));
        // k=1 → neutral grey, strictly between.
        let (r, gc, b) = (out[3], out[4], out[5]);
        assert!(approx(r, gc) && approx(gc, b)); // neutral
        assert!(r > 0.0 && r < 1.0);
        // Cross-check against Stops::sample at u=0.5.
        let mid = stops(&[
            (0.0, [0.0, 0.0, 0.0, 1.0]),
            (1.0, [1.0, 1.0, 1.0, 1.0]),
        ])
        .sample(0.5);
        assert!(approx(r, mid[0]));
    }

    #[test]
    fn sample_palette_follows_u_input() {
        // u input: n=1, t=2, c=1 with values [0.0, 1.0] → red then blue.
        let rc = ResidentContext::default();
        let times = [0.0, 1.0];
        let u_data = [0.0f32, 1.0];
        let u_view = InputView {
            data: &u_data,
            spec: SlotSpec { n: 1, c: 1 },
        };
        let inputs = [u_view];
        let kc = ctx(&inputs, &times, 1, 3, &rc);
        let p = stops(&[
            (0.0, [1.0, 0.0, 0.0, 1.0]),
            (1.0, [0.0, 0.0, 1.0, 1.0]),
        ]);
        let out = run_color(&ColorOp::SamplePalette { stops: p }, &kc);
        assert_eq!(out.len(), 6); // n=1, t=2, c=3
        // k=0 (u=0) → red.
        assert!(approx(out[0], 1.0) && approx(out[1], 0.0) && approx(out[2], 0.0));
        // k=1 (u=1) → blue.
        assert!(approx(out[3], 0.0) && approx(out[4], 0.0) && approx(out[5], 1.0));
    }

    #[test]
    fn rainbow_hue_endpoints() {
        // Input v=0 → hue 0 → red. v=2/3 (with spread 1) → hue 2/3 → blue.
        let rc = ResidentContext::default();
        let times = [0.0, 1.0];
        let v_data = [0.0f32, 2.0 / 3.0];
        let v_view = InputView {
            data: &v_data,
            spec: SlotSpec { n: 1, c: 1 },
        };
        let inputs = [v_view];
        let kc = ctx(&inputs, &times, 1, 3, &rc);
        let out = run_color(
            &ColorOp::Rainbow {
                offset: 0.0,
                spread: 1.0,
                saturation: 1.0,
            },
            &kc,
        );
        // hue 0, s 1, l 0.5 → pure red (1,0,0).
        assert!(approx(out[0], 1.0) && approx(out[1], 0.0) && approx(out[2], 0.0));
        // hue 2/3 → pure blue (0,0,1).
        assert!(approx(out[3], 0.0) && approx(out[4], 0.0) && approx(out[5], 1.0));
    }

    #[test]
    fn chroma_palette_weighted_mix() {
        // Chroma all-zero except pitch class 0 (red) full → output red.
        let rc = ResidentContext::default();
        let times = [0.0];
        let mut chroma_data = vec![0.0f32; 12];
        chroma_data[0] = 1.0; // PC 0 → red in default rainbow.
        let chroma_view = InputView {
            data: &chroma_data,
            spec: SlotSpec { n: 1, c: 12 },
        };
        let inputs = [chroma_view];
        let kc = ctx(&inputs, &times, 1, 3, &rc);
        let out = run_color(
            &ColorOp::ChromaPalette {
                colors: default_chroma_rainbow(),
            },
            &kc,
        );
        assert_eq!(out.len(), 3);
        // r_sum=1, others 0 → max-normalized red.
        assert!(approx(out[0], 1.0) && approx(out[1], 0.0) && approx(out[2], 0.0));
    }

    #[test]
    fn chroma_palette_two_pc_normalized() {
        // PC0 red (0.5) + PC9 blue (0.5). r_sum=0.5, b_sum=0.5, max=0.5 →
        // scale=2 → red 1.0, blue 1.0 (magenta).
        let rc = ResidentContext::default();
        let times = [0.0];
        let mut chroma_data = vec![0.0f32; 12];
        chroma_data[0] = 0.5; // red
        chroma_data[9] = 0.5; // blue (#0000ff)
        let chroma_view = InputView {
            data: &chroma_data,
            spec: SlotSpec { n: 1, c: 12 },
        };
        let inputs = [chroma_view];
        let kc = ctx(&inputs, &times, 1, 3, &rc);
        let out = run_color(
            &ColorOp::ChromaPalette {
                colors: default_chroma_rainbow(),
            },
            &kc,
        );
        assert!(approx(out[0], 1.0) && approx(out[1], 0.0) && approx(out[2], 1.0));
    }

    #[test]
    fn chroma_palette_wrong_channels_is_black() {
        let rc = ResidentContext::default();
        let times = [0.0];
        let bad = [0.5f32];
        let bad_view = InputView {
            data: &bad,
            spec: SlotSpec { n: 1, c: 1 },
        };
        let inputs = [bad_view];
        let kc = ctx(&inputs, &times, 1, 3, &rc);
        let out = run_color(
            &ColorOp::ChromaPalette {
                colors: default_chroma_rainbow(),
            },
            &kc,
        );
        assert!(out.iter().all(|&v| v == 0.0));
    }
}
