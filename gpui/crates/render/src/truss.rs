//! Procedural truss geometry: the whole F34 family from one generator.
//!
//! Three shapes — a straight [`Truss`] of any span, a [`Corner`] box with any
//! two to six of its faces open, and a [`Hinge`] of two half-boxes on a pin —
//! and they are all the same thing underneath: a list of [`Member`] tubes and a
//! list of faces, each carrying a connection plate and, where something bolts
//! on, four coupler bosses. One private function bakes both into one triangle
//! list. Nothing here is a mesh file.
//!
//! Behind every plate is an end ring: four tubes on the chord centre square, a
//! chord radius inside the face plane, so their outer surface is the face and
//! they read as a raised rim standing round the plate. That is what a truss end
//! looks like on the ripped stick, and it is the same ring however the piece
//! arrives at it — a corner's twelve cube edges already lay one in every face
//! plane.
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
//! cube face — which is where [`PLATE_THICKNESS_M`] comes from. Its plates are
//! cut with a diamond aperture on a square of four bolt holes; the aperture is
//! modelled here, the bolt holes are below a pixel at rig
//! distance and are not. It spends 18 144 triangles on that one block; a
//! six-way [`Corner`] here spends 1 776, a 3 m run 3 056, and a hinge 2 044.

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

/// Thickness of a connection plate, and therefore how far short of its own
/// face plane every chord in the family is cut.
///
/// The ripped Q30 block cuts its chords exactly this far short at each end so
/// its plate sits flush with the cube face. A plate lives entirely on its own
/// side of the face plane, so two bolted pieces' plates meet rather than
/// interpenetrate — which is what makes the plane [`EndFrame`] names the
/// mating plane and not a plane through solid metal.
pub const PLATE_THICKNESS_M: f32 = 0.0064;

/// How far a coupler boss reaches back into the piece from the plate it sits
/// behind.
const COUPLER_DEPTH_M: f32 = 0.024;

/// How far a coupler boss narrows over [`COUPLER_DEPTH_M`], as a fraction of
/// chord gauge.
///
/// It tapers *inward* rather than flaring out: a boss wider than its chord
/// would stand past the plate corners, and the ripped block has nothing
/// sticking out of its plate. The cone is what shows through the aperture.
const COUPLER_TAPER: f32 = 0.72;

/// Chord centre offset from the axis: half the square, and the half-width of
/// every box in the family.
pub const HALF_SQUARE_M: f32 = SQUARE_M / 2.0;

/// Half-width of the piece's outside surface: the chord centre square plus a
/// chord radius.
///
/// One number doing three jobs, because on a real block they *are* one number.
/// It is the half-width of a connection plate; it is where a box's face planes
/// sit, a chord radius outside the chord square, so the four edge tubes lying
/// in a face clear its plate instead of bulging through it; and it is
/// therefore what a bounding box is. The ripped block is built the same way —
/// 254 mm chord centres on a 304.8 mm cube — and it is why a block is bigger
/// than the stick it bolts to. Only a coupler boss stands proud of it.
pub const OUTER_M: f32 = HALF_SQUARE_M + CHORD_DIAMETER_M / 2.0;

/// Half-width of a connection plate: a shade inside [`OUTER_M`], so the round
/// edge of every tube it lies between shows around it.
///
/// A plate cut flush with the tubes swallows them, and a block whose tubes are
/// invisible reads as a folded sheet-metal box rather than as a weldment. The
/// ripped block shows a raised bead of tube around every one of its plates,
/// and this is what puts it back.
pub const PLATE_HALF_M: f32 = HALF_SQUARE_M + CHORD_DIAMETER_M / 2.0 * 0.62;

/// Half-diagonal of the diamond a plate is cut out with.
///
/// A plate over an open face has to be see-through or a six-way block reads as
/// a sealed crate; the ripped block solves that with a diamond aperture whose
/// tips reach 52% of the way to the plate edge, and this is that fraction on
/// the F34 plate, opened out a little because the F34 plate is the smaller of
/// the two next to its chords. Tips lie on the plate's own axes, flats on its diagonals —
/// so the four corners the chords land in are the solid part.
const APERTURE_M: f32 = PLATE_HALF_M * 0.62;

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
/// Members are the whole shape: chords, braces, pins and coupler bosses differ
/// only in their endpoints and their two radii, which is what makes the family
/// generatable at all. A cylinder is the case where the radii agree, so there
/// is no second primitive for the tapered spigots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Member {
    /// One end of the tube's axis.
    pub start: Vec3,
    /// The other end of the tube's axis.
    pub end: Vec3,
    /// Outside radius at [`Self::start`] and at [`Self::end`].
    pub radii: [f32; 2],
}

