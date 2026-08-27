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

use glam::{Mat3, Mat4, Vec3};

/// three.js `Object3D.position.set(posX, posZ, posY)`.
///
/// Transposing two axes is its own inverse, so this maps three space back to
/// data space just as well. [`data_pose_of`] relies on that.
#[must_use]
pub fn three_from_data(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.z, p.y)
}

/// The three-space pose a stored `(position, euler)` pair denotes.
///
/// Every stored transform in a venue — fixture or stage piece, root or attached
/// — means this and nothing else, so it is spelled once. The `(x, z, y)` swap
/// applies to *both* halves: three.js builds these as
/// `position.set(posX, posZ, posY)` / `rotation.set(rotX, rotZ, rotY)`, and a
/// pose whose translation is swapped but whose Euler triple is not is a pose in
/// no space at all. That mistake is only visible once transforms compose, which
/// is why it survived in the stage-piece parent chain for as long as it did.
///
/// Scale is not included: it is uniform, commutes with the rotation, and the
/// callers that have one apply it themselves.
#[must_use]
pub fn three_pose_from_data(pos: [f32; 3], rot: [f32; 3]) -> Mat4 {
    Mat4::from_translation(three_from_data(Vec3::from(pos)))
        * Mat4::from_mat3(euler_xyz(rot[0], rot[2], rot[1]))
}

/// Recover the stored `(position, euler)` pair from a three-space pose — the
/// inverse of [`three_pose_from_data`].
///
/// This is what flattening a parent chain needs: compose in three space, then
/// come back out to the stored convention once, at the end. Any uniform scale in
/// `m` is dropped, matching what [`three_pose_from_data`] does not take.
///
/// Round-trips to the same *pose*, not necessarily the same triple — see
/// [`euler_xyz_of`] on the gimbal clamp.
#[must_use]
pub fn data_pose_of(m: Mat4) -> ([f32; 3], [f32; 3]) {
    let (_, rotation, translation) = m.to_scale_rotation_translation();
    let e = euler_xyz_of(Mat3::from_quat(rotation));
    (
        // The swap is an involution, so this is three -> data.
        three_from_data(translation).to_array(),
        [e.x, e.z, e.y],
    )
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

/// The inverse of [`srgb_to_linear`]. Anything that averages, blends or
/// resamples a readback frame has to come back through here: the renderer hands
/// out sRGB-encoded bytes, and arithmetic on those is arithmetic on a gamma
/// curve.
#[must_use]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c < 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
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

    #[test]
    fn a_stored_pose_round_trips_through_three_space() {
        let pos = [1.5, -2.25, 4.0];
        let rot = [0.3, -1.1, 0.75];
        let (back_pos, back_rot) = data_pose_of(three_pose_from_data(pos, rot));
        assert!(Vec3::from(back_pos).abs_diff_eq(Vec3::from(pos), 1e-5));
        assert!(Vec3::from(back_rot).abs_diff_eq(Vec3::from(rot), 1e-5));
    }

    #[test]
    fn composing_stored_poses_matches_nesting_them() {
        // The property the stage-piece parent chain depends on: flattening
        // parent-then-child must land where three.js's nested groups land.
        // Composing in three space and converting once is the only order that
        // holds, because the swap is a mirror and does not distribute over a
        // product of poses built in the wrong space.
        let parent = ([1.0, 2.0, 0.0], [0.0, 0.0, std::f32::consts::FRAC_PI_2]);
        let child = ([1.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let flattened =
            three_pose_from_data(parent.0, parent.1) * three_pose_from_data(child.0, child.1);
        let (pos, _) = data_pose_of(flattened);

        // A quarter turn about stored Z takes the child's +X offset to -Y in
        // data space, so the child sits at (1, 1, 0), not (1, 3, 0).
        assert!(
            Vec3::from(pos).abs_diff_eq(Vec3::new(1.0, 1.0, 0.0), 1e-5),
            "flattened child landed at {pos:?}"
        );
    }
}
