//! Z-up world space, and the bridge from the two spaces the goldens speak.
//!
//! Spec §2.1: the renderer is Z-up, right-handed, matching the database and the
//! eval engine. Two other spaces show up at the boundary and neither survives
//! past this module:
//!
//! * **data space** — Z-up already; `posZ` is height. What the DB stores.
//! * **three space** — Y-up. The golden camera poses are in it, and every
//!   fixture/piece transform in the three.js renderer was built in it via the
//!   `(x, z, y)` swap.
//!
//! The swap `data -> three` transposes two axes, so its determinant is −1: the
//! three.js scene is a *mirror* of data space, not a rotation of it. That is
//! smell #5 in the spec, and it is load-bearing here — the goldens record the
//! mirrored world, so reproducing them means reproducing the mirror. It is
//! isolated in [`world_from_data`] (note the negated Y) rather than smeared
//! across every call site; when the app-side swap is deleted, this becomes the
//! identity and the goldens are re-captured.

use glam::{Mat3, Vec3};

/// three.js `Object3D.position.set(posX, posZ, posY)`.
#[must_use]
pub fn three_from_data(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.z, p.y)
}

/// three space (Y-up) into world space (Z-up). A rotation, so it changes no
/// pixel — it only re-labels which axis is up.
#[must_use]
pub fn world_from_three(p: Vec3) -> Vec3 {
    Vec3::new(p.x, -p.z, p.y)
}

/// Data space straight to world. Equal to `world_from_three ∘ three_from_data`,
/// which is `(x, -y, z)` — see the module note on the mirror.
#[must_use]
pub fn world_from_data(p: Vec3) -> Vec3 {
    Vec3::new(p.x, -p.y, p.z)
}

/// Conjugation matrix for taking a three-space orientation into world space:
/// `M_world = R · M_three · Rᵀ`.
#[must_use]
pub fn three_to_world_basis() -> Mat3 {
    Mat3::from_cols(
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, -1.0, 0.0),
    )
}

/// three.js `Euler` order XYZ, spelled out rather than trusting a library's
/// idea of what "XYZ" means. Matches `Matrix4.makeRotationFromEuler`.
#[must_use]
pub fn euler_xyz(x: f32, y: f32, z: f32) -> Mat3 {
    Mat3::from_rotation_x(x) * Mat3::from_rotation_y(y) * Mat3::from_rotation_z(z)
}

/// sRGB transfer to linear, three's `SRGBToLinear`. CSS colour literals in the
/// three.js scene (`#030303`, `#191919`) arrive through this; `setRGB` values
/// are already linear and must not.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c < 0.04045 {
        c * 0.077_399_38
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// `#rrggbb` in sRGB to a linear working-space colour.
#[must_use]
pub fn hex_srgb(hex: u32) -> Vec3 {
    let ch = |shift: u32| srgb_to_linear(((hex >> shift) & 0xff) as f32 / 255.0);
    Vec3::new(ch(16), ch(8), ch(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_to_world_is_a_rotation() {
        let r = three_to_world_basis();
        assert!((r.determinant() - 1.0).abs() < 1e-6);
        // Column mapping must agree with the point-wise helper.
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert!((r * p - world_from_three(p)).length() < 1e-6);
    }

    #[test]
    fn data_to_world_mirrors_y() {
        assert_eq!(
            world_from_data(Vec3::new(1.0, 2.0, 3.0)),
            Vec3::new(1.0, -2.0, 3.0)
        );
    }
}
