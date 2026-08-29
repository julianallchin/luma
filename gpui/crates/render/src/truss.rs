//! Procedural truss geometry: the whole F34 family from one generator.
//!
//! Three shapes — a straight [`Truss`] of any span, a [`Corner`] box with any
//! two to six of its faces open, and a [`Hinge`] of two half-boxes on a pin —
//! and they are all the same thing underneath: a list of [`Member`] tubes and a
//! list of [`EndFrame`] open faces, baked by one private function into one
//! triangle list. Nothing here is a mesh file.
//!
//! **Everything mates.** A piece's open face is an [`EndFrame`], and every end
//! frame in the family is the same square of chord centres with the same
//! outward normal, so [`EndFrame::mating`] bolts any piece to any other and the
//! chords line up. That is the whole reason the family is generated rather than
//! ripped: three catalogue meshes only mate if someone modelled them to.
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
//! same product. `truss_q30_box.glb`, the ripped corner, measures the same
//! 254 mm square in Ø44.4 mm tube on a 304.8 mm (12") cube, its chords cut
//! 6.4 mm short at each end to leave room for a connection plate flush with the
//! cube face — which is where [`CONNECTOR_DEPTH_M`] comes from. It spends
//! 18 144 triangles on that one block; a six-way [`Corner`] here spends 1 584,
//! and a 3 m run 2 640.

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

/// How far a connector collar stands back into the piece from its open face.
///
/// The ripped Q30 block cuts its chords this far short at each end to leave
/// room for a connection plate flush with the cube face; a collar occupies the
/// same slot, on its own side of the face plane, so two bolted pieces' collars
/// meet rather than interpenetrate.
pub const CONNECTOR_DEPTH_M: f32 = 0.0064;

/// How much wider than its chord a connector collar is. A spigot block's
/// couplers are a visible step up in diameter, and that step is the only thing
/// that says "this face bolts to something" at rig distance.
const CONNECTOR_FLARE: f32 = 1.35;

/// Chord centre offset from the axis: half the square, and the half-width of
/// every box in the family.
pub const HALF_SQUARE_M: f32 = SQUARE_M / 2.0;

/// Half-width of the piece's outside surface — the chord centre square plus a
/// chord radius. What a bounding box is; a flared collar stands slightly proud
/// of it.
pub const OUTER_M: f32 = HALF_SQUARE_M + CHORD_DIAMETER_M / 2.0;

/// The straightest and the sharpest a [`Hinge`] opens. Beyond 180° the two
/// leaves are back through each other, which is not a joint.
pub const MAX_HINGE_DEG: f32 = 180.0;

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

impl Member {
    /// The same tube carried through a rigid transform. Radius is untouched:
    /// the family has no scale parameter, so a transform that would change it
    /// is a caller bug rather than a case to handle.
    #[must_use]
    pub fn transformed(self, m: glam::Mat4) -> Self {
        Self {
            start: m.transform_point3(self.start),
            end: m.transform_point3(self.end),
            ..self
        }
    }
}

/// The pose of one open face of a piece: where another piece bolts on, which
/// way that is out of the lattice, and how the square is rolled about it.
///
/// A full frame, not a position and a normal, because the section is square:
/// two pieces whose normals oppose can still meet 45° out of register, and
/// nothing in a position-and-normal pair says so. `up` is what makes
/// [`Self::mating`] a single answer instead of a family of them.
///
/// This is the geometric half of a truss-end socket. Wiring it into
/// [`luma_scene::snap`] is the venue graph's business, not the renderer's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndFrame {
    /// Centre of the end face, in piece-local space.
    pub position: Vec3,
    /// Unit normal pointing out of the lattice.
    pub normal: Vec3,
    /// Unit vector perpendicular to `normal`, fixing the roll of the square.
    pub up: Vec3,
}

