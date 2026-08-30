//! Z-up world space, and the bridge from the two spaces the goldens speak.
//!
//! This is the **single copy** of the convention. It lives here rather than in
//! the renderer because the venue resolver ([`crate::venue`]) is the other
//! thing that has to speak it, and the resolver must stay GPU-free.
//! `luma_render::coords` re-exports every item below, so a caller's path is
//! unchanged.
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

use glam::{DMat3, DMat4, DVec3, Mat3, Mat4, Vec3};

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
/// `luma_render::scene_desc::Scene` holds its pose in three space because that is
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
/// It exists because `luma_render::scene_desc::Piece` stores a pose as Euler angles
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

// ---------------------------------------------------------------------------
// The same bridge in `f64`
// ---------------------------------------------------------------------------
//
// The socket layer and the venue resolver are `f64` (see the crate docs: the
// golden vectors pin them to 1e-6 absolute over coordinates up to 1e6, two
// orders of magnitude past what `f32` holds). The renderer is `f32`. Rather
// than cast at every boundary and hope, the conversion is spelled once in each
// precision, adjacent, with `f32_and_f64_agree` holding them together.

/// [`three_from_data`] in `f64`.
#[must_use]
pub fn three_from_data_d(p: DVec3) -> DVec3 {
    DVec3::new(p.x, p.z, p.y)
}

/// [`euler_xyz`] in `f64`.
#[must_use]
pub fn euler_xyz_d(x: f64, y: f64, z: f64) -> DMat3 {
    DMat3::from_rotation_x(x) * DMat3::from_rotation_y(y) * DMat3::from_rotation_z(z)
}

/// [`euler_xyz_of`] in `f64`.
#[must_use]
pub fn euler_xyz_of_d(m: DMat3) -> DVec3 {
    let m02 = m.z_axis.x.clamp(-1.0, 1.0);
    let y = m02.asin();
    if m02.abs() < 0.999_999_999_9 {
        DVec3::new(
            (-m.z_axis.y).atan2(m.z_axis.z),
            y,
            (-m.y_axis.x).atan2(m.x_axis.x),
        )
    } else {
        DVec3::new(m.y_axis.z.atan2(m.y_axis.y), y, 0.0)
    }
}

/// [`three_pose_from_data`] in `f64`.
#[must_use]
pub fn three_pose_from_data_d(pos: [f64; 3], rot: [f64; 3]) -> DMat4 {
    DMat4::from_translation(three_from_data_d(DVec3::from(pos)))
        * DMat4::from_mat3(euler_xyz_d(rot[0], rot[2], rot[1]))
}

/// [`data_pose_of`] in `f64`.
#[must_use]
pub fn data_pose_of_d(m: DMat4) -> ([f64; 3], [f64; 3]) {
    let (_, rotation, translation) = m.to_scale_rotation_translation();
    let e = euler_xyz_of_d(DMat3::from_quat(rotation));
    (three_from_data_d(translation).to_array(), [e.x, e.z, e.y])
}

/// The data-space *basis* a three-space pose denotes: position and rotation,
/// with no trip through Euler angles.
///
/// [`data_pose_of`] answers the same question in the stored convention, which
/// is what the database and the renderer's `Piece` want. This answers it as a
/// matrix, which is what `fixture_kinematics::Mount` wants — and going through
/// a triple would push the pose through the gimbal clamp in
/// [`euler_xyz_of`] for no reason.
///
/// The swap is a reflection, so the rotation is *conjugated* by it
/// (`S · R · S`, and `S = S⁻¹`); conjugating by a reflection preserves the
/// determinant, so the result is still a rotation.
#[must_use]
pub fn data_basis_from_three(m: DMat4) -> (DVec3, DMat3) {
    let (_, rotation, translation) = m.to_scale_rotation_translation();
    (
        three_from_data_d(translation),
        swapped_basis(DMat3::from_quat(rotation)),
    )
}

/// The same turn, read in the other space.
///
/// The `(y, z)` swap is a reflection and its own inverse, so a rotation crosses
/// the boundary by conjugation (`S · R · S`) and one function serves both
/// directions: [`data_basis_from_three`] is this reading data-ward, and a
/// caller with a data-space turn to apply to a three-space pose reads it the
/// other way. Conjugating by a reflection preserves the determinant, so the
/// result is still a rotation.
#[must_use]
pub fn swapped_basis(r: DMat3) -> DMat3 {
    SWAP_YZ * r * SWAP_YZ
}

/// The `(x, z, y)` axis swap as a matrix. Its own inverse, determinant −1.
const SWAP_YZ: DMat3 = DMat3::from_cols(
    DVec3::new(1.0, 0.0, 0.0),
    DVec3::new(0.0, 0.0, 1.0),
    DVec3::new(0.0, 1.0, 0.0),
);

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

    #[test]
    fn f32_and_f64_agree() {
        let pos = [1.5, -2.25, 4.0];
        let rot = [0.3, -1.1, 0.75];
        let a = three_pose_from_data(pos, rot);
        let b = three_pose_from_data_d(pos.map(f64::from), rot.map(f64::from));
        for c in 0..4 {
            assert!(
                (a.col(c) - b.col(c).as_vec4()).length() < 1e-5,
                "column {c}"
            );
        }
        let (ap, ar) = data_pose_of(a);
        let (bp, br) = data_pose_of_d(b);
        for i in 0..3 {
            assert!((ap[i] - bp[i] as f32).abs() < 1e-5);
            assert!((ar[i] - br[i] as f32).abs() < 1e-5);
        }
    }

    /// The matrix route and the Euler route describe the same data-space pose.
    #[test]
    fn the_basis_and_the_triple_agree() {
        let m = three_pose_from_data_d([1.0, -2.0, 3.0], [0.4, 1.2, -0.6]);
        let (pos, basis) = data_basis_from_three(m);
        let (tri_pos, tri_rot) = data_pose_of_d(m);
        assert!(pos.abs_diff_eq(DVec3::from(tri_pos), 1e-12));
        // The stored triple, read back the way `Mount::from_stored` reads it.
        let from_triple = DMat3::from_rotation_x(-tri_rot[0])
            * DMat3::from_rotation_z(-tri_rot[2])
            * DMat3::from_rotation_y(-tri_rot[1]);
        for c in 0..3 {
            assert!(
                (basis.col(c) - from_triple.col(c)).length() < 1e-9,
                "column {c}: {basis:?} vs {from_triple:?}"
            );
        }
    }

    #[test]
    fn the_swap_is_an_involution() {
        assert!((SWAP_YZ * SWAP_YZ - DMat3::IDENTITY)
            .to_cols_array()
            .iter()
            .all(|v| v.abs() < 1e-15));
        assert!((SWAP_YZ.determinant() + 1.0).abs() < 1e-15);
    }
}
