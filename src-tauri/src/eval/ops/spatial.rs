//! Spatial / geometry ops — per-primitive values from fixture layout & selection.
//! All prologue (t-invariant): each op derives a per-primitive scalar from
//! `ctx.ctx.positions` (world `[x,y,z]` per primitive) and the selection's
//! bounding geometry, constant across the time axis. Output is `c=1`, same
//! value broadcast across every `k` in `0..t`.
//!
//! Reference impls (math matched 1:1): legacy `node_graph/nodes/selection.rs`
//! `get_attribute` (the `pos_*`/`rel_*`/`index`/`normalized_index`/
//! `angular_*`/`circle_radius` attribute table) and `mirror`, plus
//! `node_graph/circle_fit.rs` for the PCA+RANSAC angular fit.

use super::KernelCtx;
use crate::node_graph::circle_fit;
use crate::node_graph::oklab::oklab_to_srgb;

/// World axis selector. `pos_*`/`rel_*`/`mirror` all parameterize over this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// Component of a `[x,y,z]` position along this axis.
    #[inline]
    fn pick(self, p: [f32; 3]) -> f32 {
        match self {
            Axis::X => p[0],
            Axis::Y => p[1],
            Axis::Z => p[2],
        }
    }
}

#[derive(Clone, Debug)]
pub enum SpatialOp {
    /// Raw world coordinate of each primitive along `axis` (legacy `pos_x/y/z`,
    /// also the `x`/`y`/`z` aliases).
    Pos(Axis),
    /// World coordinate normalized to `0..1` across the selection's bounding box
    /// along `axis` (legacy `rel_x/y/z`). `(p_axis - min) / range`, `range`
    /// floored at `1e-3` to avoid div-by-zero on a degenerate axis.
    Rel(Axis),
    /// Rig-intrinsic horizontal coordinate: position along the selection's
    /// dominant horizontal axis, normalized `0..1`. See [`rig_uv`].
    U,
    /// Rig-intrinsic vertical coordinate: world height normalized `0..1` over
    /// the selection. See [`rig_uv`].
    V,
    /// `Rel` along the axis with the largest physical extent (legacy
    /// `rel_major_span`; tie-break X > Y > Z).
    RelMajorSpan,
    /// `Rel` along the axis with the most distinct head positions, rounded to the
    /// millimetre (legacy `rel_major_count`; tie-break X > Y > Z).
    RelMajorCount,
    /// Per-primitive integer index `0,1,2,…` (legacy `index`).
    Index,
    /// Per-primitive index normalized to `0..1` (legacy `normalized_index`).
    NormalizedIndex,
    /// Integer rank of each primitive sorted by its angle (`atan2(dy,dx)`) around
    /// the XY centroid; coincident primitives share a rank (legacy
    /// `angular_index`). Pure angle sort, no circle fit.
    AngularIndex,
    /// Angular position `0..1` around the PCA+RANSAC fitted circle (legacy
    /// `angular_position`). Falls back to the centroid `atan2` angle when the
    /// fit fails (< 3 primitives or collinear).
    AngularPosition,
    /// Distance of each primitive from the XY centroid (legacy `circle_radius`).
    /// Raw world distance, not normalized.
    CircleRadius,
    /// Fold side along `axis`: `+1` past the mean, `-1` before it, `0` on it
    /// (legacy `mirror`'s `side` output). The mean is snapped to exactly `0`
    /// when within `0.1` of the origin, matching the legacy node.
    Mirror(Axis),
    /// Folded **positions** (legacy `mirror`'s `out` output): each primitive's
    /// `axis` coordinate reflected to `|pos_axis - center|` (center = mean along
    /// the axis, snapped to `0` within `0.1`). Outputs a `c=3` position slot that
    /// a downstream spatial op consumes as a position override — that's how a
    /// gradient becomes center-symmetric. Other axes pass through.
    Fold(Axis),
    /// Read a named per-fixture attribute. STUBBED: `ResidentContext` carries no
    /// per-fixture attribute table, so this returns `0.0` for every primitive.
    /// See the module note + report for the field that would be required.
    GetAttribute(String),
    /// Soft-voronoi color field: `K` colored seeds wander through the fixture
    /// bounding box; each fixture's color is a soft-min (softmax over `-d/T`)
    /// blend of the seed colors in OKLab. Output is `c=4` RGBA, time-varying.
    ///
    /// Unlike the legacy node — which integrated an N-body repulsion sim forward
    /// (path-dependent, not seek-safe) — the seeds here are a **pure function of
    /// `t`**: an R2 low-discrepancy base (evenly spread by construction, so no
    /// repulsion is needed to maintain spacing) under a slow per-seed drift +
    /// bounded wander, triangle-folded into the bbox (reflecting at the walls).
    /// The coloring (softmin + OKLab blend + chroma rescue) is ported 1:1. Seed
    /// colors are baked from the palette at compile.
    SoftVoronoi {
        num_points: usize,
        softness: f32,
        vibrance: f32,
        wander_speed: f32,
        seed: u64,
        /// Per-seed OKLab color `(L, a, b, alpha)`, baked from the stops at compile.
        lab_palette: Vec<[f32; 4]>,
        /// Per-seed chroma magnitude `sqrt(a²+b²)` (for the vibrance rescue).
        lab_chroma: Vec<f32>,
    },
}