impl EndFrame {
    /// The same face carried through a rigid transform.
    ///
    /// `m` must be a rotation and a translation. A scale would leave `normal`
    /// and `up` un-normalized and every frame downstream subtly wrong, which is
    /// why the family has no scale parameter — a truss is sized in metres of
    /// span, never stretched.
    #[must_use]
    pub fn transformed(self, m: glam::Mat4) -> Self {
        Self {
            position: m.transform_point3(self.position),
            normal: m.transform_vector3(self.normal),
            up: m.transform_vector3(self.up),
        }
    }

    /// The transform that bolts the piece owning `other` onto this face.
    ///
    /// Both frames are read in whatever space they are already in — pass a
    /// host's face already carried into world space by [`Self::transformed`],
    /// and the result is the guest's world matrix. The two faces end up
    /// coincident and their normals opposed, which is what "bolted together"
    /// means: the guest's chord square lands exactly on this one's.
    ///
    /// Total by construction. Every frame this module produces is orthonormal,
    /// so there is no degenerate pair to report.
    #[must_use]
    pub fn mating(self, other: EndFrame) -> glam::Mat4 {
        // Into-the-face, so the guest's outward normal comes back out of it.
        let target = frame_matrix(self.position, -self.normal, self.up);
        target * frame_matrix(other.position, other.normal, other.up).inverse()
    }
}

/// A rigid frame as a matrix: `normal` on X, `up` on Y, their cross on Z.
///
/// Right-handed by construction given perpendicular unit inputs, so the
/// inverse a mating takes is a true rotation and never mirrors a piece.
fn frame_matrix(position: Vec3, normal: Vec3, up: Vec3) -> glam::Mat4 {
    glam::Mat4::from_cols(
        normal.extend(0.0),
        up.extend(0.0),
        normal.cross(up).extend(0.0),
        position.extend(1.0),
    )
}

/// One of the six faces of a box in the family, and therefore one of the six
/// directions a piece can leave it in.
///
/// The axis-aligned closed set is the point: a corner block's ways are a
/// *subset of these six*, not an angle, so [`FaceSet`] can be a bitset and a
/// mesh key can be a number. Angles between the six are [`Hinge`]'s job.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Face {
    /// Toward `-X`, the upstream end of a straight run.
    #[serde(rename = "-x")]
    NegX,
    /// Toward `+X`, the downstream end of a straight run.
    #[serde(rename = "+x")]
    PosX,
    /// Downward.
    #[serde(rename = "-y")]
    NegY,
    /// Upward.
    #[serde(rename = "+y")]
    PosY,
    /// Toward `-Z`.
    #[serde(rename = "-z")]
    NegZ,
    /// Toward `+Z`.
    #[serde(rename = "+z")]
    PosZ,
}

impl Face {
    /// Every face, in bit order.
    pub const ALL: [Self; 6] = [
        Self::NegX,
        Self::PosX,
        Self::NegY,
        Self::PosY,
        Self::NegZ,
        Self::PosZ,
    ];

    /// Which of `x`, `y`, `z` this face is perpendicular to.
    #[must_use]
    pub const fn axis(self) -> usize {
        match self {
            Self::NegX | Self::PosX => 0,
            Self::NegY | Self::PosY => 1,
            Self::NegZ | Self::PosZ => 2,
        }
    }

    /// The opposite face. A pair of opposites is a straight-through way.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::NegX => Self::PosX,
            Self::PosX => Self::NegX,
            Self::NegY => Self::PosY,
            Self::PosY => Self::NegY,
            Self::NegZ => Self::PosZ,
            Self::PosZ => Self::NegZ,
        }
    }

    /// Unit outward normal.
    #[must_use]
    pub fn normal(self) -> Vec3 {
        let sign = if matches!(self, Self::PosX | Self::PosY | Self::PosZ) {
            1.0
        } else {
            -1.0
        };
        AXES[self.axis()] * sign
    }

    /// The face's open frame on a box of half-width [`HALF_SQUARE_M`].
    ///
    /// `up` is `+Y` except on the two faces `+Y` is the normal of, where it is
    /// `+Z`. Any perpendicular choice mates — the section is square, so roll is
    /// free in 90° steps — but it must be the *same* choice everywhere, or two
    /// pieces meet a quarter turn apart and their chords miss.
    #[must_use]
    pub fn frame(self) -> EndFrame {
        let normal = self.normal();
        EndFrame {
            position: normal * HALF_SQUARE_M,
            normal,
            up: if self.axis() == 1 { Vec3::Z } else { Vec3::Y },
        }
    }
}

