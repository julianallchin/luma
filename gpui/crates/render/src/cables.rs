//! Rigging cables: the black drops a flown piece hangs on.
//!
//! A flown piece is one with air under it — nothing standing between its
//! underside and the floor. That is a fact about geometry, not about the
//! authoring graph, so it is decided here from the world boxes of the pieces
//! the frame builder has just placed rather than from a `trim` field the
//! renderer would have to be handed.
//!
//! # The shared ceiling
//!
//! Every cable in a frame ends at one world height, and every one of them
//! fades out over the same band below it. Cables from pieces at different
//! trims therefore vanish together, as if into one dark grid over the room,
//! instead of each stopping at its own private altitude. The band is measured
//! from the top of the *whole* rig, so it moves only when the rig's tallest
//! piece does.
//!
//! # What the mesh carries
//!
//! One triangle strip per drop, in **world** space, with the fade already
//! baked into `uv.x` as an alpha ramp. The band is a scene-wide constant and
//! the shader is per-vertex, so baking is what lets `shaders/cables.wgsl` stay
//! a width expansion and a colour — no cable uniform, no globals field, and
//! nothing for a second call site to set differently. `uv.y` is the ±1 side of
//! the screen-facing quad the shader expands to.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::assets::Vertex;
use crate::frame::{Draw, MeshData};

/// How close a piece's underside comes to the floor before it counts as
/// standing on it.
const GROUND_M: f32 = 0.15;

/// Vertical slack for "resting on that piece's top". Tighter than a truss
/// section is tall, so two trusses bolted end to end are not read as one
/// standing on the other.
const CONTACT_M: f32 = 0.12;

/// Plan-view slack when asking whether one piece is under another. Two pieces
/// that merely touch at a face still count as stacked.
const PLAN_SLACK_M: f32 = 0.05;

/// Metres of footprint per drop point — a rigger's rule of thumb, not a
/// derivation.
const SPACING_M: f32 = 3.0;

/// How far in from the ends the outermost drops sit, as a fraction of the
/// footprint and as a hard cap. Real pick points are inboard of the ends; one
/// at the very tip reads as a mesh corner rather than as rigging.
const END_INSET_FRACTION: f32 = 0.12;
const MAX_INSET_M: f32 = 0.6;

/// Footprint length above which an axis gets at least two drops. Below it a
/// piece is small enough to hang off one.
const TWO_DROP_M: f32 = 1.0;

/// Cap per axis, so a pathological hundred-metre piece cannot fill the frame
/// with cable.
const MAX_DROPS_PER_AXIS: usize = 12;

/// Drops closer than this in plan are one drop. Adjacent pieces sharing a
/// joint — a corner block between two truss runs — would otherwise each put a
/// cable within centimetres of the other's.
const MERGE_M: f32 = 0.9;

/// Clear air above the rig's highest piece before the fade begins, and the
/// height it takes to reach nothing.
///
/// Long, so the dissolve has no locatable beginning — it is darker air the
/// higher you look rather than a band edge drawn across the room — but bounded
/// above by the headroom a *fitted* camera leaves. `Framing::fit` frames the
/// rig's bounding sphere with an 8% margin, which on a room-sized rig puts the
/// top of frame around eight metres over the highest piece; a band that
/// outruns that is cut off while still half opaque, and a cable severed by the
/// frame edge reads worse than a short fade. Their sum is what has to stay
/// under that headroom.
const FADE_MARGIN_M: f32 = 1.5;
const FADE_SPAN_M: f32 = 6.0;

/// Segments the fade band is cut into. The ramp is smooth in z, so it needs
/// vertices to interpolate between, and a longer span needs more of them to
/// stay under the eye's threshold for faceting.
const BAND_STEPS: usize = 16;

/// The world boxes of the pieces placed so far, and the cables they imply.
///
/// Accumulated as the frame builder walks `scene.pieces` because that is where
/// a piece's draws and their model matrices exist; asking the catalog again
/// afterwards would be a second reading of the same geometry.
#[derive(Default)]
pub(crate) struct Bodies {
    boxes: Vec<Body>,
    /// Local bounds by mesh index, so a mesh shared by many pieces is measured
    /// once per frame rather than once per piece.
    local: HashMap<usize, (Vec3, Vec3)>,
}

/// One placed piece, as the cable pass sees it: a world-space box.
struct Body {
    min: Vec3,
    max: Vec3,
}

