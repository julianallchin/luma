//! Circle fitting with PCA plane detection and RANSAC outlier rejection

use std::f32::consts::PI;

/// Result of circle fitting
pub struct CircleFitResult {
    /// Center of fitted circle in the 2D projected space
    pub center_u: f32,
    pub center_v: f32,
    /// Radius of fitted circle
    pub radius: f32,
    /// Angular positions (0..1) for each input point
    pub angular_positions: Vec<f32>,
    /// Whether each point is an inlier
    pub is_inlier: Vec<bool>,
}

/// 3D point with ID for tracking
#[derive(Clone)]
struct Point3D {
    x: f32,
    y: f32,
    z: f32,
}

/// 2D projected point
#[derive(Clone, Copy)]
struct Point2D {
    u: f32,
    v: f32,
}

/// Basis vectors for the fitted plane
struct PlaneBasis {
    basis_u: [f32; 3],
    basis_v: [f32; 3],
}

/// Fit a circle to 3D points using PCA + RANSAC
///
/// Algorithm:
/// 1. PCA to find best-fit plane
/// 2. Project points onto plane (2D)
/// 3. RANSAC circle fit with outlier rejection
/// 4. Compute angular positions
pub fn fit_circle_3d(positions: &[(f32, f32, f32)]) -> Option<CircleFitResult> {
    let n = positions.len();
    if n < 3 {
        return None;
    }

    let points: Vec<Point3D> = positions
        .iter()
        .map(|(x, y, z)| Point3D { x: *x, y: *y, z: *z })
        .collect();

    // 1. Compute centroid
    let (cx, cy, cz) = compute_centroid(&points);

    // 2. Find plane basis using PCA
    let basis = find_plane_basis(&points, cx, cy, cz);

    // 3. Project points onto plane
    let projected: Vec<Point2D> = points
        .iter()
        .map(|p| {
            let dx = p.x - cx;
            let dy = p.y - cy;
            let dz = p.z - cz;
            Point2D {
                u: dx * basis.basis_u[0] + dy * basis.basis_u[1] + dz * basis.basis_u[2],
                v: dx * basis.basis_v[0] + dy * basis.basis_v[1] + dz * basis.basis_v[2],
            }
        })
        .collect();

    // 4. RANSAC circle fit
    let ransac_result = ransac_circle_fit(&projected, 100, 2.5)?;

    // 5. Compute angular positions for all points
    let angular_positions: Vec<f32> = projected
        .iter()
        .map(|p| {
            let du = p.u - ransac_result.center_u;
            let dv = p.v - ransac_result.center_v;
            let angle = dv.atan2(du); // -PI to PI
            (angle + PI) / (2.0 * PI) // Normalize to 0..1
        })
        .collect();

    Some(CircleFitResult {
        center_u: ransac_result.center_u,
        center_v: ransac_result.center_v,
        radius: ransac_result.radius,
        angular_positions,
        is_inlier: ransac_result.is_inlier,
    })
}

fn compute_centroid(points: &[Point3D]) -> (f32, f32, f32) {
    let n = points.len() as f32;
    let cx: f32 = points.iter().map(|p| p.x).sum::<f32>() / n;
    let cy: f32 = points.iter().map(|p| p.y).sum::<f32>() / n;
    let cz: f32 = points.iter().map(|p| p.z).sum::<f32>() / n;
    (cx, cy, cz)
}

fn find_plane_basis(points: &[Point3D], cx: f32, cy: f32, cz: f32) -> PlaneBasis {
    // Build covariance matrix
    let mut cov = [[0.0f32; 3]; 3];
    for p in points {
        let dx = p.x - cx;
        let dy = p.y - cy;
        let dz = p.z - cz;
        cov[0][0] += dx * dx;
        cov[0][1] += dx * dy;
        cov[0][2] += dx * dz;
        cov[1][1] += dy * dy;
        cov[1][2] += dy * dz;
        cov[2][2] += dz * dz;
    }
    // Symmetric
    cov[1][0] = cov[0][1];
    cov[2][0] = cov[0][2];
    cov[2][1] = cov[1][2];

    // Power iteration to find dominant eigenvector
    let v1 = power_iteration(&cov);
    let cov2 = deflate(&cov, &v1);
    let v2 = power_iteration(&cov2);

    // Cross product for normal (we don't need it, but helps ensure orthogonality)
    let normal = cross(&v1, &v2);
    let normal = normalize(&normal);

    // Ensure v1 and v2 are orthonormal
    let basis_u = normalize(&v1);
    let basis_v = cross(&normal, &basis_u);
    let basis_v = normalize(&basis_v);

    PlaneBasis {
        basis_u: [basis_u[0], basis_u[1], basis_u[2]],
        basis_v: [basis_v[0], basis_v[1], basis_v[2]],
    }
}

