//! Procedural truss geometry: one continuous F34 lattice across a span.
//!
//! The rig is made of a handful of catalogue lengths bolted end to end, but the
//! *geometry* of a run is a repeating unit — four chords and a brace zigzag on
//! each of the four faces — so a run of any length is generated rather than
//! assembled from ripped meshes. Stick decomposition (which real segments a
//! length is built from) is a separate question the venue graph answers; it
//! does not change what the lattice looks like.
//!
//! **Local space** is the same one the `stage_lab` GLBs are authored in: glTF
//! Y-up, span along `+X`, square cross-section in the `YZ` plane, origin at the
//! centre. A procedural truss and an imported one therefore take the same pose.
//!
//! Dimensions are F34 / H30V: 290 mm square measured chord centre to chord
//! centre, Ø48.3 mm chords, Ø20 mm braces, 0.5 m panel pitch. The ripped
//! `truss_q30_1.22m.glb` it replaces is the smaller F33/Q30 (254 mm square,
//! Ø44 mm chords, 0.53 m pitch), so the two are near-proportional but not the
//! same product.

use glam::Vec3;

use crate::assets::{Material, Vertex};
use crate::frame::MeshData;

/// Chord centre-to-centre spacing, both across and vertically. The number the
/// product name carries: "F34" is this in millimetres.
pub const SQUARE_M: f32 = 0.290;

/// Outside diameter of the four main tubes.
pub const CHORD_DIAMETER_M: f32 = 0.0483;

/// Outside diameter of the zigzag bracing tubes.
pub const BRACE_DIAMETER_M: f32 = 0.020;

/// Distance between successive brace nodes on one chord. Two braces per panel
/// per face: down at the panel start, back up at its midpoint.
pub const PANEL_PITCH_M: f32 = 0.5;

/// Thickness of an end connection plate, when one is drawn.
pub const END_PLATE_THICKNESS_M: f32 = 0.006;

/// Vertices one tube contributes: a shared ring at each end for the sides,
/// plus a ring per cap, because a cap normal is the axis and a side normal is
/// radial.
const TUBE_VERTICES: usize = TUBE_SIDES * 4;

/// Indices one tube contributes: two triangles per side quad, plus a fan per
/// cap.
const TUBE_INDICES: usize = TUBE_SIDES * 6 + (TUBE_SIDES - 2) * 6;

/// The four faces, as *ordered* chord pairs into the corner list rather than
/// steps around the square: each face's zigzag starts at its first chord, so
/// listing the two `Z` faces from `+Y` and the two `Y` faces from `+Z` puts
/// opposite faces in phase. Walking the corners cyclically instead mirrors
/// every second face, and a truss whose near and far braces disagree reads as
/// a row of Xs from the side rather than as one zigzag.
const FACES: [(usize, usize); 4] = [(0, 1), (3, 2), (1, 2), (0, 3)];

/// Radial segments per tube. Twelve reads as round at rig distance and keeps a
/// six-metre run under three thousand vertices; the reference GLB spends ten
/// thousand on 1.2 m.
const TUBE_SIDES: usize = 12;

/// Longest run the generator will build, in panels. A span is authored, and an
/// authored number can be wrong; clamping keeps [`Truss::new`] total instead of
/// letting a stray value allocate for a two-hundred-metre lattice.
const MAX_PANELS: f32 = 400.0;

/// Mill-finish aluminium, matching the imported truss meshes' own material.
pub const ALUMINIUM: Material = Material {
    base_color: Vec3::new(0.7, 0.7, 0.72),
    metallic: 1.0,
    roughness: 0.4,
    emissive: Vec3::ZERO,
    normal_scale: 1.0,
    occlusion_strength: 1.0,
    flat_shading: false,
};

/// One straight tube of the lattice, in truss-local space.
///
/// Members are the whole shape: chords and braces differ only in their
/// endpoints and radius, which is what makes the family generatable at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Member {
    /// One end of the tube's axis.
    pub start: Vec3,
    /// The other end of the tube's axis.
    pub end: Vec3,
    /// Outside radius.
    pub radius: f32,
}

/// The pose of one end of a truss: where another piece bolts on, and which way
/// that is out of the lattice.
///
/// This is the geometric half of a truss-end socket. Wiring it into
/// [`luma_scene::snap`] is the venue graph's business, not the renderer's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndFrame {
    /// Centre of the end face, in truss-local space.
    pub position: Vec3,
    /// Unit normal pointing out of the lattice.
    pub normal: Vec3,
}

/// A continuous F34 lattice of a whole number of panels.
///
/// The span is the only authored parameter and it is quantized on the way in,
/// so there is no such thing as a truss of an unbuildable length: see
/// [`Truss::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truss {
    panels: u32,
    end_plates: bool,
}

