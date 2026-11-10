//! A face of cells behind one cone: the rectangle a patch of an LED panel, a
//! bar or a strobe lights the room from, and each of its cells' own light.
//!
//! A patch of cells is one cone ([`luma_scene::library::patches`]): from a
//! few metres away its cells light a wall as one source would. Close to the
//! face they do not. A point a hand's width in front of a red cell is lit
//! red, whatever the cells beside it are doing, and the haze there is red.
//! So the cone keeps its cells as a texture, and a point takes the colour of
//! the cells round its own place on the face, the nearest point of the face
//! to it: those within its beam's radius at its distance from that place.
//! The radius grows with the distance, so the colour blurs into the cone's
//! own — the mean of every cell — by the time the whole face is inside it,
//! and past that the panel is not read at all.
//!
//! The cone's shape is not the panel's business: its light still leaves a
//! disc at the patch's middle, as any cone's does. The patches are cut small
//! enough for that to hold.
//!
//! The cells are stored as a summed-area table, so the mean over any
//! rectangle of them is four reads, each interpolated: the sum of cells that
//! are constant across themselves is linear between the table's entries, and
//! the interpolation is exact. `shaders/panel.wgsl` reads it; [`Panel::colour`]
//! is the same arithmetic, for the tests.

use glam::{Vec2, Vec3};

use crate::frame::FixtureCone;

/// The cells behind one cone.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    /// Its cone's index in [`crate::Frame::fixture_cones`].
    pub cone: usize,
    /// The face's right, unit, world space. Its up is the cone's direction
    /// crossed with this.
    pub right: Vec3,
    /// Half the face's extent along right and up, metres.
    pub half: [f32; 2],
    /// Radius of one cell's own optic, metres: how much of the face a point
    /// touching it sees.
    pub optic: f32,
    /// Cells along right and up.
    pub grid: [u32; 2],
    /// Each cell's colour, row by row from the bottom left, scaled so that
    /// their mean is the cone's colour: a dark cell is black, and a lone lit
    /// one is as many times brighter than the cone as there are cells.
    pub cells: Vec<Vec3>,
}

impl Panel {
    /// The summed-area table: entry `(i, j)` is the sum of the cells left of
    /// column `i` and below row `j`, so it is one wider and taller than the
    /// grid.
    fn table(&self) -> Vec<Vec3> {
        let [columns, rows] = self.grid.map(|n| n as usize);
        let stride = columns + 1;
        let mut table = vec![Vec3::ZERO; stride * (rows + 1)];
        for row in 0..rows {
            let mut along = Vec3::ZERO;
            for column in 0..columns {
                along += self.cells[row * columns + column];
                table[(row + 1) * stride + column + 1] = table[row * stride + column + 1] + along;
            }
        }
        table
    }

    /// The colour `cone` casts toward the point `q` metres from its lens
    /// centre: `panel_colour` in `shaders/panel.wgsl`, term for term.
    #[must_use]
    pub fn colour(&self, cone: &FixtureCone, q: Vec3) -> Vec3 {
        let half = Vec2::from(self.half);
        let up = cone.direction.cross(self.right);
        let at = Vec2::new(q.dot(self.right), q.dot(up)).clamp(-half, half);
        let cos = cone.cos_beam.clamp(1e-3, 1.0);
        let off = (q - self.right * at.x - up * at.y).length();
        let radius = self.optic + off * (1.0 - cos * cos).sqrt() / cos;
        if radius >= 2.0 * half.max_element() {
            return cone.color;
        }
        let grid = Vec2::new(self.grid[0] as f32, self.grid[1] as f32);
        let lo = ((at - radius + half) / (2.0 * half)).clamp(Vec2::ZERO, Vec2::ONE) * grid;
        let hi = ((at + radius + half) / (2.0 * half)).clamp(Vec2::ZERO, Vec2::ONE) * grid;
        let table = self.table();
        let stride = self.grid[0] as usize + 1;
        let sum = |p: Vec2| {
            let cell = p.floor().min(grid - 1.0);
            let f = p - cell;
            let o = cell.y as usize * stride + cell.x as usize;
            table[o]
                .lerp(table[o + 1], f.x)
                .lerp(table[o + stride].lerp(table[o + stride + 1], f.x), f.y)
        };
        (sum(hi) - sum(Vec2::new(lo.x, hi.y)) - sum(Vec2::new(hi.x, lo.y)) + sum(lo))
            / ((hi.x - lo.x) * (hi.y - lo.y))
    }
}

/// Every panel's header and table in one buffer, and where each drawn cone's
/// panel starts in it. `kept` is each drawn cone's index in the frame, which
/// is what a panel names its cone by. A cone with no panel starts at zero,
/// which is why the buffer opens with a word nothing reads.
pub(crate) fn pack(panels: &[Panel], kept: &[usize]) -> (Vec<[f32; 4]>, Vec<f32>) {
    let mut words = vec![[0.0; 4]];
    let starts = kept
        .iter()
        .map(|cone| {
            let Some(panel) = panels.iter().find(|panel| panel.cone == *cone) else {
                return 0.0;
            };
            let start = words.len();
            let [right_x, right_y, right_z] = panel.right.to_array();
            words.push([right_x, right_y, right_z, panel.half[0]]);
            words.push([
                panel.half[1],
                panel.grid[0] as f32,
                panel.grid[1] as f32,
                panel.optic,
            ]);
            words.extend(
                panel
                    .table()
                    .into_iter()
                    .map(|sum| sum.extend(0.0).to_array()),
            );
            start as f32
        })
        .collect();
    (words, starts)
}

