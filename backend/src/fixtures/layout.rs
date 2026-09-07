use fixture_kinematics::{FixtureGeometry, Mount};
use glam::Vec3;
use luma_scene::venue::NodePose;

use crate::models::fixtures::FixtureDefinition;

/// One head's offset on the fixture housing, in millimetres, in the fixture's own
/// frame (`+X` across the housing, `+Z` up it). A layout fact, not a world
/// position: [`head_geometry`] is what turns it into geometry the rig can place.
#[derive(Debug, Clone, Copy)]
pub struct HeadLayout {
    pub x: f32, // Local offset X in mm
    pub y: f32, // Local offset Y in mm
    pub z: f32, // Local offset Z in mm
}

/// The mount frame of a placed fixture.
///
/// A fixture owns no pose: it hangs off a socket, and the socket's frame *is*
/// the mount frame (beam = mount normal). This is the one place the resolver's
/// `f64` basis narrows to the `f32` the kinematics work in, and the one place
/// the app turns a resolved node into a [`Mount`].
///
/// A fixture that is patched but not placed has no pose at all — it is in the
/// tray — so callers get `None` from `ResolvedVenue::pose` and must decide what
/// absence means rather than mount it at the origin.
#[must_use]
pub fn fixture_mount(pose: &NodePose) -> Mount {
    let (position, rotation) = pose.data_basis();
    Mount::from_frame(position.as_vec3(), rotation.as_mat3())
}

/// A fixture's cells — one per head the mode lays out — in the kinematics frame.
///
/// [`compute_head_offsets`] reads the QLC+ housing; this converts it to metres in
/// the fixture frame and hands it to `fixture-kinematics`, which owns everything
/// that happens to a cell afterwards. Combine with [`fixture_mount`] and
/// [`fixture_kinematics::rig_position`] to get a head's world position:
///
/// ```ignore
/// let venue = venue_graph::resolved(&mut access, fixtures_root).await?;
/// let geom = head_geometry(&def, &fixture.mode_name);
/// let world = rig_position(&geom, &fixture_mount(venue.pose(&fixture.id)?), head_index);
/// ```
///
/// Unauthored geometry, so no aperture depth: QLC+ never says where inside the
/// housing the light leaves from, and inventing a depth here would move every
/// pattern-space position (see `FixtureClass::aperture_depth_m`).
#[must_use]
pub fn head_geometry(def: &FixtureDefinition, mode_name: &str) -> FixtureGeometry {
    FixtureGeometry::unauthored(
        compute_head_offsets(def, mode_name)
            .into_iter()
            .map(|h| Vec3::new(h.x, h.y, h.z) / 1000.0)
            .collect(),
    )
}