// --- soft_voronoi: pure-of-t seed motion + ported OKLab coloring ---

/// Generalized golden ratio for the 3D R2 low-discrepancy sequence (root of
/// `x⁴ = x + 1`). Successive `k·αd` are maximally spread → an even seed base
/// with no clumping, which is the job repulsion did in the legacy sim.
const R2_G: f64 = 1.220_744_084_605_759_5;
/// Per-axis wander periods (s), mutually incommensurate primes (Kronecker–Weyl
/// → dense coverage), carried over from the legacy `TARGET_PERIODS_SEC`.
const SV_PERIODS: [f32; 3] = [17.0, 13.0, 11.0];
/// Bounded per-seed wander amplitude, as a fraction of each axis range. Small
/// enough that the even R2 base is preserved (seeds jiggle + locally swap but
/// can't drift into clumps).
const SV_WANDER_AMP: f32 = 0.16;
/// Slow per-seed linear drift rate (fraction of axis range per second at
/// `wander_speed = 1`). Drives the gradual permutation / region swapping.
const SV_DRIFT_RATE: f32 = 0.05;
/// Chroma-rescue boost cap (ported `MAX_CHROMA_BOOST`).
const SV_MAX_CHROMA_BOOST: f32 = 10.0;

#[inline]
fn sv_hash64(seed: u64, v: u64) -> u64 {
    let mut x = seed ^ v.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}
#[inline]
fn sv_hash01(seed: u64, v: u64) -> f32 {
    (sv_hash64(seed, v) as f64 / u64::MAX as f64) as f32
}

/// Triangle wave mapping `R → [0,1]`, period 2, reflecting at 0 and 1 — the
/// closed-form "bounce off the wall" that replaces the legacy bbox collision.
#[inline]
fn tri01(x: f32) -> f32 {
    let f = x.rem_euclid(2.0);
    if f > 1.0 {
        2.0 - f
    } else {
        f
    }
}

/// Vibrance chroma rescue (ported `apply_chroma_rescue`).
#[inline]
fn sv_chroma_rescue(a: &mut f32, b: &mut f32, c_now: f32, c_target: f32, vibrance: f32) {
    let c_final = c_now + (c_target - c_now) * vibrance;
    if c_now > 1e-6 {
        let scale = (c_final / c_now).min(SV_MAX_CHROMA_BOOST).max(0.0);
        *a *= scale;
        *b *= scale;
    }
}

