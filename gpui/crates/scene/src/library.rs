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

/// Cells of one face that light the room as one source.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    /// The node the cells sit on.
    pub node: usize,
    /// Mean of the cells' centres, in `node`'s frame.
    pub centre: Vec3,
    /// The way the cells face.
    pub normal: Vec3,
    /// The face's right, in `node`'s frame; its up is `normal × right`.
    pub right: Vec3,
    /// Extent of the cells across the face, along right and up, metres.
    pub size: [f32; 2],
    /// Columns and rows of cells in the patch.
    pub grid: [usize; 2],
    /// Indices into [`Built::cells`], ascending.
    pub cells: Vec<usize>,
    /// Each of `cells`' column and row in the patch, from its left and its
    /// bottom.
    pub places: Vec<[usize; 2]>,
}

impl Patch {
    /// The patch `cells` make, each at its column and row in `places`,
    /// counted from anywhere: the patch counts them from its own first.
    fn of(
        built: &Built,
        node: usize,
        normal: Vec3,
        right: Vec3,
        cells: Vec<usize>,
        places: Vec<[usize; 2]>,
    ) -> Self {
        let axes = [right, normal.cross(right)];
        let centre = cells
            .iter()
            .map(|&cell| built.cells[cell].centre)
            .sum::<Vec3>()
            / cells.len() as f32;
        let size = [0, 1].map(|axis| {
            let (lo, hi) = cells
                .iter()
                .map(|&cell| {
                    let cell = &built.cells[cell];
                    (cell.centre.dot(axes[axis]), cell.size[axis] / 2.0)
                })
                .fold(
                    (f32::INFINITY, f32::NEG_INFINITY),
                    |(lo, hi), (at, half)| (lo.min(at - half), hi.max(at + half)),
                );
            hi - lo
        });
        let first = [0, 1].map(|axis| places.iter().map(|place| place[axis]).min().unwrap_or(0));
        let places: Vec<[usize; 2]> = places
            .iter()
            .map(|place| [0, 1].map(|axis| place[axis] - first[axis]))
            .collect();
        let grid = [0, 1].map(|axis| places.iter().map(|place| place[axis]).max().unwrap_or(0) + 1);
        Self {
            node,
            centre,
            normal,
            right,
            size,
            grid,
            cells,
            places,
        }
    }

    /// The part of this patch that is lit: the smallest rectangle of its
    /// columns and rows that holds every cell `lit` picks, or none when it
    /// picks none.
    ///
    /// A patch lights the room from its middle, through an opening its own
    /// size. With one cell of it lit that is the wrong place and the wrong
    /// size, by up to the patch's own: the beam of a chase would stand still
    /// while the chase crossed the patch, then jump. Its lit part is the lit
    /// cell itself, and the beam leaves from there.
    #[must_use]
    pub fn lit(&self, built: &Built, lit: impl Fn(usize) -> bool) -> Option<Self> {
        let held = self
            .cells
            .iter()
            .zip(&self.places)
            .filter(|(cell, _)| lit(**cell))
            .map(|(_, place)| (*place, *place))
            .reduce(|(lo, hi), (place, _)| {
                (
                    [lo[0].min(place[0]), lo[1].min(place[1])],
                    [hi[0].max(place[0]), hi[1].max(place[1])],
                )
            });
        let (lo, hi) = held?;
        let (cells, places) = self
            .cells
            .iter()
            .zip(&self.places)
            .filter(|(_, place)| (0..2).all(|axis| (lo[axis]..=hi[axis]).contains(&place[axis])))
            .map(|(cell, place)| (*cell, *place))
            .unzip();
        Some(Self::of(
            built,
            self.node,
            self.normal,
            self.right,
            cells,
            places,
        ))
    }
}