impl Bodies {
    /// Record one piece, given the draws it expanded to.
    ///
    /// Total: a piece with no draws, or one whose meshes are empty, records
    /// nothing rather than a degenerate box at the origin.
    pub(crate) fn add(&mut self, draws: &[Draw], meshes: &[MeshData]) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for draw in draws {
            let Some(local) = self.bounds_of(draw.mesh, meshes) else {
                continue;
            };
            let (lo, hi) = corners(local, draw.model);
            min = min.min(lo);
            max = max.max(hi);
        }
        if min.x.is_finite() && max.x.is_finite() {
            self.boxes.push(Body { min, max });
        }
    }

    /// The cable geometry for this frame, or `None` when nothing is flown.
    pub(crate) fn mesh(&self) -> Option<MeshData> {
        let ceiling = self
            .boxes
            .iter()
            .fold(f32::NEG_INFINITY, |z, b| z.max(b.max.z));
        if !ceiling.is_finite() {
            return None;
        }
        let band = Band {
            start: ceiling + FADE_MARGIN_M,
            end: ceiling + FADE_MARGIN_M + FADE_SPAN_M,
        };

        // Highest first: where two pieces want a drop in the same place, the
        // upper one's is the one that is visible, and the lower one's would
        // pass through it.
        let mut order: Vec<usize> = (0..self.boxes.len()).collect();
        order.sort_by(|&a, &b| self.boxes[b].max.z.total_cmp(&self.boxes[a].max.z));

        let mut drops: Vec<Vec3> = Vec::new();
        for index in order {
            if !self.is_flown(index) {
                continue;
            }
            for drop in self.boxes[index].drops() {
                if drops
                    .iter()
                    .any(|kept| kept.truncate().distance(drop.truncate()) < MERGE_M)
                {
                    continue;
                }
                drops.push(drop);
            }
        }
        (!drops.is_empty()).then(|| build(&drops, band))
    }

    /// Whether a piece hangs: its underside is off the floor and no other
    /// piece's top meets it.
    fn is_flown(&self, index: usize) -> bool {
        let body = &self.boxes[index];
        if body.min.z <= GROUND_M {
            return false;
        }
        !self.boxes.iter().enumerate().any(|(other, under)| {
            other != index
                && (under.max.z - body.min.z).abs() <= CONTACT_M
                && under.overlaps_in_plan(body)
        })
    }

    /// One mesh's local bounds, measured once and remembered.
    fn bounds_of(&mut self, mesh: usize, meshes: &[MeshData]) -> Option<(Vec3, Vec3)> {
        if let Some(bounds) = self.local.get(&mesh) {
            return Some(*bounds);
        }
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for vertex in meshes.get(mesh)?.vertices.iter() {
            let p = Vec3::from(vertex.position);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        lo.x.is_finite().then(|| {
            self.local.insert(mesh, (lo, hi));
            (lo, hi)
        })
    }
}

impl Body {
    /// Whether two boxes share any ground, with a joint's worth of slack.
    fn overlaps_in_plan(&self, other: &Self) -> bool {
        self.min.x - PLAN_SLACK_M <= other.max.x
            && other.min.x - PLAN_SLACK_M <= self.max.x
            && self.min.y - PLAN_SLACK_M <= other.max.y
            && other.min.y - PLAN_SLACK_M <= self.max.y
    }

    /// Where this piece's cables meet it: a grid of points across its
    /// footprint, at its top.
    fn drops(&self) -> Vec<Vec3> {
        let mut points = Vec::new();
        for x in spread(self.min.x, self.max.x) {
            for y in spread(self.min.y, self.max.y) {
                points.push(Vec3::new(x, y, self.max.z));
            }
        }
        points
    }
}

/// The drop coordinates along one axis of a footprint.
///
/// One point for a footprint too small to carry two, otherwise evenly spaced
/// between the insets — which is what puts a long truss run's cables on its
/// centreline every few metres instead of on its corners.
fn spread(lo: f32, hi: f32) -> Vec<f32> {
    let length = hi - lo;
    let count = if length < TWO_DROP_M {
        1
    } else {
        ((length / SPACING_M).round() as usize).clamp(2, MAX_DROPS_PER_AXIS)
    };
    if count == 1 {
        return vec![(lo + hi) * 0.5];
    }
    let inset = (length * END_INSET_FRACTION).min(MAX_INSET_M);
    let (first, last) = (lo + inset, hi - inset);
    (0..count)
        .map(|i| first + (last - first) * i as f32 / (count - 1) as f32)
        .collect()
}

/// The height band every cable in a frame fades out over.
#[derive(Clone, Copy)]
struct Band {
    start: f32,
    end: f32,
}

