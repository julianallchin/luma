use crate::models::fixtures::FixtureDefinition;

#[derive(Debug, Clone, Copy)]
pub struct HeadLayout {
    pub x: f32, // Local offset X in mm
    pub y: f32, // Local offset Y in mm
    pub z: f32, // Local offset Z in mm
}

/// World position of one fixture head: rotate its local offset (mm, in the
/// fixture's local frame) by the fixture orientation and add the fixture's base
/// position. This is the single source of truth for how a primitive
/// (`fixture:head`) maps to a 3D point — used to build the eval engine's
/// `ResidentContext.positions` and (until it's deleted) the legacy selection
/// items.
///
/// Frames: stored/world data is **Z-up** (+X stage right, +Y back, +Z up) and so
/// is `HeadLayout` (`compute_head_offsets` lays heads out in the local XZ plane).
/// The visualizer works in Three's Y-up space and crosses over by swapping Y↔Z on
/// both the position and the Euler angles (`fixture-object.tsx`:
/// `position.set(posX, posZ, posY)`, `rotation.set(rotX, rotZ, rotY)`). This
/// function reproduces that mapping exactly: swap the local offset into Y-up,
/// apply the same `Rx(rot_x)·Ry(rot_z)·Rz(rot_y)` composition Three builds for
/// that Euler, then swap the result back to Z-up. A head mounted above the
/// fixture origin therefore lands above it in **world Z**, matching what you see
/// on screen — before this, the local vertical offset was (wrongly) added to
/// world Y, so a vertical pixel bar's heads marched into the depth axis.
///
/// `base`/result are meters; `rot` is radians `[rot_x, rot_y, rot_z]` as stored.
pub fn head_world_position(base: [f32; 3], rot: [f64; 3], offset: HeadLayout) -> [f32; 3] {
    // Local offset (mm → m), swapped from Z-up data space into the Y-up frame
    // the rotation below is expressed in.
    let lx = offset.x / 1000.0;
    let ly = offset.z / 1000.0;
    let lz = offset.y / 1000.0;

    // The stored Euler under the same Y-up remap.
    let rx = rot[0];
    let ry = rot[2];
    let rz = rot[1];

    // Rotate around Z (yaw).
    let (lx_z, ly_z) = (
        lx * rz.cos() as f32 - ly * rz.sin() as f32,
        lx * rz.sin() as f32 + ly * rz.cos() as f32,
    );
    let lz_z = lz;
    // Rotate around Y (pitch).
    let (lx_y, lz_y) = (
        lx_z * ry.cos() as f32 + lz_z * ry.sin() as f32,
        -lx_z * ry.sin() as f32 + lz_z * ry.cos() as f32,
    );
    let ly_y = ly_z;
    // Rotate around X (roll).
    let (ly_x, lz_x) = (
        ly_y * rx.cos() as f32 - lz_y * rx.sin() as f32,
        ly_y * rx.sin() as f32 + lz_y * rx.cos() as f32,
    );
    let lx_x = lx_y;

    // Back to Z-up data space (swap Y↔Z again).
    [base[0] + lx_x, base[1] + lz_x, base[2] + ly_x]
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
    use std::f64::consts::FRAC_PI_2;

    fn head(x: f32, y: f32, z: f32) -> HeadLayout {
        HeadLayout { x, y, z }
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
        let p = head_world_position([1.0, 2.0, 3.0], [0.0, 0.0, 0.0], head(0.0, 0.0, 500.0));
        close(p, [1.0, 2.0, 3.5]);
    }

    /// A vertically stacked pixel bar spreads along world Z.
    #[test]
    fn stacked_heads_spread_along_world_z() {
        let base = [0.0, 0.0, 2.0];
        let rot = [0.0, 0.0, 0.0];
        let a = head_world_position(base, rot, head(0.0, 0.0, -300.0));
        let b = head_world_position(base, rot, head(0.0, 0.0, 300.0));
        assert!((b[2] - a[2] - 0.6).abs() < 1e-5, "{a:?} {b:?}");
        assert!(
            (b[1] - a[1]).abs() < 1e-6,
            "no spread in depth: {a:?} {b:?}"
        );
    }

    /// `rot_z` is the heading (rotation about the world up axis): it swings a
    /// horizontal offset out of X and into Y, leaving height untouched.
    #[test]
    fn heading_swings_horizontal_offset_into_y() {
        let p = head_world_position(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, FRAC_PI_2],
            head(1000.0, 0.0, 0.0),
        );
        assert!(p[0].abs() < 1e-5, "{p:?}");
        assert!((p[1].abs() - 1.0).abs() < 1e-5, "{p:?}");
        assert!(p[2].abs() < 1e-5, "{p:?}");
    }

    /// Height is invariant under heading — spinning a fixture on its base never
    /// moves a head up or down.
    #[test]
    fn height_is_invariant_under_heading() {
        let p = head_world_position([0.0, 0.0, 0.0], [0.0, 0.0, 0.7], head(0.0, 0.0, 1000.0));
        close(p, [0.0, 0.0, 1.0]);
    }
}