fn power_iteration(matrix: &[[f32; 3]; 3]) -> [f32; 3] {
    let mut v = [1.0f32, 0.0, 0.0];

    for _ in 0..20 {
        let new_v = [
            matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
            matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
            matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
        ];

        let len = (new_v[0] * new_v[0] + new_v[1] * new_v[1] + new_v[2] * new_v[2]).sqrt();
        if len > 1e-10 {
            v = [new_v[0] / len, new_v[1] / len, new_v[2] / len];
        }
    }

    v
}

fn deflate(matrix: &[[f32; 3]; 3], v: &[f32; 3]) -> [[f32; 3]; 3] {
    // Compute eigenvalue (Rayleigh quotient)
    let av = [
        matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
        matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
        matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
    ];
    let lambda = v[0] * av[0] + v[1] * av[1] + v[2] * av[2];

    // Subtract outer product: M - λ * v * v^T
    let mut result = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            result[i][j] = matrix[i][j] - lambda * v[i] * v[j];
        }
    }
    result
}

fn cross(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: &[f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-10 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [1.0, 0.0, 0.0]
    }
}

struct RansacResult {
    center_u: f32,
    center_v: f32,
    radius: f32,
    is_inlier: Vec<bool>,
}

fn ransac_circle_fit(
    points: &[Point2D],
    iterations: usize,
    inlier_threshold: f32,
) -> Option<RansacResult> {
    let n = points.len();
    if n < 3 {
        return None;
    }

    // If exactly 3 points, fit directly
    if n == 3 {
        let fit = fit_circle_through_3_points(points[0], points[1], points[2])?;
        return Some(RansacResult {
            center_u: fit.0,
            center_v: fit.1,
            radius: fit.2,
            is_inlier: vec![true; 3],
        });
    }

    let mut best_inlier_count = 0;
    let mut best_fit: Option<(f32, f32, f32)> = None;
    let mut best_inliers: Vec<bool> = vec![false; n];

    // Simple deterministic pseudo-random for reproducibility
    let mut seed: u64 = 12345;
    let next_rand = |s: &mut u64| -> usize {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((*s >> 33) as usize) % n
    };

    for _ in 0..iterations {
        // Pick 3 distinct random points
        let i0 = next_rand(&mut seed);
        let mut i1 = next_rand(&mut seed);
        while i1 == i0 {
            i1 = next_rand(&mut seed);
        }
        let mut i2 = next_rand(&mut seed);
        while i2 == i0 || i2 == i1 {
            i2 = next_rand(&mut seed);
        }

        // Fit circle through these 3 points
        let fit = match fit_circle_through_3_points(points[i0], points[i1], points[i2]) {
            Some(f) => f,
            None => continue,
        };

        // Count inliers
        let mut inliers = vec![false; n];
        let mut inlier_count = 0;
        for (i, p) in points.iter().enumerate() {
            let dist_from_center = ((p.u - fit.0).powi(2) + (p.v - fit.1).powi(2)).sqrt();
            let dist_from_circle = (dist_from_center - fit.2).abs();
            if dist_from_circle < inlier_threshold {
                inliers[i] = true;
                inlier_count += 1;
            }
        }

        if inlier_count > best_inlier_count {
            best_inlier_count = inlier_count;
            best_fit = Some(fit);
            best_inliers = inliers;
        }

        // Early exit if 90% are inliers
        if inlier_count >= (n * 9) / 10 {
            break;
        }
    }

    let (center_u, center_v, radius) = best_fit?;

    // Refine with Kåsa on inliers
    let inlier_points: Vec<Point2D> = points
        .iter()
        .zip(best_inliers.iter())
        .filter(|(_, &is_inlier)| is_inlier)
        .map(|(p, _)| *p)
        .collect();

    if let Some((refined_u, refined_v, refined_r)) = kasa_fit(&inlier_points) {
        // Recompute inliers with refined fit
        let mut refined_inliers = vec![false; n];
        for (i, p) in points.iter().enumerate() {
            let dist_from_center = ((p.u - refined_u).powi(2) + (p.v - refined_v).powi(2)).sqrt();
            let dist_from_circle = (dist_from_center - refined_r).abs();
            if dist_from_circle < inlier_threshold {
                refined_inliers[i] = true;
            }
        }

        Some(RansacResult {
            center_u: refined_u,
            center_v: refined_v,
            radius: refined_r,
            is_inlier: refined_inliers,
        })
    } else {
        Some(RansacResult {
            center_u,
            center_v,
            radius,
            is_inlier: best_inliers,
        })
    }
}