impl Truss {
    /// A truss spanning `span_m` metres, snapped to the nearest whole panel
    /// with a one-panel minimum.
    ///
    /// Snapping here rather than at the call sites is what makes
    /// [`Self::span_m`] the truth about the geometry — a caller that asked for
    /// 3.2 m gets a 3.0 m truss and can see that it did. A non-finite or
    /// negative span lands on the minimum rather than failing: this is a
    /// display parameter, and there is no useful error for the frame builder
    /// to handle.
    #[must_use]
    // Not `clamp`: `f32::clamp` propagates NaN, while `max` then `min` return
    // the non-NaN operand and land a garbage span on one panel. That is the
    // whole reason this function has no `Result`.
    #[allow(clippy::manual_clamp)]
    pub fn new(span_m: f32) -> Self {
        let panels = (span_m / PANEL_PITCH_M).round().max(1.0).min(MAX_PANELS);
        Self {
            panels: panels as u32,
            end_plates: false,
        }
    }

    /// The same truss with connection plates drawn over its end faces.
    ///
    /// Off by default: plates are only visible where two runs meet, and a run
    /// of one truss per span draws four faces of them for nothing.
    #[must_use]
    pub const fn with_end_plates(self, end_plates: bool) -> Self {
        Self { end_plates, ..self }
    }

    /// Panels in the lattice; never zero.
    #[must_use]
    pub const fn panels(self) -> u32 {
        self.panels
    }

    /// The built span in metres, after snapping.
    #[must_use]
    pub fn span_m(self) -> f32 {
        self.panels as f32 * PANEL_PITCH_M
    }

    /// The built span in feet.
    ///
    /// Display only. Truss is quantized in metres here and in the catalogue;
    /// feet exist because riggers speak them, and nothing downstream may round
    /// trip through this.
    #[must_use]
    pub fn display_feet(self) -> f32 {
        self.span_m() / 0.3048
    }

    /// Both end faces, upstream end first.
    #[must_use]
    pub fn end_frames(self) -> [EndFrame; 2] {
        let half = self.span_m() / 2.0;
        [
            EndFrame {
                position: Vec3::new(-half, 0.0, 0.0),
                normal: Vec3::NEG_X,
            },
            EndFrame {
                position: Vec3::new(half, 0.0, 0.0),
                normal: Vec3::X,
            },
        ]
    }

    /// Stable identity of this truss's geometry in the frame's mesh bank.
    ///
    /// Every parameter that changes a vertex is in the key, which is the
    /// contract [`MeshData::key`] states: two trusses of one span share one
    /// upload, and a truss that gains end plates gets a new one.
    #[must_use]
    pub fn mesh_key(self) -> String {
        let plates = if self.end_plates { "+plates" } else { "" };
        format!("procedural/truss/{}{plates}", self.panels)
    }

    /// Every tube in the lattice: four chords, then the brace zigzag of each
    /// face in turn.
    ///
    /// The four chords run the full span at the corners of the square. Each
    /// face carries one continuous zigzag between its two chords, alternating
    /// every half panel, so consecutive braces share a node on a chord and the
    /// lattice reads as one member folded back and forth rather than as a row
    /// of separate Vs.
    pub fn members(self) -> impl Iterator<Item = Member> {
        let half = SQUARE_M / 2.0;
        let corners = [
            Vec3::new(0.0, half, half),
            Vec3::new(0.0, -half, half),
            Vec3::new(0.0, -half, -half),
            Vec3::new(0.0, half, -half),
        ];
        let x = self.span_m() / 2.0;
        let chords = corners.into_iter().map(move |c| Member {
            start: c - Vec3::X * x,
            end: c + Vec3::X * x,
            radius: CHORD_DIAMETER_M / 2.0,
        });

        let nodes = self.panels * 2;
        let braces = FACES.into_iter().flat_map(move |(first, second)| {
            let (a, b) = (corners[first], corners[second]);
            (0..nodes).map(move |k| {
                let at = |k: u32| -x + k as f32 * (PANEL_PITCH_M / 2.0);
                // Even nodes sit on the face's first chord, odd on its second;
                // one brace bridges each consecutive pair.
                let (from, to) = if k % 2 == 0 { (a, b) } else { (b, a) };
                Member {
                    start: from + Vec3::X * at(k),
                    end: to + Vec3::X * at(k + 1),
                    radius: BRACE_DIAMETER_M / 2.0,
                }
            })
        });
        chords.chain(braces)
    }