impl Member {
    /// A plain cylinder of constant `radius`.
    #[must_use]
    pub const fn tube(start: Vec3, end: Vec3, radius: f32) -> Self {
        Self {
            start,
            end,
            radii: [radius, radius],
        }
    }

    /// A truncated cone, `radii` read in the same order as the endpoints.
    #[must_use]
    pub const fn cone(start: Vec3, end: Vec3, radii: [f32; 2]) -> Self {
        Self { start, end, radii }
    }

    /// The same tube carried through a rigid transform. Radii are untouched:
    /// the family has no scale parameter, so a transform that would change
    /// them is a caller bug rather than a case to handle.
    #[must_use]
    pub fn transformed(self, m: glam::Mat4) -> Self {
        Self {
            start: m.transform_point3(self.start),
            end: m.transform_point3(self.end),
            ..self
        }
    }

    /// The widest the tube gets — what a bounding box has to allow for.
    #[must_use]
    pub fn max_radius(self) -> f32 {
        self.radii[0].max(self.radii[1])
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Face {
    /// Toward `-X`, the upstream end of a straight run.
    NegX,
    /// Toward `+X`, the downstream end of a straight run.
    PosX,
    /// Downward.
    NegY,
    /// Upward.
    PosY,
    /// Toward `-Z`.
    NegZ,
    /// Toward `+Z`.
    PosZ,
}

/// The wire name of every face, in [`Face::ALL`] order. One copy: serde, the
/// socket names a venue stores, and any log line all read it.
const FACE_NAMES: [&str; 6] = ["-x", "+x", "-y", "+y", "-z", "+z"];

impl serde::Serialize for Face {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Face {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = <String as serde::Deserialize>::deserialize(d)?;
        Self::from_name(&name)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown face {name}")))
    }
}

impl Face {
    /// The wire name: `-x`, `+y`, …
    #[must_use]
    pub fn as_str(self) -> &'static str {
        FACE_NAMES[self as usize]
    }

    /// The face with this wire name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        FACE_NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Self::ALL[i])
    }

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

    /// The face's frame on a box of half-width [`OUTER_M`].
    ///
    /// `up` is `+Y` except on the two faces `+Y` is the normal of, where it is
    /// `+Z`. Any perpendicular choice mates — the section is square, so roll is
    /// free in 90° steps — but it must be the *same* choice everywhere, or two
    /// pieces meet a quarter turn apart and their chords miss.
    #[must_use]
    pub fn frame(self) -> EndFrame {
        let normal = self.normal();
        EndFrame {
            position: normal * OUTER_M,
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

    /// Every tube in the lattice: four chords, an end ring at each end, then
    /// the brace zigzag of each face in turn.
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
        // Cut short of the end plane at each end so the plates sit flush, the
        // same way the ripped block cuts its cube edges.
        let chords = corners.into_iter().map(move |c| {
            Member::tube(
                c - Vec3::X * (x - PLATE_THICKNESS_M),
                c + Vec3::X * (x - PLATE_THICKNESS_M),
                CHORD_DIAMETER_M / 2.0,
            )
        });

        let rings = [-1.0f32, 1.0]
            .into_iter()
            .flat_map(move |side| end_ring(Vec3::X * side * (x - CHORD_DIAMETER_M / 2.0), 0));

        let nodes = self.panels * 2;
        let braces = FACES.into_iter().flat_map(move |(first, second)| {
            let (a, b) = (corners[first], corners[second]);
            (0..nodes).map(move |k| {
                let at = |k: u32| -x + k as f32 * (PANEL_PITCH_M / 2.0);
                // Even nodes sit on the face's first chord, odd on its second;
                // one brace bridges each consecutive pair.
                let (from, to) = if k % 2 == 0 { (a, b) } else { (b, a) };
                Member::tube(
                    from + Vec3::X * at(k),
                    to + Vec3::X * at(k + 1),
                    BRACE_DIAMETER_M / 2.0,
                )
            })
        });
        chords.chain(rings).chain(braces)
    }

    /// The lattice as one uploadable triangle list, plated at both ends.
    ///
    /// Baked rather than instanced per member: the frame's draw list is one
    /// draw per `(mesh, transform)` pair, so a per-member draw would put two
    /// hundred draw calls behind a single truss. Baking keeps that at one, and
    /// the mesh bank shares it across every truss of the same span — see
    /// [`Self::mesh_key`].
    #[must_use]
    pub fn mesh(self) -> MeshData {
        bake(self.members(), self.end_frames().map(open))
    }
}

/// A box of chord tube with some of its faces open: the junction the family
/// turns corners with.
///
/// Always the twelve edges of a [`SQUARE_M`] cube, whatever the ways — that is
/// how a real box corner is welded, and it is why every face of every block is
/// the same square of chord centres and therefore mates. What [`FaceSet`]
/// changes is the *treatment*: every face carries a plate, and an open one adds
/// four coupler bosses and an [`EndFrame`], while a closed one is braced across
/// its diagonal and bolts to nothing. The way count stays legible through the
/// plates' apertures — a closed face shows its diagonal, an open one shows the
/// inside of the block.
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
        // Every edge stops [`PLATE_THICKNESS_M`] short of both faces it runs
        // between, so all six plates sit flush — the ripped block's own
        // construction, and the reason its plates read as plates.
        let reach = OUTER_M - PLATE_THICKNESS_M;
        let edges = (0..3).flat_map(move |axis| {
            let (b, c) = perpendicular(axis);
            SIGN_PAIRS.into_iter().map(move |(sb, sc)| {
                let off = AXES[b] * sb * HALF_SQUARE_M + AXES[c] * sc * HALF_SQUARE_M;
                Member::tube(
                    off - AXES[axis] * reach,
                    off + AXES[axis] * reach,
                    CHORD_DIAMETER_M / 2.0,
                )
            })
        });
        let braces = Face::ALL
            .into_iter()
            .filter(move |&f| !self.faces.contains(f))
            .map(move |face| {
                let (b, c) = perpendicular(face.axis());
                // Tangent to the plate's inner face, so it shows through the
                // aperture instead of straddling the plate like a strap.
                let plane = face.normal() * (reach - BRACE_DIAMETER_M / 2.0);
                let diagonal = (AXES[b] + AXES[c]) * HALF_SQUARE_M;
                Member::tube(plane - diagonal, plane + diagonal, BRACE_DIAMETER_M / 2.0)
            });
        edges.chain(braces)
    }

    /// Every face and how it is finished: all six plated, the open ones
    /// coupled.
    fn plating(self) -> impl Iterator<Item = (EndFrame, Plating)> {
        Face::ALL.into_iter().map(move |face| {
            (
                face.frame(),
                if self.faces.contains(face) {
                    Plating::Open
                } else {
                    Plating::Blind
                },
            )
        })
    }

    /// The block as one uploadable triangle list, plated on every face.
    #[must_use]
    pub fn mesh(self) -> MeshData {
        bake(self.members(), self.plating())
    }
}