/// Fit circle through exactly 3 points using circumcenter formula
fn fit_circle_through_3_points(p1: Point2D, p2: Point2D, p3: Point2D) -> Option<(f32, f32, f32)> {
    let ax = p1.u;
    let ay = p1.v;
    let bx = p2.u;
    let by = p2.v;
    let cx = p3.u;
    let cy = p3.v;

    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-10 {
        return None; // Collinear
    }

    let a_sq = ax * ax + ay * ay;
    let b_sq = bx * bx + by * by;
    let c_sq = cx * cx + cy * cy;

    let center_u = (a_sq * (by - cy) + b_sq * (cy - ay) + c_sq * (ay - by)) / d;
    let center_v = (a_sq * (cx - bx) + b_sq * (ax - cx) + c_sq * (bx - ax)) / d;

    let r = ((ax - center_u).powi(2) + (ay - center_v).powi(2)).sqrt();

    Some((center_u, center_v, r))
}

/// Kåsa algebraic circle fit
fn kasa_fit(points: &[Point2D]) -> Option<(f32, f32, f32)> {
    let n = points.len();
    if n < 3 {
        return None;
    }

    let mut sum_u = 0.0f32;
    let mut sum_v = 0.0f32;
    let mut sum_uu = 0.0f32;
    let mut sum_vv = 0.0f32;
    let mut sum_uv = 0.0f32;
    let mut sum_uuu = 0.0f32;
    let mut sum_vvv = 0.0f32;
    let mut sum_uuv = 0.0f32;
    let mut sum_uvv = 0.0f32;

    for p in points {
        let u = p.u;
        let v = p.v;
        sum_u += u;
        sum_v += v;
        sum_uu += u * u;
        sum_vv += v * v;
        sum_uv += u * v;
        sum_uuu += u * u * u;
        sum_vvv += v * v * v;
        sum_uuv += u * u * v;
        sum_uvv += u * v * v;
    }

    let nf = n as f32;

    // Build normal equations: A^T A x = A^T z
    let ata = [
        [4.0 * sum_uu, 4.0 * sum_uv, 2.0 * sum_u],
        [4.0 * sum_uv, 4.0 * sum_vv, 2.0 * sum_v],
        [2.0 * sum_u, 2.0 * sum_v, nf],
    ];

    let atz = [
        2.0 * (sum_uuu + sum_uvv),
        2.0 * (sum_uuv + sum_vvv),
        sum_uu + sum_vv,
    ];

    // Solve using Cramer's rule
    let det = ata[0][0] * (ata[1][1] * ata[2][2] - ata[1][2] * ata[2][1])
        - ata[0][1] * (ata[1][0] * ata[2][2] - ata[1][2] * ata[2][0])
        + ata[0][2] * (ata[1][0] * ata[2][1] - ata[1][1] * ata[2][0]);

    if det.abs() < 1e-10 {
        return None;
    }

    let det_a = atz[0] * (ata[1][1] * ata[2][2] - ata[1][2] * ata[2][1])
        - ata[0][1] * (atz[1] * ata[2][2] - ata[1][2] * atz[2])
        + ata[0][2] * (atz[1] * ata[2][1] - ata[1][1] * atz[2]);

    let det_b = ata[0][0] * (atz[1] * ata[2][2] - ata[1][2] * atz[2])
        - atz[0] * (ata[1][0] * ata[2][2] - ata[1][2] * ata[2][0])
        + ata[0][2] * (ata[1][0] * atz[2] - atz[1] * ata[2][0]);

    let det_c = ata[0][0] * (ata[1][1] * atz[2] - atz[1] * ata[2][1])
        - ata[0][1] * (ata[1][0] * atz[2] - atz[1] * ata[2][0])
        + atz[0] * (ata[1][0] * ata[2][1] - ata[1][1] * ata[2][0]);

    let a = det_a / det;
    let b = det_b / det;
    let c = det_c / det;

    let radius_sq = a * a + b * b + c;
    if radius_sq <= 0.0 {
        return None;
    }

    Some((a, b, radius_sq.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circle_fit_perfect_circle() {
        // Create points on a perfect circle
        let mut points = Vec::new();
        for i in 0..8 {
            let angle = (i as f32) * PI / 4.0;
            points.push((angle.cos(), angle.sin(), 0.0));
        }

        let result = fit_circle_3d(&points).unwrap();

        // Radius should be ~1.0
        assert!((result.radius - 1.0).abs() < 0.01);

        // All should be inliers
        assert!(result.is_inlier.iter().all(|&x| x));
    }

    #[test]
    fn test_circle_fit_with_outlier() {
        // Create points on a circle plus one outlier
        let mut points = Vec::new();
        for i in 0..8 {
            let angle = (i as f32) * PI / 4.0;
            points.push((angle.cos(), angle.sin(), 0.0));
        }
        // Add outlier far from circle
        points.push((5.0, 5.0, 0.0));

        let result = fit_circle_3d(&points).unwrap();

        // Radius should still be ~1.0
        assert!((result.radius - 1.0).abs() < 0.1);

        // The outlier should be marked as such
        assert!(!result.is_inlier[8]);
    }
}