/// The unit axes, indexed the way [`Face::axis`] indexes them.
const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

/// Which faces of a box are open.
///
/// A six-bit set, so a corner's whole parameter space is 64 values and its mesh
/// key is one of them. Serialized as a list of face names — `["-x", "+z"]` — so
/// a scene file says which ways the block goes rather than carrying a bitmask
/// nobody can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FaceSet(u8);

impl FaceSet {
    /// Both ends of a straight run: the fallback a degenerate set lands on.
    pub const THROUGH: Self = Self(1 << 0 | 1 << 1);

    /// Every face open — a six-way block.
    pub const ALL: Self = Self(0b0011_1111);

    /// The set holding exactly `faces`.
    #[must_use]
    pub fn of(faces: impl IntoIterator<Item = Face>) -> Self {
        faces.into_iter().fold(Self(0), Self::with)
    }

    /// The same set with `face` open.
    #[must_use]
    pub const fn with(self, face: Face) -> Self {
        Self(self.0 | 1 << face as u8)
    }

    /// Whether `face` is open.
    #[must_use]
    pub const fn contains(self, face: Face) -> bool {
        self.0 & 1 << face as u8 != 0
    }

    /// How many faces are open — the block's way count.
    #[must_use]
    pub const fn ways(self) -> u32 {
        self.0.count_ones()
    }

    /// The open faces, in [`Face::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Face> {
        Face::ALL.into_iter().filter(move |&f| self.contains(f))
    }
}

impl serde::Serialize for FaceSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.iter().collect::<Vec<_>>(), s)
    }
}

impl<'de> serde::Deserialize<'de> for FaceSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::of(Vec::<Face>::deserialize(d)?))
    }
}

/// A continuous F34 lattice of a whole number of panels.
///
/// The span is the only authored parameter and it is quantized on the way in,
/// so there is no such thing as a truss of an unbuildable length: see
/// [`Truss::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truss {
    panels: u32,
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
        }
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
    ///
    /// Same shape as a [`Corner`]'s `-X` and `+X` faces, pushed out to the
    /// span: that is what lets a stick bolt to a block. Only the position
    /// differs, and it has to — a corner's faces sit on its own half-width.
    #[must_use]
    pub fn end_frames(self) -> [EndFrame; 2] {
        let half = self.span_m() / 2.0;
        [Face::NegX, Face::PosX].map(|f| EndFrame {
            position: f.normal() * half,
            ..f.frame()
        })
    }

    /// Stable identity of this truss's geometry in the frame's mesh bank.
    ///
    /// Every parameter that changes a vertex is in the key, which is the
    /// contract [`MeshData::key`] states — and the panel count is the only
    /// parameter there is, so two trusses of one span share one upload.
    #[must_use]
    pub fn mesh_key(self) -> String {
        format!("procedural/truss/{}", self.panels)
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
        // Listed *cyclically* around the square, which [`FACES`] indexes into
        // — reordering these silently turns the brace zigzag into a pair of
        // diagonals through the section.
        let corners = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)]
            .map(|(y, z)| Vec3::new(0.0, y, z) * HALF_SQUARE_M);
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
        bake(self.members(), self.end_frames())
    }
}

/// A box of chord tube with some of its faces open: the junction the family
/// turns corners with.
///
/// Always the twelve edges of a [`SQUARE_M`] cube, whatever the ways — that is
/// how a real box corner is welded, and it is why every face of every block is
/// the same square of chord centres and therefore mates. What [`FaceSet`]
/// changes is the *treatment*: an open face is left open, carrying coupler
/// collars and an [`EndFrame`]; a closed face is braced across its diagonal and
/// bolts to nothing. That is also what makes the way count legible at a glance
/// — you see straight through a six-way and through only two faces of an L.
///
/// Way count is `faces.ways()` and is not stored beside it. Two fields that can
/// disagree about the same fact are a bug waiting for the one caller that sets
/// only one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corner {
    faces: FaceSet,
}