pub fn compute_head_offsets(def: &FixtureDefinition, mode_name: &str) -> Vec<HeadLayout> {
    // Find the active mode
    let mode = match def.modes.iter().find(|m| m.name == mode_name) {
        Some(m) => m,
        None => {
            return vec![HeadLayout {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }]
        } // Fallback
    };

    // If no heads defined (or 1 head implicit), just return center
    if mode.heads.is_empty() {
        return vec![HeadLayout {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }];
    }

    // Check physical layout dimensions
    let physical = match &def.physical {
        Some(p) => p,
        None => {
            return vec![
                HeadLayout {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0
                };
                mode.heads.len()
            ]
        }
    };

    let width = physical.dimensions.as_ref().map(|d| d.width).unwrap_or(0.0);
    let height = physical
        .dimensions
        .as_ref()
        .map(|d| d.height)
        .unwrap_or(0.0);

    let layout_w = physical
        .layout
        .as_ref()
        .map(|l| l.width)
        .unwrap_or(1)
        .max(1);
    let layout_h = physical
        .layout
        .as_ref()
        .map(|l| l.height)
        .unwrap_or(1)
        .max(1);

    // Ensure we don't divide by zero if dimensions are missing
    if width == 0.0 && height == 0.0 {
        return vec![
            HeadLayout {
                x: 0.0,
                y: 0.0,
                z: 0.0
            };
            mode.heads.len()
        ];
    }

    let mut offsets = Vec::with_capacity(mode.heads.len());

    // Calculate cell sizes
    let cell_w = width / layout_w as f32;
    let cell_h = height / layout_h as f32;

    // Center offsets (origin is fixture center). Z-up, Y-forward coordinate system.
    // Assume layout rows are ordered top-to-bottom (row 0 at top).
    let start_x = -width / 2.0 + cell_w / 2.0;
    let start_z = height / 2.0 - cell_h / 2.0;

    // Iterate heads and map to grid
    // QLC+ heads are usually row-major (X then Y)
    // But <Head> order in XML is what matters. We assume they match the layout grid order.

    let total_cells = layout_w * layout_h;
    let num_heads = mode.heads.len() as u32;

    // If we have fewer heads than cells, and it divides evenly, assume grouping (e.g. 12 pixels -> 4 heads = 3 pixels/head)
    let use_grouping = num_heads > 0 && total_cells > num_heads && total_cells % num_heads == 0;
    let stride = if use_grouping {
        total_cells / num_heads
    } else {
        1
    };

    for i in 0..mode.heads.len() {
        let center_idx = if use_grouping {
            // Calculate center of the group in linear index space
            let start_idx = (i as u32) * stride;
            start_idx as f32 + (stride as f32 - 1.0) / 2.0
        } else {
            i as f32
        };

        // Map linear index (possibly fractional) back to Row/Col
        // Note: This assumes row-major winding (Fill X, then Y)
        let col = center_idx % layout_w as f32;
        let row = (center_idx / layout_w as f32).floor();

        let x = start_x + (col * cell_w);
        let y = 0.0; // Centered in Y; layout is XZ plane in Z-up space.
        let z = start_z - (row * cell_h);

        offsets.push(HeadLayout { x, y, z });
    }

    offsets
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixture_kinematics::rig_position;
    use luma_scene::coords::three_pose_from_data_d;
    use luma_scene::venue::{NodeKind, Params};
    use std::f64::consts::FRAC_PI_2;

    /// A resolved fixture node whose frame is the one a stored `(pos, rot)` row
    /// described. The expectations below are written in that convention because
    /// it is the convention the rig is measured in, not because a fixture still
    /// carries a triple.
    fn pose(pos: [f64; 3], rot: [f64; 3]) -> NodePose {
        NodePose {
            node: "fixture".into(),
            kind: NodeKind::Fixture,
            catalog_ref: None,
            label: None,
            parent: None,
            world: three_pose_from_data_d(pos, rot),
            array_index: None,
            params: Params::default(),
        }
    }

    /// One head at a millimetre offset, placed by the same pair of helpers every
    /// caller uses.
    fn placed(base: [f64; 3], rot: [f64; 3], offset: [f32; 3]) -> [f32; 3] {
        let geom = FixtureGeometry::unauthored(vec![Vec3::from(offset) / 1000.0]);
        rig_position(&geom, &fixture_mount(&pose(base, rot)), 0).to_array()
    }

    fn close(got: [f32; 3], want: [f32; 3]) {
        for a in 0..3 {
            assert!(
                (got[a] - want[a]).abs() < 1e-5,
                "axis {a}: got {got:?}, want {want:?}"
            );
        }
    }

    /// The layout's vertical (local Z) offset is a *height*: unrotated, it must
    /// land on world Z, not on world Y (the depth axis).
    #[test]
    fn vertical_offset_lands_on_world_z() {
        close(
            placed([1.0, 2.0, 3.0], [0.0; 3], [0.0, 0.0, 500.0]),
            [1.0, 2.0, 3.5],
        );
    }

    /// A vertically stacked pixel bar spreads along world Z.
    #[test]
    fn stacked_heads_spread_along_world_z() {
        let base = [0.0, 0.0, 2.0];
        let a = placed(base, [0.0; 3], [0.0, 0.0, -300.0]);
        let b = placed(base, [0.0; 3], [0.0, 0.0, 300.0]);
        assert!((b[2] - a[2] - 0.6).abs() < 1e-5, "{a:?} {b:?}");
        assert!(
            (b[1] - a[1]).abs() < 1e-6,
            "no spread in depth: {a:?} {b:?}"
        );
    }

    /// `rot_z` is the heading (rotation about the world up axis): it swings a
    /// horizontal offset out of X and into Y, leaving height untouched.
    ///
    /// The **sign** is the point. Stored space and the renderer's space are
    /// mirror images, so a positive heading swings a `+X` offset toward the
    /// house (`-Y`), not away from it. The test this replaces took `abs()` of
    /// the result and so passed either way — which is how a yaw could have been
    /// backwards in the app and nobody would have heard about it from here.
    #[test]
    fn a_positive_heading_swings_a_stage_right_offset_toward_the_house() {
        close(
            placed([0.0; 3], [0.0, 0.0, FRAC_PI_2], [1000.0, 0.0, 0.0]),
            [0.0, -1.0, 0.0],
        );
    }

    /// Height is invariant under heading — spinning a fixture on its base never
    /// moves a head up or down.
    #[test]
    fn height_is_invariant_under_heading() {
        close(
            placed([0.0; 3], [0.0, 0.0, 0.7], [0.0, 0.0, 1000.0]),
            [0.0, 0.0, 1.0],
        );
    }

    /// Whatever the housing says, a head's *placement* is the mount's business:
    /// the same layout under a floor mount lands mirrored about the fixture, and
    /// the beam that leaves it points up (`fixture_kinematics::Mount::normal`).
    #[test]
    fn a_floor_mount_flips_the_layout_with_the_fixture() {
        let up = fixture_mount(&pose([0.0, 0.0, 0.2], [std::f64::consts::PI, 0.0, 0.0]));
        assert!(up.normal().abs_diff_eq(Vec3::Z, 1e-6), "{:?}", up.normal());
        close(
            placed(
                [0.0, 0.0, 0.2],
                [std::f64::consts::PI, 0.0, 0.0],
                [0.0, 0.0, 300.0],
            ),
            [0.0, 0.0, -0.1],
        );
    }
}
