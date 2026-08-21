//! The camera: spherical, Z-up, reverse-Z with an infinite far plane.
//!
//! Spherical because that is what the orbit controller manipulates and what
//! gets persisted per venue — storing `position + target` and deriving the
//! angles back is how a camera drifts.

use crate::bvh::Ray;
use glam::{Mat4, Vec2, Vec3};

/// Keeps the view direction off the poles, where `look_at` has no up vector.
const POLAR_LIMIT: f32 = 1e-3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub radius: f32,
    /// Rotation about +Z, measured from +X.
    pub azimuth: f32,
    /// Angle from +Z. Clamped away from the poles.
    pub polar: f32,
    pub fov_y_deg: f32,
    pub znear: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            radius: 8.0,
            azimuth: std::f32::consts::FRAC_PI_4,
            polar: std::f32::consts::FRAC_PI_3,
            fov_y_deg: 50.0,
            znear: 0.1,
        }
    }
}

impl Camera {
    pub fn position(&self) -> Vec3 {
        let (sp, cp) = self.clamped_polar().sin_cos();
        let (sa, ca) = self.azimuth.sin_cos();
        self.target + self.radius * Vec3::new(sp * ca, sp * sa, cp)
    }

    fn clamped_polar(&self) -> f32 {
        self.polar
            .clamp(POLAR_LIMIT, std::f32::consts::PI - POLAR_LIMIT)
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.position(), self.target, Vec3::Z)
    }

    /// Reverse-Z, infinite far: clip `z ∈ [1, 0]`, compared with
    /// `CompareFunction::Greater`. Removes every depth-precision question over
    /// the 0.1 m – 50 m stage for the price of one matrix constant.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(self.fov_y_deg.to_radians(), aspect, self.znear)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection(aspect) * self.view()
    }

    /// Picking ray through a normalized device coordinate (`-1..1`, +Y up).
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> Ray {
        let inv = self.view_projection(aspect).inverse();
        // Reverse-Z: the near plane is z = 1 and z = 0 is infinitely far, so
        // the second point is taken at a finite depth (z = 0.5, i.e. twice
        // znear) rather than on the far plane.
        let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
        let ahead = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.5));
        Ray::new(near, ahead - near)
    }

    /// Project a world point to NDC. Marquee selection is projection-based,
    /// not raycast-based, so this is its primitive.
    pub fn project(&self, world: Vec3, aspect: f32) -> Vec3 {
        self.view_projection(aspect).project_point3(world)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spherical_position_is_z_up() {
        let c = Camera {
            target: Vec3::ZERO,
            radius: 2.0,
            azimuth: 0.0,
            polar: 0.0,
            ..Default::default()
        };
        // Straight overhead, clamped just off the pole.
        assert!(c.position().abs_diff_eq(Vec3::new(0.002, 0.0, 2.0), 1e-3));
    }

    #[test]
    fn reverse_z_maps_near_to_one_and_far_to_zero() {
        let c = Camera::default();
        let p = c.projection(16.0 / 9.0);
        let near = p.project_point3(Vec3::new(0.0, 0.0, -c.znear));
        let far = p.project_point3(Vec3::new(0.0, 0.0, -1.0e7));
        assert!((near.z - 1.0).abs() < 1e-5, "near z = {}", near.z);
        assert!(far.z.abs() < 1e-5, "far z = {}", far.z);
    }

    #[test]
    fn centre_ray_points_at_the_target() {
        let c = Camera::default();
        let ray = c.ray(Vec2::ZERO, 1.5);
        let to_target = (c.target - c.position()).normalize();
        assert!(ray.dir.abs_diff_eq(to_target, 1e-4));
        assert!((ray.origin.distance(c.position()) - c.znear).abs() < 1e-3);
    }

    #[test]
    fn project_round_trips_through_ray() {
        let c = Camera::default();
        let aspect = 1.777;
        let world = Vec3::new(1.0, -2.0, 0.5);
        let ndc = c.project(world, aspect);
        let ray = c.ray(ndc.truncate(), aspect);
        let t = ray.t_of(world);
        assert!(ray.at(t).abs_diff_eq(world, 1e-3));
    }
}