/// Two half-boxes on a vertical pin: the joint that turns a run by an angle the
/// catalogue does not stock.
///
/// The only piece in the family with a continuous parameter besides span, and
/// like span it is quantized on the way in — see [`Hinge::new`]. Each leaf is
/// half a [`Corner`] built the same way a corner is: four chords out to its
/// open face, a ring of edges at each end of them, coupler bosses, and a plate
/// over the open face. Set the angle to zero and the two of them are a
/// straight-through block with a barrel down one side.
///
/// **The pin is on the inside of the bend, not through the centre.** A run
/// bending toward `-Z` closes up on its `-Z` side and opens a wedge on its
/// `+Z` side, so the axis the swinging leaf turns about is the `-Z` edge of the
/// plane the leaves share. Pinning the centre instead is what drives two solid
/// leaves through each other the moment the angle leaves zero; pinning an edge
/// is what a book hinge is. The leaves stand [`HINGE_GAP_M`] back from that
/// plane on either side, which is the knuckle gap the lugs live in.
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
    /// — and the deflection is turned about [`Self::pin`] on top of that.
    /// Composing rather than mirroring matters: a reflection would flip every
    /// triangle's winding and light the leaf from inside.
    fn swing(self) -> glam::Mat4 {
        self.turn() * glam::Mat4::from_rotation_y(std::f32::consts::PI)
    }

    /// The deflection alone: a turn of `angle` about the pin axis.
    fn turn(self) -> glam::Mat4 {
        let pin = Self::pin();
        glam::Mat4::from_translation(pin)
            * glam::Mat4::from_rotation_y(self.angle_deg().to_radians())
            * glam::Mat4::from_translation(-pin)
    }

    /// A point on the pin axis: the `-Z` edge of the plane the leaves share.
    ///
    /// The axis itself is vertical, so any point on it will do and this is the
    /// one at mid-height.
    fn pin() -> Vec3 {
        Vec3::new(0.0, 0.0, -OUTER_M)
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

    /// Both leaves' tubes and knuckle lugs, then the pin.
    pub fn members(self) -> impl Iterator<Item = Member> {
        let swing = self.swing();
        let turn = self.turn();
        let reach = HINGE_GAP_M + CHORD_DIAMETER_M / 2.0;
        // Lugs are written in hinge space, not leaf space: they reach *across*
        // the knuckle gap to the pin, so the half turn that reverses a leaf
        // would point its lugs at the far corner instead.
        let fixed = leaf_members().chain(knuckles(FIXED_KNUCKLE_M, -reach));
        let swinging = leaf_members()
            .map(move |m| m.transformed(swing))
            .chain(knuckles(SWINGING_KNUCKLE_M, reach).map(move |m| m.transformed(turn)));
        let axis = Self::pin();
        // The pin runs past the leaves top and bottom, the way a hinge pin
        // stands proud of its knuckles — at this scale that overhang is the
        // only thing that reads as a pin rather than as another chord.
        let overhang = Vec3::Y * (OUTER_M + CHORD_DIAMETER_M);
        let pin = std::iter::once(Member::tube(axis - overhang, axis + overhang, PIN_RADIUS_M));
        fixed.chain(swinging).chain(pin)
    }

    /// The joint as one uploadable triangle list, plated on both open faces.
    #[must_use]
    pub fn mesh(self) -> MeshData {
        bake(self.members(), self.end_frames().map(open))
    }
}