#[allow(clippy::too_many_arguments)]
fn run_soft_voronoi(
    ctx: &KernelCtx,
    num_points: usize,
    softness: f32,
    vibrance: f32,
    wander_speed: f32,
    seed: u64,
    lab_palette: &[[f32; 4]],
    lab_chroma: &[f32],
) -> Vec<f32> {
    let (t, n) = (ctx.t(), ctx.n());
    let mut out = ctx.out_buf(); // n * t * 4
    if n == 0 || num_points == 0 {
        return out;
    }
    let positions = &ctx.ctx.positions;

    // Fixture bounding box + diagonal (softness is a fraction of the diagonal).
    let (min, max) = bounds(positions);
    let range = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let diag = (range[0] * range[0] + range[1] * range[1] + range[2] * range[2])
        .sqrt()
        .max(1e-4);
    let temperature = (softness * diag).max(1e-4);

    // Per-seed static parameters (t-invariant): R2 base, drift direction, phases.
    let a_r2 = [
        (1.0 / R2_G) as f32,
        (1.0 / (R2_G * R2_G)) as f32,
        (1.0 / (R2_G * R2_G * R2_G)) as f32,
    ];
    let mut base = vec![[0.0f32; 3]; num_points];
    let mut drift = vec![[0.0f32; 3]; num_points];
    let mut phase = vec![[0.0f32; 3]; num_points];
    for k in 0..num_points {
        for d in 0..3 {
            // R2 lattice, shifted as a whole by a seed-derived offset (keeps the
            // even spacing; just decorrelates different nodes/instances).
            let off = sv_hash01(seed, d as u64);
            base[k][d] = (off + (k as f32 + 1.0) * a_r2[d]).rem_euclid(1.0);
            drift[k][d] = (sv_hash01(seed, (k as u64) * 6 + d as u64) - 0.5) * 2.0;
            phase[k][d] = sv_hash01(seed, (k as u64) * 6 + 3 + d as u64) * std::f32::consts::TAU;
        }
    }

    let mut seed_pos = vec![[0.0f32; 3]; num_points];
    let mut weights = vec![0.0f32; num_points];

    for ki in 0..t {
        let tt = ctx.times[ki];
        // Seed positions at this instant — pure f(t).
        for k in 0..num_points {
            for d in 0..3 {
                let drift_term = SV_DRIFT_RATE * wander_speed * tt * drift[k][d];
                let wander = SV_WANDER_AMP
                    * (std::f32::consts::TAU * tt * wander_speed / SV_PERIODS[d] + phase[k][d])
                        .sin();
                let u = base[k][d] + drift_term + wander;
                seed_pos[k][d] = min[d] + range[d] * tri01(u);
            }
        }

        for i in 0..n {
            let p = positions[i];
            // Softmin weights via softmax over -d/T (subtract min for stability).
            let mut min_d = f32::INFINITY;
            for k in 0..num_points {
                let s = seed_pos[k];
                let dx = p[0] - s[0];
                let dy = p[1] - s[1];
                let dz = p[2] - s[2];
                let d = (dx * dx + dy * dy + dz * dz).sqrt();
                if d < min_d {
                    min_d = d;
                }
            }
            let mut wsum = 0.0f32;
            for k in 0..num_points {
                let s = seed_pos[k];
                let dx = p[0] - s[0];
                let dy = p[1] - s[1];
                let dz = p[2] - s[2];
                let d = (dx * dx + dy * dy + dz * dz).sqrt();
                let w = (-(d - min_d) / temperature).exp();
                weights[k] = w;
                wsum += w;
            }
            if wsum > 0.0 {
                for w in &mut weights {
                    *w /= wsum;
                }
            } else {
                weights[0] = 1.0;
                for w in weights.iter_mut().skip(1) {
                    *w = 0.0;
                }
            }

            // Weighted OKLab blend + chroma rescue (ported 1:1).
            let (mut l_out, mut a_out, mut b_out, mut alpha_out) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let mut chroma_target = 0.0f32;
            for k in 0..num_points {
                let [l, a, b, alpha] = lab_palette[k];
                let w = weights[k];
                l_out += w * l;
                a_out += w * a;
                b_out += w * b;
                alpha_out += w * alpha;
                chroma_target += w * lab_chroma[k];
            }
            let chroma_now = (a_out * a_out + b_out * b_out).sqrt();
            sv_chroma_rescue(&mut a_out, &mut b_out, chroma_now, chroma_target, vibrance);

            let (r, g, b) = oklab_to_srgb(l_out, a_out, b_out);
            out[ctx.out_idx(i, ki, 0)] = r.clamp(0.0, 1.0);
            out[ctx.out_idx(i, ki, 1)] = g.clamp(0.0, 1.0);
            out[ctx.out_idx(i, ki, 2)] = b.clamp(0.0, 1.0);
            out[ctx.out_idx(i, ki, 3)] = alpha_out.clamp(0.0, 1.0);
        }
    }
    out
}

/// Per-axis `(min, max)` of the primitive set, with each range floored at
/// `1e-3` (matches legacy `get_attribute` bounds handling).
fn bounds(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = positions[0];
    let mut max = positions[0];
    for &p in positions {
        for a in 0..3 {
            if p[a] < min[a] {
                min[a] = p[a];
            }
            if p[a] > max[a] {
                max[a] = p[a];
            }
        }
    }
    (min, max)
}

/// Minimum extent (metres) an axis must span before it is treated as a real
/// spread rather than a point. Also the floor applied to every `u`/`v` range, so
/// the degeneracy test and the division floor are the same number. Deliberately
/// a *physical* threshold (1 mm), not a numerical one — `circle_fit`'s `1e-10`
/// normalize guard would happily call a 0.1 mm jitter an axis.
const UV_MIN_EXTENT: f32 = 1e-3;