impl Corner {
    /// A block open on `faces`, with fewer than two ways widened to a
    /// straight-through pair.
    ///
    /// A block with one open face is a cap and with none is a paperweight;
    /// neither is a junction, and neither is worth an error the frame builder
    /// would have to decide what to draw for. Two is the floor, the same way
    /// one panel is [`Truss::new`]'s.
    #[must_use]
    pub fn new(faces: FaceSet) -> Self {
        let faces = if faces.ways() >= 2 {
            faces
        } else {
            FaceSet::THROUGH
        };
        Self { faces }
    }

    /// The open faces.
    #[must_use]
    pub const fn faces(self) -> FaceSet {
        self.faces
    }

    /// Ways: two for an L or a straight-through, three for a T, up to six.
    /// Never below two — see [`Self::new`].
    #[must_use]
    pub const fn ways(self) -> u32 {
        self.faces.ways()
    }

    /// One frame per open face, in [`Face::ALL`] order.
    pub fn end_frames(self) -> impl Iterator<Item = EndFrame> {
        self.faces.iter().map(Face::frame)
    }

    /// Stable identity of this block's geometry in the frame's mesh bank.
    ///
    /// Fifty-seven distinct blocks exist — 64 face sets, of which the empty
    /// one and the six singles collapse onto [`FaceSet::THROUGH`] — so the bank
    /// cannot be flooded by a venue full of corners the way a continuous
    /// parameter would flood it.
    #[must_use]
    pub fn mesh_key(self) -> String {
        format!("procedural/truss/corner/{}", self.faces.0)
    }

    /// The twelve cube edges in chord tube, then one brace across each closed
    /// face's diagonal.
    pub fn members(self) -> impl Iterator<Item = Member> {
        let edges = (0..3).flat_map(|axis| {
            let (b, c) = perpendicular(axis);
            SIGN_PAIRS.into_iter().map(move |(sb, sc)| {
                let off = AXES[b] * sb * HALF_SQUARE_M + AXES[c] * sc * HALF_SQUARE_M;
                Member {
                    start: off - AXES[axis] * HALF_SQUARE_M,
                    end: off + AXES[axis] * HALF_SQUARE_M,
                    radius: CHORD_DIAMETER_M / 2.0,
                }
            })
        });
        let braces = Face::ALL
            .into_iter()
            .filter(move |&f| !self.faces.contains(f))
            .map(|face| {
                let (b, c) = perpendicular(face.axis());
                let plane = face.normal() * HALF_SQUARE_M;
                let diagonal = (AXES[b] + AXES[c]) * HALF_SQUARE_M;
                Member {
                    start: plane - diagonal,
                    end: plane + diagonal,
                    radius: BRACE_DIAMETER_M / 2.0,
                }
            });
        edges.chain(braces)
    }

    /// The block as one uploadable triangle list, plated on its open faces.
    #[must_use]
    pub fn mesh(self) -> MeshData {
        bake(self.members(), self.end_frames())
    }
}

/// Two half-boxes on a vertical pin: the joint that turns a run by an angle the
/// catalogue does not stock.
///
/// The only piece in the family with a continuous parameter besides span, and
/// like span it is quantized on the way in — see [`Hinge::new`]. Each leaf is
/// half a [`Corner`]: four chords out to its open face, the four edges that
/// ring that face, and a plate over it. The pin runs vertically through the
/// centre, and is the axis the second leaf swings about.
///
/// At 0° the leaves are back to back and the joint is straight through; at 90°
/// a run entering at `-X` leaves at `-Z`. So the angle is the *deflection*, not
/// the included angle — 0 is a piece of straight truss, which is the reading
/// that makes `angle` composable with a run's total heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hinge {
    degrees: u16,
}

