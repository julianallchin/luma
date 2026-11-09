//! A library fixture's [`Built`], posed the way Luma drives it.
//!
//! The build is in housing space, which is the space a bundled `.qxf` housing
//! mesh is authored in (three space, Y-up, hung: origin at the pivot, beam
//! `-Y`, front `+Z`), so a library fixture slots into the same mount frame.
//!
//! # Pan and tilt
//!
//! Luma's pan and tilt are one convention for every fixture: the one
//! `fixture-kinematics` owns and the renderer draws a bundled moving head with,
//! `Ry(+pan)` on the arm and `Rx(-tilt)` on the head in housing space. A
//! build's joints carry their own axes, and the library's are a convention of
//! its builder (pan reads `-Y` once the spec is hung), not a measurement of
//! which way the motor turns. So [`angles`] turns Luma's degrees into joint
//! angles by the sign that makes each joint turn the way Luma means, and the
//! DMX side maps Luma's degrees straight through the channel's `phys` range,
//! as it does for a `.qxf` mover. A library fixture and a `.qxf` one at the
//! same pan and tilt point the same way and send the same bytes.

use glam::{Mat4, Vec3};
use luma_fixture::spec::Drive;
use luma_fixture::{Built, JointKind};

/// The fixture's own (not per-element) DMX joint of `kind`, if it has one.
#[must_use]
pub fn own_joint(built: &Built, kind: JointKind) -> Option<usize> {
    built
        .joints
        .iter()
        .position(|j| j.kind == kind && j.element.is_none() && j.drive == Drive::Dmx)
}

