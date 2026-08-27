//! Axis-aligned bounding boxes.
//!
//! Two precisions, mirroring glam's own `Vec3`/`DVec3` split and for the same
//! reason (see the crate docs): [`Aabb`] is renderer space — mesh bounds, node
//! bounds, BVH nodes — and [`DAabb`] is authoring space, where socket anchors
//! are resolved against a mesh's measured bbox and a float32 rounding is
//! already larger than the tolerance the golden vectors hold us to.

use glam::{DVec3, Vec3};

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