impl Hinge {
    /// A hinge deflecting `angle_deg`, clamped to `0..=180` and rounded to a
    /// whole degree.
    ///
    /// Rounding is what keeps the mesh bank finite: the angle changes every
    /// vertex, so an un-quantized hinge would intern a new mesh for every frame
    /// of a drag. A hundred and eighty-one is a catalogue; a float is not. A
    /// non-finite or out-of-range angle lands on the nearest legal one rather
    /// than failing — same reasoning as [`Truss::new`].
    #[must_use]
    // `f32::clamp` propagates NaN; `max` then `min` return the non-NaN operand,
    // which is what makes this total.
    #[allow(clippy::manual_clamp)]
    pub fn new(angle_deg: f32) -> Self {
        Self {
            degrees: angle_deg.round().max(0.0).min(MAX_HINGE_DEG) as u16,
        }
    }

    /// The built deflection in whole degrees, after clamping.
    #[must_use]
    pub fn angle_deg(self) -> f32 {
        f32::from(self.degrees)
    }

    /// The transform carrying the fixed leaf's geometry onto the swinging one.
    ///
    /// A half turn puts the second leaf's open face at `+X` — straight through
    /// — and the deflection is added to that. Composing rather than mirroring
    /// matters: a reflection would flip every triangle's winding and light the
    /// leaf from inside.
    fn swing(self) -> glam::Mat4 {
        glam::Mat4::from_rotation_y(std::f32::consts::PI + self.angle_deg().to_radians())
    }

    /// Both open faces, fixed leaf first.
    ///
    /// At 0° these are exactly a straight-through [`Corner`]'s two faces, which
    /// is the joint's own statement that a hinge is a corner with the angle
    /// pulled out as a parameter.
    #[must_use]
    pub fn end_frames(self) -> [EndFrame; 2] {
        let leaf = Face::NegX.frame();
        [leaf, leaf.transformed(self.swing())]
    }

    /// Stable identity in the frame's mesh bank; one entry per whole degree.
    #[must_use]
    pub fn mesh_key(self) -> String {
        format!("procedural/truss/hinge/{}", self.degrees)
    }

    /// Both leaves' tubes, then the pin.
    pub fn members(self) -> impl Iterator<Item = Member> {
        let swing = self.swing();
        let fixed = leaf_members();
        let swinging = leaf_members().map(move |m| m.transformed(swing));
        let pin = std::iter::once(Member {
            start: Vec3::NEG_Y * HALF_SQUARE_M,
            end: Vec3::Y * HALF_SQUARE_M,
            radius: CHORD_DIAMETER_M / 2.0,
        });
        fixed.chain(swinging).chain(pin)
    }

    /// The joint as one uploadable triangle list, plated on both open faces.
    #[must_use]
    pub fn mesh(self) -> MeshData {
        bake(self.members(), self.end_frames())
    }
}

/// One leaf of a [`Hinge`]: half a corner box, open at `-X`.
///
/// Four chords from the hinge plane out to the open face, then the four edges
/// that ring that face. Nothing closes the leaf at `x = 0`; that is where the
/// other leaf and the pin are, and a ring there would read as a bulge through
/// the middle of a straight joint.
fn leaf_members() -> impl Iterator<Item = Member> {
    let chords = SIGN_PAIRS.into_iter().map(|(sy, sz)| {
        let off = Vec3::new(0.0, sy, sz) * HALF_SQUARE_M;
        Member {
            start: off,
            end: off + Vec3::NEG_X * HALF_SQUARE_M,
            radius: CHORD_DIAMETER_M / 2.0,
        }
    });
    let ring = [1usize, 2].into_iter().flat_map(|axis| {
        let other = AXES[3 - axis];
        [-1.0f32, 1.0].into_iter().map(move |sign| {
            let off = Vec3::NEG_X * HALF_SQUARE_M + other * sign * HALF_SQUARE_M;
            Member {
                start: off - AXES[axis] * HALF_SQUARE_M,
                end: off + AXES[axis] * HALF_SQUARE_M,
                radius: CHORD_DIAMETER_M / 2.0,
            }
        })
    });
    chords.chain(ring)
}