    /// The lattice as one uploadable triangle list.
    ///
    /// Baked rather than instanced per member: the frame's draw list is one
    /// draw per `(mesh, transform)` pair, so a per-member draw would put two
    /// hundred draw calls behind a single truss. Baking keeps that at one, and
    /// the mesh bank shares it across every truss of the same span — see
    /// [`Self::mesh_key`].
    #[must_use]
    pub fn mesh(self) -> MeshData {
        let members = 4 + 8 * self.panels as usize;
        let mut vertices = Vec::with_capacity(members * TUBE_VERTICES);
        let mut indices = Vec::with_capacity(members * TUBE_INDICES);
        for member in self.members() {
            push_tube(&mut vertices, &mut indices, member);
        }
        if self.end_plates {
            let outer = SQUARE_M / 2.0 + CHORD_DIAMETER_M / 2.0;
            for frame in self.end_frames() {
                let inset = frame.position - frame.normal * (END_PLATE_THICKNESS_M / 2.0);
                push_plate(&mut vertices, &mut indices, inset, outer);
            }
        }
        MeshData {
            key: String::new(),
            vertices: vertices.into(),
            indices: indices.into(),
        }
    }
}

/// Append one closed capped tube along `member`'s axis.
fn push_tube(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, member: Member) {
    let axis = member.end - member.start;
    let length = axis.length();
    if length < f32::EPSILON || member.radius <= 0.0 {
        return;
    }
    let along = axis / length;
    // Any vector off the axis works; +X is off it for every brace, and +Y is
    // off it for the chords that run along +X.
    let across = if along.x.abs() > 0.9 {
        Vec3::Y
    } else {
        Vec3::X
    }
    .cross(along)
    .normalize();
    // `(across, up, along)` is right-handed, which is what makes the side
    // winding below come out counter-clockwise seen from outside the tube.
    let up = along.cross(across);

    let ring = |i: usize| {
        let turn = i as f32 / TUBE_SIDES as f32 * std::f32::consts::TAU;
        across * turn.cos() + up * turn.sin()
    };

    // Sides: two rings sharing radial normals, so the tube shades smooth.
    let side = vertices.len() as u32;
    for (end, origin) in [(0u32, member.start), (1, member.end)] {
        for i in 0..TUBE_SIDES {
            let n = ring(i);
            vertices.push(Vertex {
                position: (origin + n * member.radius).to_array(),
                normal: n.to_array(),
                uv: [i as f32 / TUBE_SIDES as f32, end as f32],
                tangent: along.extend(1.0).to_array(),
            });
        }
    }
    for i in 0..TUBE_SIDES as u32 {
        let (a, b) = (i, (i + 1) % TUBE_SIDES as u32);
        let (c, d) = (a + TUBE_SIDES as u32, b + TUBE_SIDES as u32);
        indices.extend([side + a, side + d, side + c, side + a, side + b, side + d]);
    }

    // Caps: their own vertices, because a cap normal is the axis and a side
    // normal is radial.
    for (origin, normal) in [(member.start, -along), (member.end, along)] {
        let base = vertices.len() as u32;
        for i in 0..TUBE_SIDES {
            let n = ring(i);
            vertices.push(Vertex {
                position: (origin + n * member.radius).to_array(),
                normal: normal.to_array(),
                uv: [0.0, 0.0],
                tangent: across.extend(1.0).to_array(),
            });
        }
        for i in 1..TUBE_SIDES as u32 - 1 {
            if normal.dot(along) > 0.0 {
                indices.extend([base, base + i, base + i + 1]);
            } else {
                indices.extend([base, base + i + 1, base + i]);
            }
        }
    }
}