impl Band {
    /// Opacity at a world height: solid below the band, gone above it.
    fn alpha(self, z: f32) -> f32 {
        let t = ((z - self.start) / (self.end - self.start)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }
}

/// The strips, as one world-space mesh.
fn build(drops: &[Vec3], band: Band) -> MeshData {
    let mut vertices = Vec::with_capacity(drops.len() * (BAND_STEPS + 2) * 2);
    let mut indices = Vec::with_capacity(drops.len() * (BAND_STEPS + 1) * 6);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    band.start.to_bits().hash(&mut hasher);
    band.end.to_bits().hash(&mut hasher);
    for drop in drops {
        // Millimetres: finer than any placement the room can express, coarse
        // enough that float noise cannot mint a new mesh key every frame.
        for axis in drop.to_array() {
            ((axis * 1000.0) as i32).hash(&mut hasher);
        }
        let base = vertices.len() as u32;
        let foot = drop.z.min(band.start);
        let heights = std::iter::once(foot).chain(
            (0..=BAND_STEPS)
                .map(|i| band.start + (band.end - band.start) * i as f32 / BAND_STEPS as f32),
        );
        let mut levels = 0u32;
        for z in heights {
            let alpha = band.alpha(z);
            for side in [-1.0, 1.0] {
                vertices.push(Vertex {
                    position: [drop.x, drop.y, z],
                    // Unread by `cables.wgsl`; kept valid so the mesh is a
                    // legal member of the shared vertex layout.
                    normal: [0.0, 0.0, 1.0],
                    uv: [alpha, side],
                    tangent: [1.0, 0.0, 0.0, 1.0],
                });
            }
            levels += 1;
        }
        for i in 0..levels - 1 {
            let q = base + i * 2;
            indices.extend_from_slice(&[q, q + 1, q + 2, q + 1, q + 3, q + 2]);
        }
    }
    MeshData {
        key: format!("::cables:{:016x}", hasher.finish()),
        vertices: Arc::from(vertices),
        indices: Arc::from(indices),
    }
}

/// A local box under a model matrix, as a world box: the eight corners, since
/// the matrix may rotate.
fn corners((lo, hi): (Vec3, Vec3), model: Mat4) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for x in [lo.x, hi.x] {
        for y in [lo.y, hi.y] {
            for z in [lo.z, hi.z] {
                let p = model.transform_point3(Vec3::new(x, y, z));
                min = min.min(p);
                max = max.max(p);
            }
        }
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(min: [f32; 3], max: [f32; 3]) -> Body {
        Body {
            min: Vec3::from(min),
            max: Vec3::from(max),
        }
    }

    fn bodies(list: Vec<Body>) -> Bodies {
        Bodies {
            boxes: list,
            local: HashMap::new(),
        }
    }

    #[test]
    fn a_piece_on_the_floor_is_not_flown() {
        let rig = bodies(vec![body([-1.0, -1.0, 0.0], [1.0, 1.0, 2.0])]);
        assert!(!rig.is_flown(0));
        assert!(rig.mesh().is_none());
    }

    #[test]
    fn a_piece_standing_on_another_is_not_flown() {
        let rig = bodies(vec![
            body([-1.0, -1.0, 0.0], [1.0, 1.0, 1.0]),
            body([-0.5, -0.5, 1.0], [0.5, 0.5, 1.4]),
        ]);
        assert!(!rig.is_flown(1));
    }

    #[test]
    fn a_truss_over_a_deck_is_flown() {
        let rig = bodies(vec![
            body([-2.0, -2.0, 0.0], [2.0, 2.0, 1.0]),
            body([-6.0, -0.15, 6.0], [6.0, 0.15, 6.3]),
        ]);
        assert!(rig.is_flown(1));
    }

    #[test]
    fn two_trusses_bolted_end_to_end_are_both_flown() {
        let rig = bodies(vec![
            body([-6.0, -0.15, 6.0], [0.0, 0.15, 6.3]),
            body([0.0, -0.15, 6.0], [6.0, 0.15, 6.3]),
        ]);
        assert!(rig.is_flown(0) && rig.is_flown(1));
    }

    #[test]
    fn a_long_run_drops_every_few_metres_inboard_of_its_ends() {
        let run = body([-6.0, -0.15, 6.0], [6.0, 0.15, 6.3]);
        let drops = run.drops();
        assert_eq!(drops.len(), 4);
        assert!(drops.iter().all(|d| (d.z - 6.3).abs() < 1e-6));
        assert!(drops[0].x > -6.0 && drops[3].x < 6.0);
        assert!(drops.iter().all(|d| d.y.abs() < 1e-6));
    }

    #[test]
    fn a_tiny_piece_gets_one_drop() {
        assert_eq!(
            body([-0.15, -0.15, 6.0], [0.15, 0.15, 6.3]).drops().len(),
            1
        );
    }

    #[test]
    fn the_fade_band_is_shared_by_every_cable() {
        let rig = bodies(vec![
            body([-6.0, -0.15, 4.0], [6.0, 0.15, 4.3]),
            body([-6.0, 3.0, 7.0], [6.0, 3.3, 7.3]),
        ]);
        let mesh = rig.mesh().expect("both pieces are flown");
        let top = mesh
            .vertices
            .iter()
            .fold(f32::NEG_INFINITY, |z, v| z.max(v.position[2]));
        assert!((top - (7.3 + FADE_MARGIN_M + FADE_SPAN_M)).abs() < 1e-4);
        // Every cable ends transparent at that one height, whatever it hangs.
        for v in mesh.vertices.iter() {
            if (v.position[2] - top).abs() < 1e-4 {
                assert!(v.uv[0] < 1e-6, "the top of a cable is invisible");
            }
        }
    }

    #[test]
    fn coincident_drops_merge() {
        let rig = bodies(vec![
            body([-1.0, -0.15, 6.0], [1.0, 0.15, 6.3]),
            body([-1.02, -0.15, 6.0], [0.98, 0.15, 6.3]),
        ]);
        let mesh = rig.mesh().expect("both are flown");
        let cables = mesh.vertices.len() / ((BAND_STEPS + 2) * 2);
        assert_eq!(cables, 2, "one shared pair, not four");
    }
}