/// The two axes a face's own axis is not.
const fn perpendicular(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// The four corners of a square, as signs on two axes.
const SIGN_PAIRS: [(f32, f32); 4] = [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)];

/// The four chord centres an open face presents, in the frame's own space.
///
/// One definition of "the square", used by the connectors, and the only thing a
/// mating has to get right. A face is square, so this is also why roll is free
/// in 90° steps and fixed in between.
#[must_use]
pub fn chord_centres(frame: EndFrame) -> [Vec3; 4] {
    let right = frame.normal.cross(frame.up);
    SIGN_PAIRS.map(|(a, b)| frame.position + (frame.up * a + right * b) * HALF_SQUARE_M)
}

/// The coupler collars standing back from one open face.
///
/// An open face is *open* — you see through a corner block, which is the whole
/// difference between a two-way and a six-way at a glance. What marks it as a
/// joint is a short flared collar on each chord end, set back into the piece by
/// [`CONNECTOR_DEPTH_M`] so two bolted pieces' collars meet at the face plane
/// instead of interpenetrating. This replaces plating the aperture: a solid
/// plate over an open face draws exactly the wrong picture.
fn connectors(frame: EndFrame) -> impl Iterator<Item = Member> {
    chord_centres(frame).into_iter().map(move |centre| Member {
        start: centre,
        end: centre - frame.normal * CONNECTOR_DEPTH_M,
        radius: CHORD_DIAMETER_M / 2.0 * CONNECTOR_FLARE,
    })
}