#[cfg(test)]
mod tests {
    use super::Panel;
    use crate::frame::FixtureCone;
    use glam::Vec3;
    use luma_scene::luminaire::Lens;

    /// A cone facing down `-Z` from the origin, a 30 degree beam.
    fn cone(color: Vec3) -> FixtureCone {
        FixtureCone {
            position: Vec3::ZERO,
            range: 30.0,
            direction: Vec3::NEG_Z,
            cos_beam: 15.0_f32.to_radians().cos(),
            color,
            intensity: 1.0,
            cos_field: 30.0_f32.to_radians().cos(),
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: Lens { radius: 0.02 },
        }
    }

    /// Eight cells by two, 4 cm apart, lit `cells`.
    fn panel(cells: Vec<Vec3>) -> Panel {
        Panel {
            cone: 0,
            right: Vec3::X,
            half: [0.16, 0.04],
            optic: 0.01,
            grid: [8, 2],
            cells,
        }
    }

    /// Red on the left half and blue on the right, the mean of both.
    fn split() -> (Panel, FixtureCone) {
        let (red, blue) = (Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 2.0));
        let cells = (0..16)
            .map(|i| if i % 8 < 4 { red } else { blue })
            .collect();
        (panel(cells), cone(Vec3::new(1.0, 0.0, 1.0)))
    }

    /// Every cell the cone's colour: every point is lit that colour, however
    /// close and wherever it is.
    #[test]
    fn an_even_face_is_its_cones_colour_everywhere() {
        let colour = Vec3::new(0.2, 0.7, 0.4);
        let (panel, cone) = (panel(vec![colour; 16]), cone(colour));
        for q in [
            Vec3::new(0.0, 0.0, -0.001),
            Vec3::new(-0.15, 0.03, -0.01),
            Vec3::new(0.4, -0.2, -0.05),
            Vec3::new(1.0, 2.0, -0.3),
            Vec3::new(0.0, 0.0, -9.0),
        ] {
            assert!(panel.colour(&cone, q).abs_diff_eq(colour, 1e-5), "{q}");
        }
    }

    /// A point touching a cell is lit by that cell alone, and one far enough
    /// off for its beam to span the face by the mean of them all.
    #[test]
    fn a_near_point_sees_its_own_cell_and_a_far_one_the_whole_face() {
        let (panel, cone) = split();
        let red = panel.colour(&cone, Vec3::new(-0.10, 0.0, -0.001));
        let blue = panel.colour(&cone, Vec3::new(0.10, 0.0, -0.001));
        assert!(red.x > 1.99 && red.z < 0.01, "{red}");
        assert!(blue.z > 1.99 && blue.x < 0.01, "{blue}");
        // Just past the face's own edge it is still the nearest cells'
        // colour, and far off to the side the whole face's, as from in front.
        let beside = panel.colour(&cone, Vec3::new(-0.3, 0.0, -0.001));
        assert!(beside.x > 1.99 && beside.z < 0.01, "{beside}");
        let aside = panel.colour(&cone, Vec3::new(-5.0, 0.0, -0.001));
        assert_eq!(aside, cone.color);
        let far = panel.colour(&cone, Vec3::new(-0.10, 0.0, -5.0));
        assert_eq!(far, cone.color);
    }

    /// Backing away from the face, the colour runs from the cell's to the
    /// cone's without a step, and never leaves the range between them.
    #[test]
    fn the_colour_blurs_into_the_cones_without_a_step() {
        let (panel, cone) = split();
        let mut previous = panel.colour(&cone, Vec3::new(-0.10, 0.0, 0.0));
        let mut depth = 0.0_f32;
        while depth < 3.0 {
            depth += 0.002;
            let colour = panel.colour(&cone, Vec3::new(-0.10, 0.0, -depth));
            assert!(
                (colour - previous).abs().max_element() < 0.02,
                "a step at {depth} m: {previous} to {colour}"
            );
            assert!(colour.x <= previous.x + 1e-5 && colour.x >= 1.0 - 1e-5);
            assert!(colour.z >= previous.z - 1e-5 && colour.z <= 1.0 + 1e-5);
            previous = colour;
        }
        assert_eq!(previous, cone.color);
    }

    /// A lone lit cell is the whole face's light: the face's mean over its
    /// cells is the cone's colour, and the cell itself that many times it.
    #[test]
    fn a_lone_cell_carries_the_faces_light() {
        let mut cells = vec![Vec3::ZERO; 16];
        cells[3] = Vec3::splat(16.0);
        let (panel, cone) = (panel(cells), cone(Vec3::ONE));
        // The cone faces down -Z with its right along X, so its up is -Y.
        let at_cell = panel.colour(&cone, Vec3::new(-0.02, 0.02, -0.001));
        let at_dark = panel.colour(&cone, Vec3::new(0.14, 0.02, -0.001));
        assert!(at_cell.abs_diff_eq(Vec3::splat(16.0), 1e-3), "{at_cell}");
        assert!(at_dark.abs_diff_eq(Vec3::ZERO, 1e-4), "{at_dark}");
        // Drawn second of three cones, the first of which was dropped: the
        // panel follows its cone to the cone's new place.
        let panel = Panel { cone: 1, ..panel };
        let (words, starts) = super::pack(&[panel], &[1, 2]);
        assert_eq!(starts, vec![1.0, 0.0]);
        // A header of two words, then the table, whose last entry is the sum
        // of every cell.
        assert_eq!(words.len(), 1 + 2 + 9 * 3);
        assert_eq!(words.last().unwrap()[..3], [16.0; 3]);
    }
}
