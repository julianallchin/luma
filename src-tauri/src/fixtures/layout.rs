use crate::fixtures::models::FixtureDefinition;

#[derive(Debug, Clone, Copy)]
pub struct HeadLayout {
    pub x: f32, // Local offset X in mm
    pub y: f32, // Local offset Y in mm
    pub z: f32, // Local offset Z in mm
}

pub fn compute_head_offsets(def: &FixtureDefinition, mode_name: &str) -> Vec<HeadLayout> {
    // Find the active mode
    let mode = match def.modes.iter().find(|m| m.name == mode_name) {
        Some(m) => m,
        None => return vec![HeadLayout { x: 0.0, y: 0.0, z: 0.0 }], // Fallback
    };

    // If no heads defined (or 1 head implicit), just return center
    if mode.heads.is_empty() {
        return vec![HeadLayout { x: 0.0, y: 0.0, z: 0.0 }];
    }

    // Check physical layout dimensions
    let physical = match &def.physical {
        Some(p) => p,
        None => return vec![HeadLayout { x: 0.0, y: 0.0, z: 0.0 }; mode.heads.len()],
    };

    let width = physical.dimensions.as_ref().map(|d| d.width).unwrap_or(0.0);
    let height = physical.dimensions.as_ref().map(|d| d.height).unwrap_or(0.0);
    let depth = physical.dimensions.as_ref().map(|d| d.depth).unwrap_or(0.0);

    let layout_w = physical.layout.as_ref().map(|l| l.width).unwrap_or(1).max(1);
    let layout_h = physical.layout.as_ref().map(|l| l.height).unwrap_or(1).max(1);
    let layout_d = 1; // QLC+ usually doesn't do 3D grids, assume 1 layer deep

    // Ensure we don't divide by zero if dimensions are missing
    if width == 0.0 && height == 0.0 && depth == 0.0 {
        return vec![HeadLayout { x: 0.0, y: 0.0, z: 0.0 }; mode.heads.len()];
    }

    let mut offsets = Vec::with_capacity(mode.heads.len());

    // Calculate cell sizes
    let cell_w = width / layout_w as f32;
    let cell_h = height / layout_h as f32;
    let _cell_d = depth / layout_d as f32;

    // Center offsets (0,0 is middle of fixture)
    let start_x = -width / 2.0 + cell_w / 2.0;
    let start_y = -height / 2.0 + cell_h / 2.0;
    // let start_z = -depth / 2.0 + cell_d / 2.0;

    // Iterate heads and map to grid
    // QLC+ heads are usually row-major (X then Y)
    // But <Head> order in XML is what matters. We assume they match the layout grid order.
    // If there are more heads than grid slots, we clamp or loop (basic safety).
    
    for i in 0..mode.heads.len() {
        let col = (i as u32) % layout_w;
        let row = (i as u32 / layout_w) % layout_h;
        
        let x = start_x + (col as f32 * cell_w);
        let y = start_y + (row as f32 * cell_h);
        let z = 0.0; // Flat layout for now

        offsets.push(HeadLayout { x, y, z });
    }

    offsets
}
