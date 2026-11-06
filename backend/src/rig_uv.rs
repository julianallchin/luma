//! Venue binding coordinates: rig-horizontal principal axis and world height.
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

#[cfg(test)]
mod tests {
    use super::*;
    fn horizontal(positions: &[[f32; 3]]) -> Vec<f32> {
        rig_uv(positions).iter().map(|p| p[0]).collect()
    }
    fn vertical(positions: &[[f32; 3]]) -> Vec<f32> {
        rig_uv(positions).iter().map(|p| p[1]).collect()
    }

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
        let rc = test_rig();
        let u = horizontal(&rc);
        assert_eq!(u[..4], [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
        assert_eq!(u[..4], u[4..]); // both rows chase together
    }

    /// `v` is height, `0..1`, identical along each row.
    #[test]
    fn v_sweep_runs_up_the_rig() {
        let rc = test_rig();
        let v = vertical(&rc);
        assert!(v[..4].iter().all(|&x| x == 0.0), "{v:?}");
        assert!(v[4..].iter().all(|&x| x == 1.0), "{v:?}");
    }

    /// The whole point: rotating the venue must not change the pattern. `u` is
    /// invariant under a 45° yaw of the entire rig (and anywhere else in the
    /// canonical `(-45°, 135°]` window), sign included.
    #[test]
    fn u_is_invariant_under_venue_rotation() {
        let base = horizontal(&(test_rig()));
        for deg in [45.0, 30.0, 90.0, 134.0, -44.0] {
            let rotated = horizontal(&(rotate_z(&test_rig(), deg)));
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
        let base = horizontal(&(test_rig()));
        let turned = horizontal(&(rotate_z(&test_rig(), 180.0)));
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
        let u = horizontal(&(reversed));
        // Reversed list -> reversed values, but still 0 at the -X end.
        assert_eq!(u[..4], [1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0]);

        // A rig running front-to-back (+Y): u still increases with +Y.
        let fb = vec![[0.0, -1.0, 2.0], [0.0, 0.0, 2.0], [0.0, 1.0, 2.0]];
        assert_eq!(horizontal(&(fb)), vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn uv_degenerate_rigs_fall_back() {
        // Single vertical stack: no horizontal spread -> u = normalized index.
        let stack = vec![[1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [1.0, 1.0, 2.0]];
        assert_eq!(horizontal(&stack), vec![0.0, 0.5, 1.0]);
        assert_eq!(vertical(&stack), vec![0.0, 0.5, 1.0]);

        // Flat rig: no height spread -> v = 0.5 everywhere.
        let flat = vec![[0.0, 0.0, 3.0], [1.0, 0.0, 3.0]];
        assert_eq!(vertical(&flat), vec![0.5, 0.5]);

        // One fixture: both fall back, no NaNs.
        let one = vec![[5.0, 5.0, 5.0]];
        assert_eq!(horizontal(&one), vec![0.0]);
        assert_eq!(vertical(&one), vec![0.5]);
    }
}

fn bounds(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    positions.iter().fold(
        ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]),
        |(mut lo, mut hi), p| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        },
    )
}