/// Radius of the hinge pin and of the knuckle lugs that ride on it.
const PIN_RADIUS_M: f32 = CHORD_DIAMETER_M / 2.0 * 0.75;

/// How far back from the plane it shares with the other leaf each leaf stops.
///
/// Two solids that touch cannot hinge: rotating one about an axis in their
/// shared plane drives it straight into the other. The gap is what the
/// knuckles occupy on a real book hinge, and half of it is the clearance that
/// makes every angle in `0..=180` buildable.
pub const HINGE_GAP_M: f32 = CHORD_DIAMETER_M / 2.0;

/// Heights the fixed leaf's two knuckle lugs sit at, as a fraction of the
/// half-square. The swinging leaf's are elsewhere on the pin so the two leaves'
/// knuckles interleave instead of colliding at 0°.
const FIXED_KNUCKLE_M: f32 = HALF_SQUARE_M * 0.88;
const SWINGING_KNUCKLE_M: f32 = HALF_SQUARE_M * 0.42;

/// The two lugs tying one leaf's `-Z` corner to the pin, at `±height`, reaching
/// from `reach` on the leaf's own side of the knuckle gap.
fn knuckles(height: f32, reach: f32) -> impl Iterator<Item = Member> {
    [-1.0f32, 1.0].into_iter().map(move |sign| {
        Member::tube(
            Vec3::new(reach, sign * height, -OUTER_M),
            Vec3::new(0.0, sign * height, -OUTER_M),
            PIN_RADIUS_M,
        )
    })
}

/// One leaf of a [`Hinge`]: half a corner box, open at `-X`.
///
/// Four chords from the knuckle gap out to the open face, an [`end_ring`] at
/// each end of them, and a diagonal across each side face — the near ring is
/// what keeps the leaf from ending in four cut tubes when the joint is open,
/// and the far one is the rim behind the plate. Every vertex is at `x <= -HINGE_GAP_M`; that clearance is the
/// whole reason two leaves can turn about a shared edge without meeting.
fn leaf_members() -> impl Iterator<Item = Member> {
    let near = HINGE_GAP_M;
    let far = OUTER_M - PLATE_THICKNESS_M;
    let chords = SIGN_PAIRS.into_iter().map(move |(sy, sz)| {
        let off = Vec3::new(0.0, sy, sz) * HALF_SQUARE_M;
        Member::tube(
            off - Vec3::X * near,
            off - Vec3::X * far,
            CHORD_DIAMETER_M / 2.0,
        )
    });
    let rings = [near + CHORD_DIAMETER_M / 2.0, HALF_SQUARE_M]
        .into_iter()
        .flat_map(|x| end_ring(Vec3::NEG_X * x, 0));
    // One diagonal across each of the four side faces, the same treatment a
    // [`Corner`] gives a face nothing bolts to. Without them a leaf is a bare
    // cage, and a hinge that is not braced like a corner does not read as one.
    let braces = [1usize, 2].into_iter().flat_map(move |axis| {
        let other = AXES[3 - axis];
        [-1.0f32, 1.0].into_iter().map(move |sign| {
            let face = AXES[axis] * sign * HALF_SQUARE_M;
            Member::tube(
                face - Vec3::X * (near + CHORD_DIAMETER_M / 2.0) + other * HALF_SQUARE_M,
                face - Vec3::X * HALF_SQUARE_M - other * HALF_SQUARE_M,
                BRACE_DIAMETER_M / 2.0,
            )
        })
    });
    chords.chain(rings).chain(braces)
}

