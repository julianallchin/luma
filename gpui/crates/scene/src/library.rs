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
/// as well as the fixture's own: Luma has one position per fixture). Every
/// other joint stays at its `rest`, so a static fixture whose tilt is set to
/// face front faces front.
#[must_use]
pub fn angles(built: &Built, [pan, tilt]: [f32; 2]) -> Vec<f32> {
    built
        .joints
        .iter()
        .enumerate()
        .map(|(i, j)| match (j.kind, j.drive) {
            (JointKind::Pan, Drive::Dmx) => sign(built, i) * pan,
            (JointKind::Tilt, Drive::Dmx) => sign(built, i) * tilt,
            _ => j.rest,
        })
        .collect()
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

    #[test]
    fn the_clamp_sits_above_the_pivot_and_the_lens_below_it() {
        let b = sharpy();
        let (lens, dir) = beam(&b, &worlds(&b, [0.0, 0.0]), 0).unwrap();
        assert!(clamp_height(&b) > 0.0);
        assert!(lens.y < 0.0 && dir.y < -0.99);
    }
}
