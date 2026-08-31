//! Axis-aligned bounding boxes.
//!
//! Two precisions, mirroring glam's own `Vec3`/`DVec3` split and for the same
//! reason (see the crate docs): [`Aabb`] is renderer space — mesh bounds, node
//! bounds, BVH nodes — and [`DAabb`] is authoring space, where socket anchors
//! are resolved against a mesh's measured bbox and a float32 rounding is
//! already larger than the tolerance the golden vectors hold us to.

use glam::{DMat4, DVec3, Vec3};

/// Renderer-space AABB (f32, Z-up). Empty is encoded as `min > max`, which
/// makes [`Aabb::EMPTY`] absorb any point under [`Aabb::expand`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut b = Self::EMPTY;
        for p in points {
            b.expand(p);
        }
        b
    }

    pub fn expand(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&mut self, other: &Aabb) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    pub fn is_empty(&self) -> bool {
        self.min.cmpgt(self.max).any()
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        (self.max - self.min).max(Vec3::ZERO)
    }

    /// The eight corners, indexed by the bits of `i`: bit 0 picks `max.x` over
    /// `min.x`, bit 1 `max.y`, bit 2 `max.z`.
    pub fn corners(&self) -> [Vec3; 8] {
        let (lo, hi) = (self.min, self.max);
        std::array::from_fn(|i| {
            Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            )
        })
    }

    /// Surface area, the SAH cost term. Zero for an empty box.
    pub fn surface_area(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        let d = self.size();
        2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
    }
}

/// Authoring-space AABB (f64, Y-up). The Rust equivalent of three.js `Box3`
/// as `sockets.ts` uses it: measured once per mesh, then anchors resolve
/// against it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DAabb {
    pub min: DVec3,
    pub max: DVec3,
}

impl DAabb {
    pub fn new(min: DVec3, max: DVec3) -> Self {
        Self { min, max }
    }

    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> DVec3 {
        self.max - self.min
    }

    pub fn as_aabb(&self) -> Aabb {
        Aabb::new(self.min.as_vec3(), self.max.as_vec3())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_absorbs_points() {
        let mut b = Aabb::EMPTY;
        assert!(b.is_empty());
        b.expand(Vec3::new(1.0, -2.0, 3.0));
        assert!(!b.is_empty());
        assert_eq!(b.min, b.max);
        assert_eq!(b.surface_area(), 0.0);
    }

    #[test]
    fn center_and_size_match_three_box3() {
        // three.js: getCenter = (min+max)/2, getSize = max-min.
        let b = DAabb::new(DVec3::new(0.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 0.6));
        assert_eq!(b.center(), DVec3::new(1.0, 0.5, 0.3));
        assert_eq!(b.size(), DVec3::new(2.0, 1.0, 0.6));
    }
}

/// Whether two oriented boxes overlap: each a local-space [`DAabb`] carried by
/// a rigid world transform. Separating-axis test over the 15 candidate axes.
///
/// `clearance` shrinks both boxes first, because contact is not collision — a
/// piece mated flush on a face touches its host by construction, and a test
/// that refused touching would refuse every joint the resolver writes.
#[must_use]
pub fn obb_intersects(
    a: DAabb,
    a_world: &glam::DMat4,
    b: DAabb,
    b_world: &glam::DMat4,
    clearance: f64,
) -> bool {
    let shrink = |b: DAabb| {
        DAabb::new(
            b.min + DVec3::splat(clearance),
            b.max - DVec3::splat(clearance),
        )
    };
    let (a, b) = (shrink(a), shrink(b));
    if a.size().min_element() <= 0.0 || b.size().min_element() <= 0.0 {
        return false;
    }
    let axes_of = |m: &glam::DMat4| {
        [
            m.x_axis.truncate().normalize_or_zero(),
            m.y_axis.truncate().normalize_or_zero(),
            m.z_axis.truncate().normalize_or_zero(),
        ]
    };
    let (a_axes, b_axes) = (axes_of(a_world), axes_of(b_world));
    let (a_extent, b_extent) = (a.size() * 0.5, b.size() * 0.5);
    let between = b_world.transform_point3(b.center()) - a_world.transform_point3(a.center());
    let separated = |axis: DVec3| {
        if axis.length_squared() < 1e-9 {
            return false;
        }
        let axis = axis.normalize();
        let reach = |axes: &[DVec3; 3], extent: DVec3| {
            extent.x * axes[0].dot(axis).abs()
                + extent.y * axes[1].dot(axis).abs()
                + extent.z * axes[2].dot(axis).abs()
        };
        between.dot(axis).abs() > reach(&a_axes, a_extent) + reach(&b_axes, b_extent)
    };
    for axis in a_axes {
        if separated(axis) {
            return false;
        }
    }
    for axis in b_axes {
        if separated(axis) {
            return false;
        }
    }
    for a_axis in a_axes {
        for b_axis in b_axes {
            if separated(a_axis.cross(b_axis)) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod obb_tests {
    use super::*;
    use glam::{DMat4, DQuat};

    fn unit() -> DAabb {
        DAabb::new(DVec3::splat(-0.5), DVec3::splat(0.5))
    }

    #[test]
    fn separated_touching_and_overlapping_boxes_answer_correctly() {
        let at = |x: f64| DMat4::from_translation(DVec3::new(x, 0.0, 0.0));
        let none = 0.0;
        assert!(obb_intersects(unit(), &at(0.0), unit(), &at(0.5), none));
        assert!(!obb_intersects(unit(), &at(0.0), unit(), &at(2.0), none));
        // Flush contact survives a clearance: shrunk boxes no longer touch.
        assert!(!obb_intersects(unit(), &at(0.0), unit(), &at(1.0), 0.02));
    }

    #[test]
    fn rotation_is_honoured_not_boxed_over() {
        // A long thin bar rotated 90° about Y no longer reaches the box that
        // its unrotated AABB would swallow.
        let bar = DAabb::new(DVec3::new(-2.0, -0.1, -0.1), DVec3::new(2.0, 0.1, 0.1));
        let world = DMat4::from_quat(DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2));
        let probe = DMat4::from_translation(DVec3::new(1.5, 0.0, 0.0));
        assert!(!obb_intersects(bar, &world, unit(), &probe, 0.0));
        assert!(obb_intersects(bar, &DMat4::IDENTITY, unit(), &probe, 0.0));
    }
}

/// Where a ray enters an oriented box, if it does.
///
/// The ray is carried into the box's local frame and slab-tested there; the
/// returned `t` is in the caller's (world) parameterisation only under a rigid
/// pose, which is what every placed piece has. Used by the headless room pick
/// — the harness's stand-in for the viewport's mesh BVH.
#[must_use]
pub fn ray_obb(origin: DVec3, dir: DVec3, bounds: DAabb, world: &DMat4) -> Option<f64> {
    let inv = world.inverse();
    let o = inv.transform_point3(origin);
    let d = inv.transform_vector3(dir);
    let mut enter = f64::NEG_INFINITY;
    let mut exit = f64::INFINITY;
    for axis in 0..3 {
        let (o, d) = (o[axis], d[axis]);
        let (lo, hi) = (bounds.min[axis], bounds.max[axis]);
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (t1, t2) = ((lo - o) / d, (hi - o) / d);
        enter = enter.max(t1.min(t2));
        exit = exit.min(t1.max(t2));
    }
    (exit >= enter.max(0.0)).then(|| enter.max(0.0))
}
