use crate::models::fixtures::FixtureDefinition;

#[derive(Debug, Clone, Copy)]
pub struct HeadLayout {
    pub x: f32, // Local offset X in mm
    pub y: f32, // Local offset Y in mm
    pub z: f32, // Local offset Z in mm
}

/// World position of one fixture head: rotate its local offset (mm, in the
/// fixture's local frame) by the fixture orientation, axis-remap, and add the
/// fixture's base position. This is the single source of truth for how a
/// primitive (`fixture:head`) maps to a 3D point — used to build the eval
/// engine's `ResidentContext.positions` and (until it's deleted) the legacy
/// selection items. Conventions, all inherited from the legacy mapping:
///   - rotations are interpreted with Y/Z swapped (legacy UI mapping);
///   - applied yaw(Z)→pitch(Y)→roll(X) to the local offset;
///   - the rotated Z component is added to base Y and rotated Y to base Z.
/// `base`/result are meters; `rot` is radians `[rot_x, rot_y, rot_z]` as stored.
pub fn head_world_position(base: [f32; 3], rot: [f64; 3], offset: HeadLayout) -> [f32; 3] {
    // Local offset in meters (Z-up, Y-forward data space).
    let lx = offset.x / 1000.0;
    let ly = offset.y / 1000.0;
    let lz = offset.z / 1000.0;

    // Interpret stored rotations with Y/Z swapped (legacy UI mapping).
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