/// The rim behind one face: four tubes of chord gauge on the chord centre
/// square, mitred at the corners, lying in the plane through `plane`
/// perpendicular to `axis`.
///
/// Every plated face in the family stands one of these behind its plate, and
/// it is what makes an end read as an *end* rather than as four tubes stopped
/// short. The ripped stick has the same thing in square section, running from
/// its chord centre square (127 mm) out to its cube face (152.4 mm) and reaching
/// two of those widths back from the plate; stated as a chord-gauge tube on the
/// chord centres it lands on both numbers at once, because [`OUTER_M`] *is* the
/// chord square plus a chord radius. Round rather than square section because a
/// flat facet of mill aluminium turned away from the sun has no diffuse term to
/// catch and renders as a hole, while a tube carries a highlight along its
/// length — which is how the rip reads, and how every other member here already
/// does.
///
/// Sitting a chord radius inside the face plane, its outer surface is the face
/// plane exactly: the ring stands *around* the plate, a shade wider than
/// [`PLATE_HALF_M`], not proud of it. So the mating plane is still the plate's
/// own face and two bolted pieces meet plate to plate with their rims clear.
///
/// A [`Corner`] does not call this: its twelve cube edges already lay four
/// tubes on the chord square in every face plane, which is the same ring. One
/// definition of the rim, in two places it falls out of.
fn end_ring(plane: Vec3, axis: usize) -> impl Iterator<Item = Member> {
    let (b, c) = perpendicular(axis);
    [(b, c), (c, b)]
        .into_iter()
        .flat_map(move |(along, across)| {
            [-1.0f32, 1.0].into_iter().map(move |sign| {
                let off = plane + AXES[across] * sign * HALF_SQUARE_M;
                Member::tube(
                    off - AXES[along] * HALF_SQUARE_M,
                    off + AXES[along] * HALF_SQUARE_M,
                    CHORD_DIAMETER_M / 2.0,
                )
            })
        })
}

/// A face that bolts to something, in the shape [`bake`] wants.
fn open(frame: EndFrame) -> (EndFrame, Plating) {
    (frame, Plating::Open)
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

/// The coupler bosses standing back from one open face.
///
/// A short cone on each chord end, widest where it meets the plate and tapering
/// back to chord gauge. It lives entirely behind the plate, on its own side of
/// the mating plane, so two bolted pieces' couplers never meet in the same
/// millimetre of space — and it is the step in diameter, seen past the plate
/// corners, that says "this face bolts to something" at rig distance.
fn couplers(frame: EndFrame) -> impl Iterator<Item = Member> {
    let back = frame.normal * PLATE_THICKNESS_M;
    chord_centres(frame).into_iter().map(move |centre| {
        Member::cone(
            centre - back,
            centre - back - frame.normal * COUPLER_DEPTH_M,
            [
                CHORD_DIAMETER_M / 2.0,
                CHORD_DIAMETER_M / 2.0 * COUPLER_TAPER,
            ],
        )
    })
}

/// How one face of a piece is finished.
///
/// Every face of every piece carries a plate — that is what a truss end looks
/// like, and leaving one off to signal "open" made a block read as a wire
/// sculpture. What an open face adds is the four coupler bosses; what a blind
/// one has instead is the diagonal brace its owner already put in `members`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plating {
    /// Bolts to another piece: plate plus coupler bosses.
    Open,
    /// Plate only.
    Blind,
}

/// Vertices and indices one plate contributes.
const PLATE_VERTICES: usize = 48;
const PLATE_INDICES: usize = 96;

