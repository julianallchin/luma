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

/// World space (Z-up) back into three space (Y-up), the inverse of
/// [`world_from_three`]. A live camera orbits in world space, but
/// [`crate::scene_desc::Scene`] holds its pose in three space because that is
/// the space the goldens were captured in — this is the one conversion at that
/// boundary.
#[must_use]
pub fn three_from_world(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.z, -p.y)
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

/// The inverse of [`euler_xyz`]: recover `(x, y, z)` radians from a rotation.
///
/// Exact port of three.js `Euler.setFromRotationMatrix(m, "XYZ")`, gimbal
/// clamp included — at `|m02| ≈ 1` the X and Z rotations are the same rotation
/// and three attributes all of it to X, so a matrix built by `euler_xyz` and
/// read back here round-trips to the *same matrix*, not necessarily the same
/// triple.
///
/// It exists because [`crate::scene_desc::Piece`] stores a pose as Euler angles
/// while a parent chain composes as matrices: flattening one means going out of
/// the angles and back. Keeping the pair adjacent is what stops the two
/// conventions from drifting.
#[must_use]
pub fn euler_xyz_of(m: Mat3) -> Vec3 {
    // Column-major: `m.z_axis.x` is row 0, column 2.
    let m02 = m.z_axis.x.clamp(-1.0, 1.0);
    let y = m02.asin();
    if m02.abs() < 0.999_999_9 {
        Vec3::new(
            (-m.z_axis.y).atan2(m.z_axis.z),
            y,
            (-m.y_axis.x).atan2(m.x_axis.x),
        )
    } else {
        Vec3::new(m.y_axis.z.atan2(m.y_axis.y), y, 0.0)
    }
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
    fn euler_round_trips_through_the_matrix() {
        for &(x, y, z) in &[
            (0.0, 0.0, 0.0),
            (0.3, -1.1, 2.0),
            (-2.9, 0.7, 0.4),
            (0.5, std::f32::consts::FRAC_PI_2, 0.9),
        ] {
            let m = euler_xyz(x, y, z);
            let e = euler_xyz_of(m);
            let back = euler_xyz(e.x, e.y, e.z);
            for c in 0..3 {
                assert!(
                    (m.col(c) - back.col(c)).length() < 1e-5,
                    "({x}, {y}, {z}) -> {e:?}"
                );
            }
        }
    }

    #[test]
    fn three_and_world_round_trip() {
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert!(three_from_world(world_from_three(p)).abs_diff_eq(p, 1e-6));
        assert!(world_from_three(three_from_world(p)).abs_diff_eq(p, 1e-6));
    }

    #[test]
    fn data_to_world_mirrors_y() {
        assert_eq!(
            world_from_data(Vec3::new(1.0, 2.0, 3.0)),
            Vec3::new(1.0, -2.0, 3.0)
        );
    }
}