/// `+1` or `-1`: the sign that turns joint `index` the way Luma's positive
/// angle of its kind turns (see the module note). Pan and tilt only.
fn sign(built: &Built, index: usize) -> f32 {
    let joint = &built.joints[index];
    let zero = built.world_matrices(&[]);
    let axis = zero[joint.node].transform_vector3(joint.axis);
    let luma = match joint.kind {
        JointKind::Pan => Vec3::Y,
        _ => Vec3::NEG_X,
    };
    if axis.dot(luma) < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// One angle per joint that poses the build at Luma's `[pan, tilt]` degrees.
///
/// Pan and tilt joints the DMX drives take Luma's angle (an array element's
/// as well as the fixture's own: Luma has one position per fixture).
///
/// A hand-set tilt (a bracket or yoke you lock with a knob) sits at 0, which
/// is beam straight out of the clamp: Luma's rule is that a fixture shines
/// along the normal of the surface it is mounted on, and aiming it elsewhere
/// is the placed fixture's own `pan`/`tilt`, not the spec's. The spec's `rest`
/// says how the maker photographs it (90 = facing front off the floor), and
/// posing a bracket there on a truss face shone it along the truss. A fixed
/// tilt is how the head is built, and it and every other joint stay at
/// their `rest`.
#[must_use]
pub fn angles(built: &Built, [pan, tilt]: [f32; 2]) -> Vec<f32> {
    built
        .joints
        .iter()
        .enumerate()
        .map(|(i, j)| match (j.kind, j.drive) {
            (JointKind::Pan, Drive::Dmx) => sign(built, i) * pan,
            (JointKind::Tilt, Drive::Dmx) => sign(built, i) * tilt,
            (JointKind::Tilt, Drive::Manual) => 0.0,
            _ => j.rest,
        })
        .collect()
}

/// The fixture's hand-set tilt joint (a bracket or yoke locked by a knob), if
/// it has one.
fn bracket(built: &Built) -> Option<usize> {
    built
        .joints
        .iter()
        .position(|j| j.kind == JointKind::Tilt && j.drive == Drive::Manual && j.element.is_none())
}

/// A placed fixture's frames with its own aim on its bracket.
///
/// The solve turns a placed fixture's pose by its `[pan, tilt]` aim (radians)
/// off the mount, as a whole, which is right for anything without a bracket:
/// a mover is aimed by its motors on top of that, and a box has nothing else
/// to turn. A fixture with a hand-set tilt keeps its bracket on the clamp and
/// leans its head in it, so here the tilt half of the aim comes back out of
/// the pose and goes onto the bracket's joint instead. The beam points the
/// same way either way; only what is drawn turning differs.
///
/// Returns what to post-multiply the pose by, and every node's frame.
#[must_use]
pub fn aimed(built: &Built, pan_tilt: [f32; 2], aim: [f32; 2]) -> (Mat4, Vec<Mat4>) {
    let mut angles = angles(built, pan_tilt);
    let Some(i) = bracket(built).filter(|_| aim[1] != 0.0) else {
        return (Mat4::IDENTITY, built.world_matrices(&angles));
    };
    // The tilt half of the solve's turn, in housing space: a turn about
    // housing X, spelled exactly as the solve spells it.
    let tilt = crate::coords::swapped_basis(fixture_kinematics::articulation_basis_d(
        0.0,
        f64::from(aim[1]),
    ))
    .as_mat3();
    let about_x = tilt.y_axis.z.atan2(tilt.y_axis.y);
    let joint = &built.joints[i];
    let axis = built.world_matrices(&[])[joint.node].transform_vector3(joint.axis);
    angles[i] = about_x.to_degrees() * axis.x.signum();
    (
        Mat4::from_mat3(tilt).inverse(),
        built.world_matrices(&angles),
    )
}

/// Every node's housing-space frame at Luma's `[pan, tilt]`.
#[must_use]
pub fn worlds(built: &Built, pan_tilt: [f32; 2]) -> Vec<Mat4> {
    built.world_matrices(&angles(built, pan_tilt))
}

/// Each canonical cell's centre at rest, in housing space. A cell no emitter
/// draws sits at the pivot.
#[must_use]
pub fn cell_centres(built: &Built) -> Vec<Vec3> {
    let worlds = worlds(built, [0.0, 0.0]);
    built
        .cells
        .iter()
        .map(|c| {
            c.node
                .map_or(Vec3::ZERO, |n| worlds[n].transform_point3(c.centre))
        })
        .collect()
}

/// Head `index`'s lens centre and unit beam direction in housing space, under
/// `worlds`.
#[must_use]
pub fn beam(built: &Built, worlds: &[Mat4], index: usize) -> Option<(Vec3, Vec3)> {
    let head = built.heads.get(index)?;
    let w = worlds.get(head.node)?;
    Some((
        w.transform_point3(head.lens_centre),
        w.transform_vector3(head.beam).normalize_or_zero(),
    ))
}

/// How far the housing reaches above its pivot: where the clamp goes.
#[must_use]
pub fn clamp_height(built: &Built) -> f32 {
    built.bounds().map_or(0.0, |(_, hi)| hi.y.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sharpy() -> Built {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../harness/fixture-specs/claypaky/sharpy.luma.json"),
        )
        .unwrap();
        luma_fixture::build(&luma_fixture::Fixture::from_json(&text).unwrap()).unwrap()
    }

    /// The beam of the build at Luma's angles, against the arm-and-head
    /// convention a bundled moving head is drawn with.
    #[test]
    fn a_library_mover_turns_the_way_a_bundled_one_does() {
        let b = sharpy();
        for pan_tilt in [
            [0.0, 0.0],
            [40.0, 0.0],
            [0.0, 50.0],
            [-120.0, 30.0],
            [75.0, -60.0],
        ] {
            let [pan, tilt] = pan_tilt;
            let (_, beam) = beam(&b, &worlds(&b, pan_tilt), 0).unwrap();
            let bundled = (Mat4::from_rotation_y(pan.to_radians())
                * Mat4::from_rotation_x(-tilt.to_radians()))
            .transform_vector3(Vec3::NEG_Y);
            assert!(
                beam.dot(bundled) > 0.999,
                "{pan_tilt:?}: library {beam}, bundled {bundled}"
            );
        }
    }

    fn slimpar() -> Built {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../harness/fixture-specs/chauvet/slimpar-64.luma.json"),
        )
        .unwrap();
        luma_fixture::build(&luma_fixture::Fixture::from_json(&text).unwrap()).unwrap()
    }

    /// A par on a hand-locked yoke, whose spec rests it facing front, shines
    /// straight out of its clamp: down off a grid, out off a truss face.
    #[test]
    fn a_hand_set_bracket_shines_out_of_its_clamp() {
        let b = slimpar();
        assert!(b
            .joints
            .iter()
            .any(|j| j.kind == JointKind::Tilt && j.drive == Drive::Manual && j.rest != 0.0));
        let (_, dir) = beam(&b, &worlds(&b, [0.0, 0.0]), 0).unwrap();
        assert!(dir.y < -0.99, "beam {dir}");
    }

    /// Aimed by hand, a bracketed par leans its head and keeps its bracket:
    /// the beam goes where the whole-body turn would send it, and the
    /// bracket's frame, under the pose, is the bracket's frame unaimed.
    #[test]
    fn a_hand_aim_leans_the_head_and_keeps_the_bracket() {
        let b = slimpar();
        let tilt = 0.6_f32;
        let whole = Mat4::from_mat3(
            crate::coords::swapped_basis(fixture_kinematics::articulation_basis_d(
                0.0,
                f64::from(tilt),
            ))
            .as_mat3(),
        );
        let (_, plain) = beam(&b, &worlds(&b, [0.0, 0.0]), 0).unwrap();
        let (back, frames) = aimed(&b, [0.0, 0.0], [0.0, tilt]);
        let (_, leaned) = beam(&b, &frames, 0).unwrap();
        let drawn = (whole * back).transform_vector3(leaned);
        let wanted = whole.transform_vector3(plain);
        assert!(drawn.dot(wanted) > 0.999, "drawn {drawn}, wanted {wanted}");
        let mount = b.joints[bracket(&b).unwrap()].node;
        let parent = b.nodes[mount].parent.unwrap();
        let rest = worlds(&b, [0.0, 0.0]);
        assert!((whole * back * frames[parent]).abs_diff_eq(rest[parent], 1e-5));
    }

    #[test]
    fn the_clamp_sits_above_the_pivot_and_the_lens_below_it() {
        let b = sharpy();
        let (lens, dir) = beam(&b, &worlds(&b, [0.0, 0.0]), 0).unwrap();
        assert!(clamp_height(&b) > 0.0);
        assert!(lens.y < 0.0 && dir.y < -0.99);
    }
}