/// Append a square end plate of half-width `outer`, centred at `centre`,
/// spanning the YZ plane with `X` as its thickness.
fn push_plate(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, centre: Vec3, outer: f32) {
    let half = Vec3::new(END_PLATE_THICKNESS_M / 2.0, outer, outer);
    for (normal, u, v) in [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y, Vec3::X),
    ] {
        let face = centre + normal * half.dot(normal.abs());
        let (du, dv) = (u * half.dot(u.abs()), v * half.dot(v.abs()));
        let base = vertices.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            vertices.push(Vertex {
                position: (face + du * su + dv * sv).to_array(),
                normal: normal.to_array(),
                uv: [0.0, 0.0],
                tangent: u.extend(1.0).to_array(),
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_snaps_to_whole_panels_with_a_one_panel_floor() {
        for (asked, built) in [
            (0.0, 0.5),
            (-4.0, 0.5),
            (0.1, 0.5),
            (0.74, 0.5),
            (0.76, 1.0),
            (1.2192, 1.0),
            (3.0, 3.0),
            (3.2, 3.0),
            (12.25, 12.5),
        ] {
            let truss = Truss::new(asked);
            assert!(
                (truss.span_m() - built).abs() < 1e-6,
                "{asked} m built as {} m, wanted {built}",
                truss.span_m()
            );
        }
        assert_eq!(Truss::new(f32::NAN).panels(), 1);
        assert!((Truss::new(f32::INFINITY).span_m() - MAX_PANELS * PANEL_PITCH_M).abs() < 1e-3);
    }

    #[test]
    fn feet_are_display_only_and_do_not_round_trip() {
        let truss = Truss::new(6.0);
        assert!((truss.display_feet() - 19.685_04).abs() < 1e-3);
        // 6 m is not a whole number of feet, so anyone re-snapping the feet
        // value would land somewhere else. That is the point of the doc note.
        assert!((Truss::new(truss.display_feet()).span_m() - 6.0).abs() > 1.0);
    }

    #[test]
    fn member_count_and_vertex_count_scale_linearly_with_panels() {
        let mut previous = None;
        for panels in 1..=8u32 {
            let truss = Truss::new(panels as f32 * PANEL_PITCH_M);
            assert_eq!(truss.panels(), panels);
            // Four chords, plus two braces per panel on each of four faces.
            let members = truss.members().count();
            assert_eq!(members, 4 + 8 * panels as usize);
            let mesh = truss.mesh();
            assert_eq!(mesh.vertices.len(), members * TUBE_VERTICES);
            if let Some((prev_panels, prev_verts)) = previous {
                let step: usize = mesh.vertices.len() - prev_verts;
                assert_eq!(step, 8 * TUBE_VERTICES, "panel {prev_panels}->{panels}");
            }
            previous = Some((panels, mesh.vertices.len()));
        }
    }

    #[test]
    fn geometry_fills_the_span_and_the_square_and_nothing_more() {
        let truss = Truss::new(3.0);
        let mesh = truss.mesh();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in mesh.vertices.iter() {
            lo = lo.min(Vec3::from(v.position));
            hi = hi.max(Vec3::from(v.position));
        }
        let outer = SQUARE_M / 2.0 + CHORD_DIAMETER_M / 2.0;
        // The chords stop exactly on the end plane; the outermost brace meets
        // its chord there at an angle, so its end cap leans a few millimetres
        // past it. Under a brace radius, and invisible against a 48 mm chord.
        let overhang = hi.x - 1.5;
        assert!(
            (0.0..BRACE_DIAMETER_M / 2.0).contains(&overhang) && (lo.x + hi.x).abs() < 1e-5,
            "{lo} {hi}"
        );
        assert!(
            (hi.y - outer).abs() < 1e-4 && (hi.z - outer).abs() < 1e-4,
            "{hi}"
        );
        assert!(
            (lo.y + outer).abs() < 1e-4 && (lo.z + outer).abs() < 1e-4,
            "{lo}"
        );
    }

    #[test]
    fn brace_zigzag_is_continuous_along_each_face() {
        let truss = Truss::new(2.0);
        let braces: Vec<_> = truss.members().skip(4).collect();
        // Each face's braces are emitted in order; the end of one is the start
        // of the next, which is what makes it one folded member.
        for face in braces.chunks(truss.panels() as usize * 2) {
            for pair in face.windows(2) {
                assert!(
                    (pair[0].end - pair[1].start).length() < 1e-5,
                    "{:?} does not meet {:?}",
                    pair[0],
                    pair[1]
                );
            }
            let span = truss.span_m();
            assert!((face[0].start.x + span / 2.0).abs() < 1e-5);
            assert!((face[face.len() - 1].end.x - span / 2.0).abs() < 1e-5);
        }
    }

    #[test]
    fn end_frames_face_out_of_the_lattice() {
        let truss = Truss::new(4.0);
        let [near, far] = truss.end_frames();
        assert_eq!(near.position, Vec3::new(-2.0, 0.0, 0.0));
        assert_eq!(near.normal, Vec3::NEG_X);
        assert_eq!(far.position, Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(far.normal, Vec3::X);
    }

    #[test]
    fn end_plates_are_opt_in_and_change_the_mesh_key() {
        let plain = Truss::new(2.0);
        let plated = plain.with_end_plates(true);
        assert_ne!(plain.mesh_key(), plated.mesh_key());
        assert_eq!(plain.mesh_key(), "procedural/truss/4");
        assert!(plated.mesh().vertices.len() > plain.mesh().vertices.len());
        // Two boxes, six quads each.
        assert_eq!(
            plated.mesh().vertices.len() - plain.mesh().vertices.len(),
            2 * 6 * 4
        );
    }

    #[test]
    fn every_index_is_in_range_and_every_normal_is_unit() {
        let mesh = Truss::new(1.5).with_end_plates(true).mesh();
        assert!(mesh
            .indices
            .iter()
            .all(|&i| (i as usize) < mesh.vertices.len()));
        assert_eq!(mesh.indices.len() % 3, 0);
        for v in mesh.vertices.iter() {
            assert!((Vec3::from(v.normal).length() - 1.0).abs() < 1e-4, "{v:?}");
        }
    }
}