/// Append one connection plate: a square slab with a diamond aperture, its
/// outer face lying on `frame`'s plane and its body entirely behind it.
///
/// Modelled as a ring rather than a slab with a hole punched through it —
/// eight triangles a face, four quads of wall inside and out — because the
/// aperture is the point. A truss block you cannot see through is a crate.
fn push_plate(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, frame: EndFrame) {
    let right = frame.normal.cross(frame.up);
    // `(up, right, normal)` is right-handed, so listing a triangle in
    // increasing angle about the normal winds it counter-clockwise from
    // outside — which is what the back face then reverses.
    let at = |a: f32, b: f32, r: f32| frame.position + (frame.up * a + right * b) * r;
    // Interleaved around the ring: aperture tip, plate corner, tip, corner…
    let tips =
        [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)].map(|(a, b)| at(a, b, APERTURE_M));
    let corners =
        [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].map(|(a, b)| at(a, b, PLATE_HALF_M));

    let back = frame.normal * PLATE_THICKNESS_M;
    for (offset, normal) in [(Vec3::ZERO, frame.normal), (-back, -frame.normal)] {
        let base = vertices.len() as u32;
        for point in tips.into_iter().chain(corners) {
            vertices.push(Vertex {
                position: (point + offset).to_array(),
                normal: normal.to_array(),
                uv: [0.0, 0.0],
                tangent: frame.up.extend(1.0).to_array(),
            });
        }
        // Tips are 0..4 and corners 4..8; tip `k` sits between corner `k-1`
        // and corner `k` going round.
        let (tip, corner) = (|k: u32| base + k % 4, |k: u32| base + 4 + k % 4);
        for k in 0..4 {
            let ring = [
                [tip(k), corner(k), tip(k + 1)],
                [corner(k), corner(k + 1), tip(k + 1)],
            ];
            for face in ring {
                if normal.dot(frame.normal) > 0.0 {
                    indices.extend(face);
                } else {
                    indices.extend([face[0], face[2], face[1]]);
                }
            }
        }
    }

    // Walls: the plate edge outside, the aperture edge inside. Each quad gets
    // its own four vertices because its normal is neither face's.
    for (ring, outward) in [(tips, -1.0f32), (corners, 1.0)] {
        for k in 0..4 {
            let (a, b) = (ring[k], ring[(k + 1) % 4]);
            let edge = (b - a).normalize();
            let normal = edge.cross(frame.normal) * outward;
            let base = vertices.len() as u32;
            for point in [a, b, b - back, a - back] {
                vertices.push(Vertex {
                    position: point.to_array(),
                    normal: normal.to_array(),
                    uv: [0.0, 0.0],
                    tangent: edge.extend(1.0).to_array(),
                });
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// Tubes and finished faces into one uploadable triangle list.
///
/// The single bake for the whole family: every piece is `members` plus the way
/// each of its faces is finished, so a new shape is a new pair of iterators and
/// no new mesh code. Baked rather than instanced per member because the frame's
/// draw list is one draw per `(mesh, transform)` pair — a per-member draw would
/// put two hundred draw calls behind a single truss.
#[must_use]
fn bake(
    members: impl IntoIterator<Item = Member>,
    faces: impl IntoIterator<Item = (EndFrame, Plating)>,
) -> MeshData {
    let members = members.into_iter();
    let (lower, _) = members.size_hint();
    let mut vertices = Vec::with_capacity(lower * TUBE_VERTICES + 6 * PLATE_VERTICES);
    let mut indices = Vec::with_capacity(lower * TUBE_INDICES + 6 * PLATE_INDICES);
    let faces: Vec<_> = faces.into_iter().collect();
    let bosses = faces
        .iter()
        .filter(|(_, plating)| *plating == Plating::Open)
        .flat_map(|(frame, _)| couplers(*frame));
    for member in members.chain(bosses) {
        push_tube(&mut vertices, &mut indices, member);
    }
    for (frame, _) in &faces {
        push_plate(&mut vertices, &mut indices, *frame);
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// Append one closed capped tube — or cone — along `member`'s axis.
fn push_tube(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, member: Member) {
    let axis = member.end - member.start;
    let length = axis.length();
    if length < f32::EPSILON || member.max_radius() <= 0.0 {
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

    // Sides: two rings sharing radial normals, so the tube shades smooth. A
    // taper tilts that normal off radial by the cone's own slope — without it
    // a coupler boss lights like a cylinder and its step reads as a seam.
    let slope = (member.radii[0] - member.radii[1]) / length;
    let side = vertices.len() as u32;
    for (end, origin, radius) in [
        (0u32, member.start, member.radii[0]),
        (1, member.end, member.radii[1]),
    ] {
        for i in 0..TUBE_SIDES {
            let n = ring(i);
            vertices.push(Vertex {
                position: (origin + n * radius).to_array(),
                normal: (n + along * slope).normalize().to_array(),
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
    for (origin, normal, radius) in [
        (member.start, -along, member.radii[0]),
        (member.end, along, member.radii[1]),
    ] {
        let base = vertices.len() as u32;
        for i in 0..TUBE_SIDES {
            let n = ring(i);
            vertices.push(Vertex {
                position: (origin + n * radius).to_array(),
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
    fn triangle_counts_stay_a_fraction_of_the_ripped_meshes() {
        let counts = [
            ("3 m run", Truss::new(3.0).mesh().indices.len() / 3),
            (
                "six-way",
                Corner::new(FaceSet::ALL).mesh().indices.len() / 3,
            ),
            ("hinge", Hinge::new(45.0).mesh().indices.len() / 3),
        ];
        for (what, triangles) in counts {
            // The ripped 12" block spends 18 144 on itself alone.
            assert!(triangles < 5_000, "{what} spends {triangles}");
            println!("{what}: {triangles} triangles");
        }
    }

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
            // Four chords, an end ring of four at each end, plus two braces
            // per panel on each of four faces.
            let members = truss.members().count();
            assert_eq!(members, 4 + 8 + 8 * panels as usize);
            let mesh = truss.mesh();
            // Plus eight coupler bosses and two plates, four bosses per end.
            assert_eq!(
                mesh.vertices.len(),
                (members + 8) * TUBE_VERTICES + 2 * PLATE_VERTICES
            );
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
        // Nothing stands proud of [`OUTER_M`]: the chords are the section's
        // outermost surface, the plates sit inside them, and a coupler boss is
        // chord gauge at its widest.
        let outer = OUTER_M;
        // The end plate's outer face is the end plane exactly; the outermost
        // brace meets its chord there at an angle, so its end cap leans a few
        // millimetres past. Under a brace radius, invisible against a plate.
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
        // Four chords and two end rings of four come first.
        let braces: Vec<_> = truss.members().skip(12).collect();
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
    fn both_ends_are_plated_and_the_key_is_the_span() {
        let truss = Truss::new(2.0);
        assert_eq!(truss.mesh_key(), "procedural/truss/4");
        // Four bosses and one plate per end, on top of the lattice's tubes.
        assert_eq!(
            truss.mesh().vertices.len(),
            (truss.members().count() + 8) * TUBE_VERTICES + 2 * PLATE_VERTICES
        );
    }

    /// A plate's outer face lies exactly on the plane its end frame names, and
    /// its body is entirely behind it. That is what makes the frame a *mating*
    /// plane: bolt two pieces together and their plates meet rather than
    /// occupy the same millimetre.
    fn assert_plated(mesh: &MeshData, frame: EndFrame) {
        let depth = |v: &Vertex| (Vec3::from(v.position) - frame.position).dot(frame.normal);
        for v in mesh.vertices.iter() {
            // The only thing allowed past the plane is the end cap of the
            // outermost brace, which meets its chord at an angle and leans a
            // few millimetres over. Nothing structural may.
            assert!(
                depth(v) < BRACE_DIAMETER_M / 2.0,
                "{v:?} stands {} m proud of {frame:?}",
                depth(v)
            );
        }
        // Every corner of the plate and every tip of its aperture lies *in*
        // the plane, so a plate that had shrunk, drifted off-axis or been laid
        // a millimetre back fails here rather than in a golden nobody re-reads.
        let right = frame.normal.cross(frame.up);
        let outline = [(1.0f32, 1.0f32), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)]
            .map(|(a, b)| (frame.up * a + right * b) * PLATE_HALF_M)
            .into_iter()
            .chain(
                [(1.0f32, 0.0f32), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)]
                    .map(|(a, b)| (frame.up * a + right * b) * APERTURE_M),
            );
        for point in outline {
            let want = frame.position + point;
            assert!(
                mesh.vertices
                    .iter()
                    .any(|v| (Vec3::from(v.position) - want).length() < 1e-5),
                "{want} is not on the plate of {frame:?}"
            );
        }
    }

    #[test]
    fn every_open_face_carries_a_plate_on_its_own_plane() {
        let truss = Truss::new(1.5);
        for frame in truss.end_frames() {
            assert_plated(&truss.mesh(), frame);
        }
        let corner = Corner::new(FaceSet::ALL);
        for frame in corner.end_frames() {
            assert_plated(&corner.mesh(), frame);
        }
        for degrees in [0.0f32, 45.0, 90.0, 135.0, 180.0] {
            let hinge = Hinge::new(degrees);
            for frame in hinge.end_frames() {
                assert_plated(&hinge.mesh(), frame);
            }
        }
    }

    /// The rim of four chord-gauge tubes standing a chord radius inside
    /// `frame`, mitred so that their ends meet on the chord centre square.
    ///
    /// One shape, three ways of arriving at it: a [`Truss`] and a [`Hinge`]
    /// leaf build theirs with [`end_ring`], a [`Corner`]'s falls out of its
    /// twelve cube edges. If one of the three drifts, ends in the same rig
    /// stop matching each other, which is exactly what this family exists to
    /// prevent.
    fn assert_rimmed(members: &[Member], frame: EndFrame) {
        let radius = CHORD_DIAMETER_M / 2.0;
        let back = frame.normal * radius;
        let depth = |p: Vec3| (p - frame.position).dot(frame.normal);
        let rim: Vec<_> = members
            .iter()
            .filter(|m| {
                (m.max_radius() - radius).abs() < 1e-6
                    && (depth(m.start) + radius).abs() < 1e-4
                    && (depth(m.end) + radius).abs() < 1e-4
            })
            .collect();
        assert_eq!(rim.len(), 4, "{frame:?} is backed by {} tubes", rim.len());
        // Closed at the corners: two of the four reach every chord centre, so
        // the ring mitres instead of leaving a notch. Reach, not endpoint — a
        // [`Corner`]'s edges run past their corners into the next face.
        for corner in chord_centres(frame) {
            let target = corner - back;
            let meeting = rim
                .iter()
                .filter(|m| {
                    let axis = m.end - m.start;
                    let t = (target - m.start).dot(axis) / axis.length_squared();
                    (-1e-3..=1.001).contains(&t) && (m.start + axis * t - target).length() < 1e-4
                })
                .count();
            assert_eq!(meeting, 2, "{corner} of {frame:?} is met by {meeting}");
        }
    }

    #[test]
    fn every_face_is_backed_by_a_rim_on_the_chord_square() {
        let truss = Truss::new(1.5);
        let members: Vec<_> = truss.members().collect();
        for frame in truss.end_frames() {
            assert_rimmed(&members, frame);
        }
        for faces in [FaceSet::THROUGH, FaceSet::of([Face::PosX, Face::PosZ])] {
            let corner = Corner::new(faces);
            let members: Vec<_> = corner.members().collect();
            // Every face, not just the open ones: a corner's rim is its cube
            // edges, and those are there whatever the ways.
            for face in Face::ALL {
                assert_rimmed(&members, face.frame());
            }
        }
        // Short of 180°, where the joint folds back and the two leaves' rims
        // land in one plane — eight tubes there, and both rings correct.
        for degrees in [0.0f32, 45.0, 90.0, 135.0] {
            let hinge = Hinge::new(degrees);
            let members: Vec<_> = hinge.members().collect();
            for frame in hinge.end_frames() {
                assert_rimmed(&members, frame);
            }
        }
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
                .filter(|m| m.max_radius() > BRACE_DIAMETER_M)
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
            // Twelve edges, at most four diagonals, four bosses per way, and
            // six plates whatever the ways.
            assert!(
                mesh.vertices.len() <= (16 + 6 * 4) * TUBE_VERTICES + 6 * PLATE_VERTICES,
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
    fn a_hinge_is_two_leaves_four_knuckles_and_a_pin() {
        let hinge = Hinge::new(90.0);
        // Twelve tubes, four braces and two lugs a leaf, then the pin.
        assert_eq!(hinge.members().count(), 18 + 18 + 1);
        let mesh = hinge.mesh();
        assert_eq!(
            mesh.vertices.len(),
            (37 + 8) * TUBE_VERTICES + 2 * PLATE_VERTICES
        );
        assert!(mesh
            .indices
            .iter()
            .all(|&i| (i as usize) < mesh.vertices.len()));
        for v in mesh.vertices.iter() {
            assert!((Vec3::from(v.normal).length() - 1.0).abs() < 1e-4, "{v:?}");
        }
    }

    /// How much overlap the sampled clearance below is allowed to report.
    ///
    /// Its measure is capsule clearance, and a capsule is fatter at the ends
    /// than the flat-capped tube it stands for — so two chords whose end caps
    /// meet across the knuckle gap read as a hair overlapped when they are
    /// touching. Two millimetres of that is contact; more is a joint drawn
    /// through itself.
    const TOUCH_M: f32 = 0.002;

    /// Closest approach between two tube axes, sampled. Exact enough for a
    /// clearance guard and a great deal shorter than the analytic case split.
    fn clearance(a: Member, b: Member) -> f32 {
        let sample = |m: Member| (0..=24).map(move |i| m.start.lerp(m.end, i as f32 / 24.0));
        let mut worst = f32::MAX;
        for p in sample(a) {
            for q in sample(b) {
                worst = worst.min((p - q).length() - a.max_radius() - b.max_radius());
            }
        }
        worst
    }

    /// The two leaves turn about the pin without ever meeting.
    ///
    /// This is the property a centre pin cannot have: rotating one half-box
    /// about an axis through the middle drives it straight through the other
    /// the moment the angle leaves zero. The knuckle gap plus an edge pin is
    /// what buys it, so if either changes this is what says so.
    #[test]
    fn hinge_leaves_clear_each_other_at_every_angle() {
        let leaf: Vec<_> = leaf_members().collect();
        for degrees in (0..=180).step_by(5) {
            let hinge = Hinge::new(degrees as f32);
            let swing = hinge.swing();
            for fixed in &leaf {
                for other in &leaf {
                    let moved = other.transformed(swing);
                    // Zero is two chords meeting cap to cap across the
                    // knuckle gap at 0°, which is contact, not overlap.
                    assert!(
                        clearance(*fixed, moved) > -TOUCH_M,
                        "{degrees}°: {fixed:?} meets {moved:?}"
                    );
                }
            }
        }
    }

    /// Both leaves stay attached to the pin: a knuckle that only reaches its
    /// axis at zero is a lug drawn floating in space at every other angle.
    #[test]
    fn hinge_knuckles_stay_on_the_pin() {
        for degrees in [0.0f32, 45.0, 90.0, 135.0, 180.0] {
            let hinge = Hinge::new(degrees);
            let axis = Hinge::pin();
            let lugs: Vec<_> = hinge
                .members()
                .filter(|m| (m.max_radius() - PIN_RADIUS_M).abs() < 1e-6)
                .collect();
            // Four lugs and the pin itself.
            assert_eq!(lugs.len(), 5, "{degrees}");
            let on_axis = |p: Vec3| (p.x - axis.x).hypot(p.z - axis.z) < 1e-4;
            for lug in lugs {
                assert!(
                    on_axis(lug.start) || on_axis(lug.end),
                    "{degrees}°: {lug:?} is off the pin"
                );
            }
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