/// A face's cells as the patches that light the room: neighbours whose
/// centres are no more than `reach` metres to either side of their patch's
/// middle are one patch.
///
/// Two cells a few centimetres apart behind a wide optic light a wall
/// metres away as one source would: their pools differ by the gap between
/// them, against a beam metres across. `reach` is how much of that the caller
/// lets go (see [`crate::luminaire::patch_reach`]); under half a cell's
/// pitch every cell is its own patch. Each face's rows and columns are
/// dealt into patches as evenly as they divide, so the split depends on the
/// build alone and not on which cells are lit; [`Patch::lit`] is the part of
/// one that is.
#[must_use]
pub fn patches(built: &Built, reach: f32) -> Vec<Patch> {
    // Faces: cells on one node that face one way.
    let mut faces: Vec<(usize, Vec3, Vec<usize>)> = Vec::new();
    for (index, cell) in built.cells.iter().enumerate() {
        let Some(node) = cell.node else { continue };
        match faces
            .iter_mut()
            .find(|(n, normal, _)| *n == node && normal.dot(cell.normal) > 0.999)
        {
            Some((_, _, cells)) => cells.push(index),
            None => faces.push((node, cell.normal, vec![index])),
        }
    }
    let mut patches = Vec::new();
    for (node, normal, cells) in faces {
        let u = built.cells[cells[0]]
            .emitters
            .first()
            .map_or_else(|| normal.any_orthonormal_vector(), |emitter| emitter.u);
        let axes = [u, normal.cross(u)];
        let along = |cell: usize, axis: usize| built.cells[cell].centre.dot(axes[axis]);
        // Along each axis: the cells' distinct places in order (its columns,
        // or its rows), and how many patches they are dealt into.
        let lines: [(Vec<f32>, usize); 2] = [0, 1].map(|axis| {
            let mut places: Vec<f32> = cells.iter().map(|&cell| along(cell, axis)).collect();
            places.sort_by(f32::total_cmp);
            places.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
            let count = places.len();
            let pitch = (places[count - 1] - places[0]) / (count.max(2) - 1) as f32;
            let each = if pitch > 0.0 {
                ((2.0 * reach / pitch) as usize).clamp(1, count)
            } else {
                count
            };
            (places, count.div_ceil(each))
        });
        // A cell's column (or row) on the face, and the patch that takes it.
        let line_of = |cell: usize, axis: usize| {
            let places = &lines[axis].0;
            let at = along(cell, axis);
            places
                .partition_point(|place| *place < at - 1e-4)
                .min(places.len() - 1)
        };
        let patch_of =
            |cell: usize, axis: usize| line_of(cell, axis) * lines[axis].1 / lines[axis].0.len();
        let mut dealt: std::collections::BTreeMap<(usize, usize), Vec<usize>> =
            std::collections::BTreeMap::new();
        for &cell in &cells {
            dealt
                .entry((patch_of(cell, 1), patch_of(cell, 0)))
                .or_default()
                .push(cell);
        }
        for cells in dealt.into_values() {
            let places = cells
                .iter()
                .map(|&cell| [0, 1].map(|axis| line_of(cell, axis)))
                .collect();
            patches.push(Patch::of(built, node, normal, axes[0], cells, places));
        }
    }
    patches
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

    fn hyt() -> Built {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../harness/fixture-specs/oppsk/hyt-006-384sl.luma.json"),
        )
        .unwrap();
        luma_fixture::build(&luma_fixture::Fixture::from_json(&text).unwrap()).unwrap()
    }

    /// Whatever the reach, the patches are the face's cells, each once, and
    /// no patch's cells sit further than the reach from its middle.
    #[test]
    fn patches_deal_every_cell_once_and_stay_within_reach() {
        let b = hyt();
        let lit: Vec<usize> = (0..b.cells.len())
            .filter(|&cell| b.cells[cell].node.is_some())
            .collect();
        let mut fewest = usize::MAX;
        for reach in [0.0, 0.005, 0.02, 0.05, 0.1, 1.0] {
            let patches = patches(&b, reach);
            let mut dealt: Vec<usize> = patches.iter().flat_map(|p| p.cells.clone()).collect();
            dealt.sort_unstable();
            assert_eq!(dealt, lit, "reach {reach}");
            for patch in &patches {
                for &cell in &patch.cells {
                    let off = b.cells[cell].centre - patch.centre;
                    let across = (off - patch.normal * off.dot(patch.normal)).abs();
                    assert!(
                        across.max_element() <= reach + 1e-4,
                        "reach {reach}: a cell {across} from its patch's middle"
                    );
                }
            }
            assert!(patches.len() <= fewest, "a longer reach made more patches");
            fewest = patches.len();
        }
    }

    /// A patch's lit part is the rectangle of its cells round the lit ones:
    /// one lit cell is that cell alone, at its own place and size, and two
    /// at opposite corners are the whole patch again.
    #[test]
    fn a_patchs_lit_part_is_the_rectangle_round_its_lit_cells() {
        let b = hyt();
        let face = &patches(&b, 1.0)[0];
        assert_eq!(face.lit(&b, |_| false), None);
        assert_eq!(face.lit(&b, |_| true).as_ref(), Some(face));
        for &cell in &face.cells {
            let lone = face.lit(&b, |c| c == cell).unwrap();
            assert_eq!(lone.cells, vec![cell]);
            assert_eq!(lone.grid, [1, 1]);
            assert!(lone.centre.abs_diff_eq(b.cells[cell].centre, 1e-6));
            assert!((lone.size[0] - b.cells[cell].size[0]).abs() < 1e-6);
        }
        let at =
            |place: [usize; 2]| face.cells[face.places.iter().position(|p| *p == place).unwrap()];
        let (first, last) = (at([0, 0]), at([15, 2]));
        assert_eq!(
            face.lit(&b, |c| c == first || c == last).as_ref(),
            Some(face)
        );
        // Two cells of one row: the cells between them too, and no other row.
        let (left, right) = (at([3, 1]), at([6, 1]));
        let run = face.lit(&b, |c| c == left || c == right).unwrap();
        assert_eq!(run.grid, [4, 1]);
        assert_eq!(run.cells.len(), 4);
        let middle = (b.cells[left].centre + b.cells[right].centre) / 2.0;
        assert!(
            run.centre.abs_diff_eq(middle, 1e-5),
            "{} against {middle}",
            run.centre
        );
        assert!(run.size[0] < face.size[0] * 0.3 && run.size[1] < face.size[1] * 0.5);
    }

    /// A reach under half a pitch leaves every cell its own patch, the cell's
    /// own size and place; a reach past the face makes the face one patch.
    #[test]
    fn a_short_reach_keeps_cells_apart_and_a_long_one_joins_the_face() {
        let b = hyt();
        let apart = patches(&b, 0.0);
        assert_eq!(apart.len(), b.cells.len());
        for patch in &apart {
            let cell = &b.cells[patch.cells[0]];
            assert_eq!(patch.cells.len(), 1);
            assert!(patch.centre.abs_diff_eq(cell.centre, 1e-6));
            assert!((patch.size[0] - cell.size[0]).abs() < 1e-6);
            assert!((patch.size[1] - cell.size[1]).abs() < 1e-6);
        }
        let joined = patches(&b, 1.0);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].cells.len(), b.cells.len());
        // Sixteen columns of three rows, each place taken once, and a cell
        // further along the face's right in a later column.
        let face = &joined[0];
        assert_eq!(face.grid, [16, 3]);
        let mut places = face.places.clone();
        places.sort_unstable();
        places.dedup();
        assert_eq!(places.len(), b.cells.len());
        for (a, pa) in face.cells.iter().zip(&face.places) {
            for (c, pc) in face.cells.iter().zip(&face.places) {
                let ahead = (b.cells[*c].centre - b.cells[*a].centre).dot(face.right);
                if ahead > 1e-3 {
                    assert!(pc[0] > pa[0], "{pa:?} then {pc:?} at {ahead} m");
                }
            }
        }
        // The whole face: wider than any one cell both ways.
        assert!(joined[0].size[0] > 10.0 * apart[0].size[0]);
        assert!(joined[0].size[1] > 2.0 * apart[0].size[1]);
    }

    #[test]
    fn the_clamp_sits_above_the_pivot_and_the_lens_below_it() {
        let b = sharpy();
        let (lens, dir) = beam(&b, &worlds(&b, [0.0, 0.0]), 0).unwrap();
        assert!(clamp_height(&b) > 0.0);
        assert!(lens.y < 0.0 && dir.y < -0.99);
    }
}
