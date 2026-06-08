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
    /// Read a named per-fixture attribute. STUBBED: `ResidentContext` carries no
    /// per-fixture attribute table, so this returns `0.0` for every primitive.
    /// See the module note + report for the field that would be required.
    GetAttribute(String),
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
    let (t, n) = (ctx.t(), ctx.n());
    let positions = &ctx.ctx.positions;
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
                        [distinct[0].len() as f32, distinct[1].len() as f32, distinct[2].len() as f32]
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
        for k in 0..4 {
            assert_eq!(buf[0 * 4 + k], 1.0);
            assert_eq!(buf[1 * 4 + k], 3.0);
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
        let rc = ctx_with(
            vec![[0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3]],
        );
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
        let rc = ctx_with(
            vec![
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
            ],
        );
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
        let rc = ctx_with(
            vec![
                [1.0, 0.0, 0.0],  // right
                [0.0, 1.0, 0.0],  // top
                [-1.0, 0.0, 0.0], // left
                [0.0, -1.0, 0.0], // bottom
            ],
        );
        let mut ranks = run(&SpatialOp::AngularIndex, &rc);
        ranks.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ranks, vec![0.0, 1.0, 2.0, 3.0]);

        // A coincident pair shares a rank: 3 distinct positions + 1 duplicate
        // -> max rank 2, and the duplicate equals its twin.
        let rc2 = ctx_with(
            vec![
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [-1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0], // duplicate of index 0
            ],
        );
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
        assert_eq!(
            run(&SpatialOp::Mirror(Axis::X), &rc),
            vec![-1.0, 0.0, 1.0]
        );
    }

    #[test]
    fn get_attribute_is_stubbed_zero() {
        let rc = ctx_with(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        let v = run(&SpatialOp::GetAttribute("gobo".into()), &rc);
        assert_eq!(v, vec![0.0, 0.0]);
    }
}