/// Tubes and open faces into one uploadable triangle list.
///
/// The single bake for the whole family: every piece is `members` plus the
/// faces it opens, so a new shape is a new pair of iterators and no new mesh
/// code. Baked rather than instanced per member because the frame's draw list
/// is one draw per `(mesh, transform)` pair — a per-member draw would put two
/// hundred draw calls behind a single truss.
#[must_use]
fn bake(
    members: impl IntoIterator<Item = Member>,
    faces: impl IntoIterator<Item = EndFrame>,
) -> MeshData {
    let members = members.into_iter();
    let (lower, _) = members.size_hint();
    let mut vertices = Vec::with_capacity(lower * TUBE_VERTICES);
    let mut indices = Vec::with_capacity(lower * TUBE_INDICES);
    for member in members.chain(faces.into_iter().flat_map(connectors)) {
        push_tube(&mut vertices, &mut indices, member);
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
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
            // Plus eight coupler collars, four on each end.
            assert_eq!(mesh.vertices.len(), (members + 8) * TUBE_VERTICES);
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
        // The coupler collars stand proud of [`OUTER_M`], so they, not the
        // chords, are the cross-section's outermost surface.
        let outer = HALF_SQUARE_M + CHORD_DIAMETER_M / 2.0 * CONNECTOR_FLARE;
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
        // Every brace lies *in* a face plane. A corner list that is not cyclic
        // still passes the continuity check below while zigzagging across the
        // section's diagonals instead, which reads as a lattice with no faces.
        for brace in &braces {
            let on = |v: fn(Vec3) -> f32| {
                (v(brace.start) - v(brace.end)).abs() < 1e-6
                    && (v(brace.start).abs() - HALF_SQUARE_M).abs() < 1e-6
            };
            assert!(on(|p| p.y) || on(|p| p.z), "{brace:?} is off every face");
        }
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
    fn both_ends_are_couplered_and_the_key_is_the_span() {
        let truss = Truss::new(2.0);
        assert_eq!(truss.mesh_key(), "procedural/truss/4");
        // Four collars per end, on top of the lattice's own tubes.
        assert_eq!(
            truss.mesh().vertices.len(),
            (truss.members().count() + 8) * TUBE_VERTICES
        );
    }

    /// Two faces are bolted together when their chord squares coincide and
    /// their normals oppose. Order is not part of it: a piece may arrive
    /// rolled a quarter turn and still be bolted on.
    fn assert_mated(host: EndFrame, guest: EndFrame) {
        assert!(
            (host.position - guest.position).length() < 1e-6,
            "{host:?} vs {guest:?}"
        );
        assert!(
            (host.normal + guest.normal).length() < 1e-6,
            "{host:?} vs {guest:?}"
        );
        for point in chord_centres(guest) {
            assert!(
                chord_centres(host)
                    .iter()
                    .any(|h| (*h - point).length() < 1e-6),
                "chord at {point} is not on {host:?}"
            );
        }
    }

    #[test]
    fn a_stick_bolts_onto_every_face_of_every_corner() {
        let stick = Truss::new(3.0);
        for faces in [
            FaceSet::THROUGH,
            FaceSet::of([Face::PosX, Face::PosZ]),
            FaceSet::of([Face::NegX, Face::PosX, Face::NegY]),
            FaceSet::ALL,
        ] {
            let corner = Corner::new(faces);
            for host in corner.end_frames() {
                // A stick offers two ends; either bolts on.
                for guest in stick.end_frames() {
                    let mated = guest.transformed(host.mating(guest));
                    assert_mated(host, mated);
                }
                // And the far end has travelled exactly one span outward.
                let [near, far] = stick.end_frames();
                let placed = far.transformed(host.mating(near));
                assert!(
                    (placed.position - (host.position + host.normal * stick.span_m())).length()
                        < 1e-6,
                    "{placed:?} off {host:?}"
                );
            }
        }
    }

    #[test]
    fn corners_bolt_to_corners() {
        let host_block = Corner::new(FaceSet::ALL);
        let guest_block = Corner::new(FaceSet::of([Face::NegZ, Face::PosY]));
        for host in host_block.end_frames() {
            for guest in guest_block.end_frames() {
                assert_mated(host, guest.transformed(host.mating(guest)));
            }
        }
    }

    #[test]
    fn a_corner_is_a_cube_frame_whatever_its_ways() {
        let edges = |faces| {
            Corner::new(faces)
                .members()
                .filter(|m| m.radius > BRACE_DIAMETER_M)
                .count()
        };
        assert_eq!(edges(FaceSet::THROUGH), 12);
        assert_eq!(edges(FaceSet::ALL), 12);
        // One diagonal per closed face, so the member count falls as ways rise.
        for faces in [FaceSet::THROUGH, FaceSet::of([Face::PosX]), FaceSet::ALL] {
            let corner = Corner::new(faces);
            assert_eq!(
                corner.members().count(),
                12 + (6 - corner.ways()) as usize,
                "{faces:?}"
            );
        }
    }

    #[test]
    fn a_corner_needs_two_ways_and_says_how_many_it_has() {
        assert_eq!(Corner::new(FaceSet::default()).faces(), FaceSet::THROUGH);
        assert_eq!(Corner::new(FaceSet::of([Face::PosY])).ways(), 2);
        assert_eq!(
            Corner::new(FaceSet::of([Face::PosY])).faces(),
            FaceSet::THROUGH
        );
        assert_eq!(Corner::new(FaceSet::ALL).ways(), 6);
        for ways in 2..=6usize {
            let faces = FaceSet::of(Face::ALL.into_iter().take(ways));
            assert_eq!(Corner::new(faces).ways() as usize, ways);
            assert_eq!(Corner::new(faces).end_frames().count(), ways);
        }
    }

    #[test]
    fn every_corner_interns_under_its_own_key_and_stays_small() {
        let mut keys = std::collections::BTreeSet::new();
        for bits in 0..64u8 {
            let corner = Corner::new(FaceSet::of(
                Face::ALL.into_iter().filter(|f| bits & 1 << *f as u8 != 0),
            ));
            keys.insert(corner.mesh_key());
            let mesh = corner.mesh();
            // Twelve edges, at most four diagonals, four collars per way.
            assert!(
                mesh.vertices.len() <= (16 + 6 * 4) * TUBE_VERTICES,
                "{bits} {}",
                mesh.vertices.len()
            );
            assert!(mesh
                .indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len()));
        }
        // The empty set and the six singles collapse onto THROUGH, which is
        // itself one of the two-way sets: 64 requests, 57 distinct blocks.
        assert_eq!(keys.len(), 64 - 7);
    }

    #[test]
    fn a_hinge_at_zero_is_a_straight_through_corner() {
        let straight = Corner::new(FaceSet::THROUGH);
        let hinge = Hinge::new(0.0);
        for (block, joint) in straight.end_frames().zip(hinge.end_frames()) {
            assert!(
                (block.position - joint.position).length() < 1e-6
                    && (block.normal - joint.normal).length() < 1e-6,
                "{block:?} vs {joint:?}"
            );
        }
        // And a stick bolts to it exactly as it does to the block.
        let stick = Truss::new(1.0);
        for host in hinge.end_frames() {
            let [near, _] = stick.end_frames();
            assert_mated(host, near.transformed(host.mating(near)));
        }
    }

    #[test]
    fn a_hinge_deflects_a_run_by_its_angle() {
        for degrees in [0.0f32, 30.0, 45.0, 90.0, 180.0] {
            let [fixed, swinging] = Hinge::new(degrees).end_frames();
            // The run enters against `fixed`'s normal and leaves along
            // `swinging`'s, so the turn is the angle between those two.
            let turn = (-fixed.normal).angle_between(swinging.normal).to_degrees();
            assert!((turn - degrees).abs() < 1e-3, "{degrees} turned {turn}");
        }
    }

    #[test]
    fn hinge_angle_clamps_and_quantizes() {
        for (asked, built) in [
            (0.0, 0.0),
            (-40.0, 0.0),
            (44.6, 45.0),
            (90.0, 90.0),
            (180.0, 180.0),
            (400.0, 180.0),
            (f32::INFINITY, 180.0),
            (f32::NEG_INFINITY, 0.0),
            (f32::NAN, 0.0),
        ] {
            assert!(
                (Hinge::new(asked).angle_deg() - built).abs() < 1e-6,
                "{asked} built as {}",
                Hinge::new(asked).angle_deg()
            );
        }
        // Quantizing is what keeps the mesh bank finite over a drag.
        assert_eq!(Hinge::new(44.6).mesh_key(), Hinge::new(45.4).mesh_key());
        assert_ne!(Hinge::new(45.0).mesh_key(), Hinge::new(46.0).mesh_key());
    }

    #[test]
    fn a_hinge_is_two_leaves_and_a_pin() {
        let hinge = Hinge::new(90.0);
        assert_eq!(hinge.members().count(), 8 + 8 + 1);
        let mesh = hinge.mesh();
        // Seventeen tubes, plus four coupler collars on each of two faces.
        assert_eq!(mesh.vertices.len(), (17 + 8) * TUBE_VERTICES);
        assert!(mesh
            .indices
            .iter()
            .all(|&i| (i as usize) < mesh.vertices.len()));
        for v in mesh.vertices.iter() {
            assert!((Vec3::from(v.normal).length() - 1.0).abs() < 1e-4, "{v:?}");
        }
    }

    #[test]
    fn face_sets_are_a_readable_list_on_the_wire() {
        let faces = FaceSet::of([Face::NegX, Face::PosZ]);
        let json = serde_json::to_string(&faces).expect("face set serializes");
        assert_eq!(json, r#"["-x","+z"]"#);
        assert_eq!(
            serde_json::from_str::<FaceSet>(&json).expect("face set parses"),
            faces
        );
        // Order and repetition on the wire do not change the set.
        assert_eq!(
            serde_json::from_str::<FaceSet>(r#"["+z","-x","+z"]"#).expect("parses"),
            faces
        );
    }

    #[test]
    fn every_index_is_in_range_and_every_normal_is_unit() {
        let mesh = Truss::new(1.5).mesh();
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