/// Rig-intrinsic pattern space: `(u, v)` per primitive, both normalized `0..1`
/// over the selection. Patterns authored against `u`/`v` ("chase along u") read
/// the same on any rig, however the venue happens to be oriented in world space.
///
/// **Gravity-pinned, not free 3D PCA.** `v` is always world height (Z); `u` is
/// the first principal component of the *horizontal* (XY) spread only. Gravity
/// is a real, shared axis for every rig — letting PCA choose a tilted "vertical"
/// would make a truss dip look like a sweep. So only the horizontal heading is
/// discovered; up stays up.
///
/// **Sign invariant.** The principal axis is only defined up to sign, so `u` is
/// canonicalized: the world component of largest magnitude is forced positive
/// (i.e. `u` points broadly `+X`, stage right, or `+Y` when the rig runs
/// front-to-back). Without this, an arbitrary eigenvector sign flip would
/// silently reverse every chase. The canonical direction is the representative
/// lying in the half-plane `(-45°, 135°]`, so `u` is stable under any rig
/// rotation inside that 180° window and reverses outside it — a rig turned all
/// the way around does run its chase the other way, and no sign rule can avoid
/// that (an axis has no head or tail; only the world does).
///
/// **Degenerate rigs.** If the whole selection is within [`UV_MIN_EXTENT`] in XY
/// (a single vertical stack, one fixture), there is no horizontal axis to find
/// and `u` falls back to normalized index. If the height range is degenerate
/// (one flat plane of fixtures, the common case), `v` is a constant `0.5` — a
/// midpoint reads better than `rel_z`'s pile-up at `0.0` for gradients keyed on
/// height. Near-collinear XY is *not* degenerate: a straight line of fixtures is
/// the ideal case — `u` is the line.
///
/// Scope: computed over the primitives handed to this evaluation, i.e. the
/// current selection — the same scope as `rel_*` and every other spatial op.
/// Venue-scoped `u`/`v` (a basis fitted once over the whole rig, plus a
/// per-group override) is the natural next step; it would slot in here as an
/// alternate source of `(centroid, axis)` with the projection below unchanged.
pub fn rig_uv(positions: &[[f32; 3]]) -> Vec<[f32; 2]> {
    let n = positions.len();
    if n == 0 {
        return Vec::new();
    }

    // --- v: world height, normalized over the selection ---
    let (min, max) = bounds(positions);
    let z_extent = max[2] - min[2];
    let v: Vec<f32> = if z_extent < UV_MIN_EXTENT {
        vec![0.5; n]
    } else {
        positions
            .iter()
            .map(|p| (p[2] - min[2]) / z_extent)
            .collect()
    };

    // --- u: dominant horizontal axis ---
    let xy_extent = (max[0] - min[0]).max(max[1] - min[1]);
    if xy_extent < UV_MIN_EXTENT {
        // No horizontal spread at all: fall back to patch order.
        return (0..n)
            .map(|i| {
                let u = if n <= 1 {
                    0.0
                } else {
                    i as f32 / (n - 1) as f32
                };
                [u, v[i]]
            })
            .collect();
    }

    let (cx, cy) = centroid_xy(positions);
    // 2×2 covariance of the centered XY cloud.
    let (mut sxx, mut sxy, mut syy) = (0.0f32, 0.0f32, 0.0f32);
    for p in positions {
        let dx = p[0] - cx;
        let dy = p[1] - cy;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // Dominant eigenvector, closed form. (`circle_fit::power_iteration` is the
    // 3×3 cousin, but it seeds at (1,0,0) and never escapes when the dominant
    // eigenvector is orthogonal to that — exactly the front-to-back rig we must
    // get right. 2×2 symmetric has an exact solution; use it.)
    let axis = if sxy.abs() < f32::EPSILON {
        if sxx >= syy {
            [1.0, 0.0]
        } else {
            [0.0, 1.0]
        }
    } else {
        let tr = sxx + syy;
        let det = sxx * syy - sxy * sxy;
        let lambda = 0.5 * (tr + (tr * tr - 4.0 * det).max(0.0).sqrt());
        let (ax, ay) = (lambda - syy, sxy);
        let len = (ax * ax + ay * ay).sqrt();
        [ax / len, ay / len]
    };
    // Sign canonicalization: the largest world component points positive.
    let flip = if axis[0].abs() >= axis[1].abs() {
        axis[0] < 0.0
    } else {
        axis[1] < 0.0
    };
    let axis = if flip { [-axis[0], -axis[1]] } else { axis };

    // Project onto the axis and normalize over the selection's own span.
    let s: Vec<f32> = positions
        .iter()
        .map(|p| (p[0] - cx) * axis[0] + (p[1] - cy) * axis[1])
        .collect();
    let s_min = s.iter().cloned().fold(f32::INFINITY, f32::min);
    let s_max = s.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let s_range = (s_max - s_min).max(UV_MIN_EXTENT);

    (0..n).map(|i| [(s[i] - s_min) / s_range, v[i]]).collect()
}

/// XY centroid of the primitive set.
fn centroid_xy(positions: &[[f32; 3]]) -> (f32, f32) {
    let n = positions.len() as f32;
    let sx: f32 = positions.iter().map(|p| p[0]).sum();
    let sy: f32 = positions.iter().map(|p| p[1]).sum();
    (sx / n, sy / n)
}

/// Normalized `0..1` angle around a center, starting at the top (matches legacy
/// `angular_index`/`angular_position` fallback convention).
#[inline]
fn top_angle01(dx: f32, dy: f32) -> f32 {
    use std::f32::consts::PI;
    let a = dy.atan2(dx);
    ((a + PI) / (2.0 * PI) + 0.25) % 1.0
}

pub fn run_spatial(op: &SpatialOp, ctx: &KernelCtx) -> Vec<f32> {
    // soft_voronoi is the one time-varying, multi-channel spatial op — it writes
    // the full n×t×4 buffer itself rather than a broadcast per-primitive scalar.
    if let SpatialOp::SoftVoronoi {
        num_points,
        softness,
        vibrance,
        wander_speed,
        seed,
        lab_palette,
        lab_chroma,
    } = op
    {
        return run_soft_voronoi(
            ctx,
            *num_points,
            *softness,
            *vibrance,
            *wander_speed,
            *seed,
            lab_palette,
            lab_chroma,
        );
    }

    let (t, n) = (ctx.t(), ctx.n());

    // Positions source: an upstream transform (e.g. `mirror`'s fold) feeds a
    // `c=3` position slot as input(0), which overrides the resident geometry.
    // Otherwise read `ResidentContext.positions` directly.
    let pos_override: Option<Vec<[f32; 3]>> = if !ctx.inputs.is_empty() && ctx.input(0).spec.c == 3
    {
        let inp = ctx.input(0);
        Some(
            (0..n)
                .map(|i| [inp.at(i, 0, 0, t), inp.at(i, 0, 1, t), inp.at(i, 0, 2, t)])
                .collect(),
        )
    } else {
        None
    };
    let positions: &[[f32; 3]] = match &pos_override {
        Some(v) => v,
        None => &ctx.ctx.positions,
    };

    // Fold writes a `c=3` position slot (folded along `axis`), broadcast over t.
    if let SpatialOp::Fold(axis) = op {
        let mut out = ctx.out_buf(); // n * t * 3
        if n > 0 {
            let ai = match axis {
                Axis::X => 0,
                Axis::Y => 1,
                Axis::Z => 2,
            };
            let mean = positions.iter().map(|p| p[ai]).sum::<f32>() / n as f32;
            let center = if mean.abs() < 0.1 { 0.0 } else { mean };
            for i in 0..n {
                let mut p = positions[i];
                p[ai] = (p[ai] - center).abs();
                for k in 0..t {
                    for ch in 0..3 {
                        out[ctx.out_idx(i, k, ch)] = p[ch];
                    }
                }
            }
        }
        return out;
    }

    let mut out = ctx.out_buf();

    // Compute one per-primitive value, then broadcast it across the time axis.
    let mut per_prim = vec![0.0f32; n];

    match op {
        SpatialOp::Pos(axis) => {
            for i in 0..n {
                per_prim[i] = axis.pick(positions[i]);
            }
        }
        SpatialOp::Rel(axis) => {
            if n > 0 {
                let (min, max) = bounds(positions);
                let a = match axis {
                    Axis::X => 0,
                    Axis::Y => 1,
                    Axis::Z => 2,
                };
                let range = (max[a] - min[a]).max(1e-3);
                for i in 0..n {
                    per_prim[i] = (positions[i][a] - min[a]) / range;
                }
            }
        }
        SpatialOp::U | SpatialOp::V => {
            // Note the position source above: a `mirror` fold upstream overrides
            // `ctx.positions`, so `u`/`v` are fitted to the *folded* rig. That's
            // the intended reading — folding is an authored restatement of the
            // geometry, and a chase along `u` should follow it out from the
            // centre like `rel_x` does.
            let ch = usize::from(matches!(op, SpatialOp::V));
            for (i, uv) in rig_uv(positions).into_iter().enumerate() {
                per_prim[i] = uv[ch];
            }
        }
        SpatialOp::RelMajorSpan | SpatialOp::RelMajorCount => {
            if n > 0 {
                let (min, max) = bounds(positions);
                let ranges = [
                    (max[0] - min[0]).max(1e-3),
                    (max[1] - min[1]).max(1e-3),
                    (max[2] - min[2]).max(1e-3),
                ];
                // Pick the major axis (tie-break X > Y > Z).
                let metric = match op {
                    SpatialOp::RelMajorCount => {
                        // Count distinct head positions per axis (mm-rounded).
                        let mut distinct = [
                            std::collections::HashSet::new(),
                            std::collections::HashSet::new(),
                            std::collections::HashSet::new(),
                        ];
                        for p in positions {
                            for a in 0..3 {
                                distinct[a].insert((p[a] * 1000.0).round() as i32);
                            }
                        }
                        [
                            distinct[0].len() as f32,
                            distinct[1].len() as f32,
                            distinct[2].len() as f32,
                        ]
                    }
                    _ => ranges, // RelMajorSpan: largest physical extent
                };
                let a = if metric[0] >= metric[1] && metric[0] >= metric[2] {
                    0
                } else if metric[1] >= metric[0] && metric[1] >= metric[2] {
                    1
                } else {
                    2
                };
                for i in 0..n {
                    per_prim[i] = (positions[i][a] - min[a]) / ranges[a];
                }
            }
        }
        SpatialOp::Index => {
            for i in 0..n {
                per_prim[i] = i as f32;
            }
        }
        SpatialOp::NormalizedIndex => {
            for i in 0..n {
                per_prim[i] = if n <= 1 {
                    0.0
                } else {
                    i as f32 / (n - 1) as f32
                };
            }
        }
        SpatialOp::AngularIndex => {
            if n > 0 {
                let (cx, cy) = centroid_xy(positions);
                let mut indexed: Vec<(usize, f32)> = (0..n)
                    .map(|i| {
                        let p = positions[i];
                        (i, top_angle01(p[0] - cx, p[1] - cy))
                    })
                    .collect();
                indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                // Same rank for primitives at (near-)coincident positions — e.g.
                // a mirrored pair — matching the legacy 0.5-unit epsilon.
                let pos_epsilon = 0.5_f32;
                let mut rank = 0usize;
                for k in 0..indexed.len() {
                    let orig = indexed[k].0;
                    if k > 0 {
                        let prev = positions[indexed[k - 1].0];
                        let curr = positions[orig];
                        let dx = curr[0] - prev[0];
                        let dy = curr[1] - prev[1];
                        let dz = curr[2] - prev[2];
                        if (dx * dx + dy * dy + dz * dz).sqrt() > pos_epsilon {
                            rank += 1;
                        }
                    }
                    per_prim[orig] = rank as f32;
                }
            }
        }
        SpatialOp::AngularPosition => {
            if n > 0 {
                let pts: Vec<(f32, f32, f32)> =
                    positions.iter().map(|p| (p[0], p[1], p[2])).collect();
                let fit = circle_fit::fit_circle_3d(&pts);
                let (cx, cy) = centroid_xy(positions);
                for i in 0..n {
                    per_prim[i] = match &fit {
                        Some(f) => f.angular_positions.get(i).copied().unwrap_or(0.0),
                        None => {
                            let p = positions[i];
                            top_angle01(p[0] - cx, p[1] - cy)
                        }
                    };
                }
            }
        }
        SpatialOp::CircleRadius => {
            if n > 0 {
                let (cx, cy) = centroid_xy(positions);
                for i in 0..n {
                    let p = positions[i];
                    let dx = p[0] - cx;
                    let dy = p[1] - cy;
                    per_prim[i] = (dx * dx + dy * dy).sqrt();
                }
            }
        }
        SpatialOp::Mirror(axis) => {
            if n > 0 {
                let sum: f32 = positions.iter().map(|p| axis.pick(*p)).sum();
                let mean = sum / n as f32;
                let center = if mean.abs() < 0.1 { 0.0 } else { mean };
                let epsilon = 0.5_f32;
                for i in 0..n {
                    let v = axis.pick(positions[i]);
                    per_prim[i] = if v > center + epsilon {
                        1.0
                    } else if v < center - epsilon {
                        -1.0
                    } else {
                        0.0
                    };
                }
            }
        }
        SpatialOp::GetAttribute(_attr) => {
            // STUB: no per-fixture attribute data in ResidentContext. Leaves
            // per_prim all-zero. See report.
        }
        SpatialOp::SoftVoronoi { .. } | SpatialOp::Fold(_) => {
            unreachable!("handled before the broadcast match")
        }
    }

    for i in 0..n {
        for k in 0..t {
            out[ctx.out_idx(i, k, 0)] = per_prim[i];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::ops::InputView;
    use crate::eval::{ResidentContext, SlotSpec};

    /// A `ResidentContext` carrying just the given primitive positions.
    fn ctx_with(positions: Vec<[f32; 3]>) -> ResidentContext {
        ResidentContext {
            positions,
            ..Default::default()
        }
    }

    /// Run a spatial op against `rc` at `t=1`, returning the per-primitive value
    /// (every k is identical for spatial ops; the broadcast is tested separately).
    fn run(op: &SpatialOp, rc: &ResidentContext) -> Vec<f32> {
        let n = rc.positions.len() as u32;
        let out_spec = SlotSpec { n, c: 1 };
        let inputs: Vec<InputView> = Vec::new();
        let times = [0.0f32];
        let kctx = KernelCtx {
            inputs: &inputs,
            out_spec,
            times: &times,
            ctx: rc,
        };
        run_spatial(op, &kctx)
    }

    /// All k along the time axis carry the same value (t-invariance).
    #[test]
    fn broadcasts_across_time() {
        let rc = ctx_with(vec![[1.0, 0.0, 0.0], [3.0, 0.0, 0.0]]);
        let out_spec = SlotSpec { n: 2, c: 1 };
        let inputs: Vec<InputView> = Vec::new();
        let times = [0.0, 0.1, 0.2, 0.3];
        let kctx = KernelCtx {
            inputs: &inputs,
            out_spec,
            times: &times,
            ctx: &rc,
        };
        let buf = run_spatial(&SpatialOp::Pos(Axis::X), &kctx);
        assert_eq!(buf.len(), 2 * 4);
        // buf is n-major with a 4-wide time stride: index = n * times.len() + k.
        for (n, expected) in [1.0, 3.0].into_iter().enumerate() {
            for k in 0..4 {
                assert_eq!(buf[n * 4 + k], expected);
            }
        }
    }

    #[test]
    fn pos_returns_world_coords() {
        let rc = ctx_with(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        assert_eq!(run(&SpatialOp::Pos(Axis::X), &rc), vec![1.0, 4.0]);
        assert_eq!(run(&SpatialOp::Pos(Axis::Y), &rc), vec![2.0, 5.0]);
        assert_eq!(run(&SpatialOp::Pos(Axis::Z), &rc), vec![3.0, 6.0]);
    }

    #[test]
    fn rel_normalizes_to_unit_range() {
        // x spans 2..6 -> [0, 0.5, 1]; y is constant -> degenerate range -> 0.
        let rc = ctx_with(vec![[2.0, 9.0, 0.0], [4.0, 9.0, 0.0], [6.0, 9.0, 0.0]]);
        assert_eq!(run(&SpatialOp::Rel(Axis::X), &rc), vec![0.0, 0.5, 1.0]);
        let rel_y = run(&SpatialOp::Rel(Axis::Y), &rc);
        // Constant axis: (9-9)/max(0,1e-3) == 0 for all.
        assert!(rel_y.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn index_and_normalized_index() {
        let rc = ctx_with(vec![[0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3]]);
        assert_eq!(run(&SpatialOp::Index, &rc), vec![0.0, 1.0, 2.0, 3.0]);
        let ni = run(&SpatialOp::NormalizedIndex, &rc);
        assert!((ni[0] - 0.0).abs() < 1e-6);
        assert!((ni[1] - 1.0 / 3.0).abs() < 1e-6);
        assert!((ni[3] - 1.0).abs() < 1e-6);

        // Single primitive -> normalized index is 0.
        let one = ctx_with(vec![[0.0; 3]]);
        assert_eq!(run(&SpatialOp::NormalizedIndex, &one), vec![0.0]);
    }

    #[test]
    fn circle_radius_is_distance_from_xy_centroid() {
        // Square of side 2 centered at origin: each corner is sqrt(2) away.
        let rc = ctx_with(vec![
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
        ]);
        let r = run(&SpatialOp::CircleRadius, &rc);
        for v in r {
            assert!((v - 2.0_f32.sqrt()).abs() < 1e-5);
        }
    }

    #[test]
    fn angular_index_ranks_by_angle_with_coincident_sharing() {
        // Four points around origin at top/right/bottom/left, given out of order.
        // top_angle01 starts at top and increases clockwise-ish; assert that
        // ranks are a 0..3 permutation (distinct positions -> distinct ranks).
        let rc = ctx_with(vec![
            [1.0, 0.0, 0.0],  // right
            [0.0, 1.0, 0.0],  // top
            [-1.0, 0.0, 0.0], // left
            [0.0, -1.0, 0.0], // bottom
        ]);
        let mut ranks = run(&SpatialOp::AngularIndex, &rc);
        ranks.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ranks, vec![0.0, 1.0, 2.0, 3.0]);

        // A coincident pair shares a rank: 3 distinct positions + 1 duplicate
        // -> max rank 2, and the duplicate equals its twin.
        let rc2 = ctx_with(vec![
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0], // duplicate of index 0
        ]);
        let r2 = run(&SpatialOp::AngularIndex, &rc2);
        assert_eq!(r2[0], r2[3]); // coincident pair shares rank
        let max = r2.iter().cloned().fold(0.0_f32, f32::max);
        assert_eq!(max, 2.0); // 3 distinct ranks: 0,1,2
    }

    #[test]
    fn angular_position_on_unit_circle_is_unit_interval() {
        // 8 points on a unit circle -> fit succeeds, positions span 0..1.
        let mut positions = Vec::new();
        for i in 0..8 {
            let a = (i as f32) * std::f32::consts::PI / 4.0;
            positions.push([a.cos(), a.sin(), 0.0]);
        }
        let rc = ctx_with(positions);
        let ap = run(&SpatialOp::AngularPosition, &rc);
        assert_eq!(ap.len(), 8);
        for v in &ap {
            assert!(*v >= 0.0 && *v < 1.0, "angular_position out of range: {v}");
        }
        // Distinct points -> distinct angular positions.
        let mut sorted = ap.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in sorted.windows(2) {
            assert!((w[1] - w[0]).abs() > 1e-3, "angular positions collided");
        }
    }

    #[test]
    fn angular_position_falls_back_when_fit_fails() {
        // 2 points -> circle fit returns None -> centroid atan2 fallback, no panic.
        let rc = ctx_with(vec![[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]]);
        let ap = run(&SpatialOp::AngularPosition, &rc);
        assert_eq!(ap.len(), 2);
        for v in ap {
            assert!(v >= 0.0 && v < 1.0);
        }
    }

    #[test]
    fn mirror_side_signs_and_centered_zero() {
        // x: -2, 0, 2 -> mean 0 -> sides -1, 0, +1.
        let rc = ctx_with(vec![[-2.0, 0.0, 0.0], [0.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
        assert_eq!(run(&SpatialOp::Mirror(Axis::X), &rc), vec![-1.0, 0.0, 1.0]);
    }

    /// A 2×4 rig: two rows of four fixtures, running along world X at two
    /// heights. The reference rig for the `u`/`v` tests.
    fn test_rig() -> Vec<[f32; 3]> {
        let mut p = Vec::new();
        for row in 0..2 {
            for col in 0..4 {
                p.push([col as f32 * 2.0 - 3.0, 0.0, 2.0 + row as f32]);
            }
        }
        p
    }

    /// Rotate a rig about the world Z axis (through the origin) by `deg`.
    fn rotate_z(positions: &[[f32; 3]], deg: f32) -> Vec<[f32; 3]> {
        let a = deg.to_radians();
        positions
            .iter()
            .map(|p| {
                [
                    p[0] * a.cos() - p[1] * a.sin(),
                    p[0] * a.sin() + p[1] * a.cos(),
                    p[2],
                ]
            })
            .collect()
    }

    /// `u` runs along the rig, `0..1`, identical for both rows.
    #[test]
    fn u_chase_runs_along_the_rig() {
        let rc = ctx_with(test_rig());
        let u = run(&SpatialOp::U, &rc);
        assert_eq!(u[..4], [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
        assert_eq!(u[..4], u[4..]); // both rows chase together
    }

    /// `v` is height, `0..1`, identical along each row.
    #[test]
    fn v_sweep_runs_up_the_rig() {
        let rc = ctx_with(test_rig());
        let v = run(&SpatialOp::V, &rc);
        assert!(v[..4].iter().all(|&x| x == 0.0), "{v:?}");
        assert!(v[4..].iter().all(|&x| x == 1.0), "{v:?}");
    }

    /// The whole point: rotating the venue must not change the pattern. `u` is
    /// invariant under a 45° yaw of the entire rig (and anywhere else in the
    /// canonical `(-45°, 135°]` window), sign included.
    #[test]
    fn u_is_invariant_under_venue_rotation() {
        let base = run(&SpatialOp::U, &ctx_with(test_rig()));
        for deg in [45.0, 30.0, 90.0, 134.0, -44.0] {
            let rotated = run(&SpatialOp::U, &ctx_with(rotate_z(&test_rig(), deg)));
            for (i, (a, b)) in base.iter().zip(rotated.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-4,
                    "u changed at {deg}° (primitive {i}): {base:?} vs {rotated:?}"
                );
            }
        }
    }

    /// …and outside that window `u` reverses, because the canonical direction is
    /// pinned to the world (+X), not to the rig. Pinned here so the seam is a
    /// documented property rather than a surprise.
    #[test]
    fn u_reverses_when_the_rig_turns_past_the_canonical_window() {
        let base = run(&SpatialOp::U, &ctx_with(test_rig()));
        let turned = run(&SpatialOp::U, &ctx_with(rotate_z(&test_rig(), 180.0)));
        for (a, b) in base.iter().zip(turned.iter()) {
            assert!((a - (1.0 - b)).abs() < 1e-4, "{base:?} vs {turned:?}");
        }
    }

    /// Sign canonicalization: a rig authored right-to-left produces the *same*
    /// `u` as the same rig authored left-to-right — the axis points +X either
    /// way, so `u` follows geometry, not patch order.
    #[test]
    fn u_sign_is_canonical() {
        let mut reversed = test_rig();
        reversed.reverse();
        let u = run(&SpatialOp::U, &ctx_with(reversed));
        // Reversed list -> reversed values, but still 0 at the -X end.
        assert_eq!(u[..4], [1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0]);

        // A rig running front-to-back (+Y): u still increases with +Y.
        let fb = vec![[0.0, -1.0, 2.0], [0.0, 0.0, 2.0], [0.0, 1.0, 2.0]];
        assert_eq!(run(&SpatialOp::U, &ctx_with(fb)), vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn uv_degenerate_rigs_fall_back() {
        // Single vertical stack: no horizontal spread -> u = normalized index.
        let stack = ctx_with(vec![[1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [1.0, 1.0, 2.0]]);
        assert_eq!(run(&SpatialOp::U, &stack), vec![0.0, 0.5, 1.0]);
        assert_eq!(run(&SpatialOp::V, &stack), vec![0.0, 0.5, 1.0]);

        // Flat rig: no height spread -> v = 0.5 everywhere.
        let flat = ctx_with(vec![[0.0, 0.0, 3.0], [1.0, 0.0, 3.0]]);
        assert_eq!(run(&SpatialOp::V, &flat), vec![0.5, 0.5]);

        // One fixture: both fall back, no NaNs.
        let one = ctx_with(vec![[5.0, 5.0, 5.0]]);
        assert_eq!(run(&SpatialOp::U, &one), vec![0.0]);
        assert_eq!(run(&SpatialOp::V, &one), vec![0.5]);
    }

    #[test]
    fn get_attribute_is_stubbed_zero() {
        let rc = ctx_with(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        let v = run(&SpatialOp::GetAttribute("gobo".into()), &rc);
        assert_eq!(v, vec![0.0, 0.0]);
    }
}
