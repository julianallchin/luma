//! The stage catalog: every placeable piece, and where its sockets are.
//!
//! This is the **single copy**. It used to exist twice — hand-authored in
//! `src/features/stage/lib/stage-meshes.ts` and absent on the Rust side, which
//! is why `gpui/crates/scene` could port the snap *algorithms* but not run them
//! against a real piece. The TypeScript side now reads
//! `src/features/stage/lib/catalog.generated.ts`, emitted from this module by
//! `tests/binding.rs`; editing that file by hand is a merge conflict waiting to
//! happen.
//!
//! # Two ways a piece has geometry
//!
//! - **A GLB.** Decks, speakers, CDJs, rails, covers — real products, ripped
//!   from real models. Their sockets are *authored*, against the mesh's
//!   measured bounding box (see [`crate::sockets`]), so they survive whatever
//!   pivot the modeller chose.
//! - **A generator.** The truss family (`luma_render::truss`) is parametric,
//!   and every one of its open faces is already a full frame. Authoring
//!   sockets for it would be transcribing geometry the generator knows, so
//!   procedural pieces carry **no** [`SocketDef`]s: `luma_render::catalog`
//!   turns their end frames into sockets directly.
//!
//! The four ripped truss GLBs the palette used to carry are gone from here for
//! that reason (the files stay on disk; render goldens still compare against
//! them). They were imperial products — 1.22 m and 1.83 m spans, a Q30 at
//! 254 mm — and only mated each other by luck of modelling.

use crate::sockets::{BboxAnchor, SocketDef, SocketMode, SocketType};
use glam::DVec3;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Authoring constants
// ---------------------------------------------------------------------------

/// How far inward from a stage corner the corner socket sits, so a truss of
/// finite section visually rests *at* the corner rather than half over the
/// edge.
pub const TRUSS_INSET_M: f64 = 0.15;

/// The speaker stand's pole axis sits ~10 cm off its bbox centre in X: the
/// three tripod feet are not placed symmetrically about the bbox, so the
/// extreme on +X is one leg and on -X another. Anchors shift onto the actual
/// pole, so a mounted speaker sits centred on it and the base snaps where the
/// pole meets the floor.
pub const SPEAKER_STAND_POLE_OFFSET: DVec3 = DVec3::new(0.1, 0.0, 0.0);

/// Tiny inset along a cable cover's own axis, so two covers butted end to end
/// do not z-fight.
pub const CABLE_COVER_END_INSET_M: f64 = 0.005;

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// What a piece *is*, for the handful of behaviours that branch on it — the
/// surface raycast only accepts a hit on a [`PieceKind::Floor`], for one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PieceKind {
    Floor,
    Truss,
    Speaker,
    Cdj,
    Mixer,
    Guardrail,
    Stand,
    CableCover,
}

impl PieceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PieceKind::Floor => "floor",
            PieceKind::Truss => "truss",
            PieceKind::Speaker => "speaker",
            PieceKind::Cdj => "cdj",
            PieceKind::Mixer => "mixer",
            PieceKind::Guardrail => "guardrail",
            PieceKind::Stand => "stand",
            PieceKind::CableCover => "cable_cover",
        }
    }
}

/// Palette section. The order of [`PaletteGroup::ALL`] is the order the
/// builder's palette draws them in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PaletteGroup {
    Stage,
    Trusses,
    Speakers,
    Equipment,
    Accessories,
}

impl PaletteGroup {
    pub const ALL: [PaletteGroup; 5] = [
        PaletteGroup::Stage,
        PaletteGroup::Trusses,
        PaletteGroup::Speakers,
        PaletteGroup::Equipment,
        PaletteGroup::Accessories,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PaletteGroup::Stage => "Stage",
            PaletteGroup::Trusses => "Trusses",
            PaletteGroup::Speakers => "Speakers",
            PaletteGroup::Equipment => "Equipment",
            PaletteGroup::Accessories => "Accessories",
        }
    }
}

/// Where a piece's shape comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Geometry {
    /// A bundled GLB, at `path` relative to the mesh root. Always equal to the
    /// piece's [`Piece::id`] — see `mesh_id_is_its_path`.
    Mesh { path: &'static str },
    /// A generated family. **No parameters here**: the span of a truss, the
    /// open faces of a corner and the angle of a hinge are the generator's
    /// vocabulary (`luma_render::scene_desc::Procedural`), quantized by its
    /// constructors, and a copy of them in the palette would be a second thing
    /// to keep in step. The catalog names the family; `luma_render::catalog`
    /// supplies the palette default and the geometry.
    Procedural(Family),
}

/// The generated piece families. Every one is truss today, and every one mates
/// with every other — see `luma_render::truss`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// A continuous lattice of a given span.
    Truss,
    /// A box open on two to six faces.
    Corner,
    /// Two half-boxes on a pin, deflecting by an angle.
    Hinge,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Truss => "truss",
            Family::Corner => "corner",
            Family::Hinge => "hinge",
        }
    }
}

impl Geometry {
    /// The tag the generated TypeScript discriminates on.
    pub fn tag(self) -> &'static str {
        match self {
            Geometry::Mesh { .. } => "mesh",
            Geometry::Procedural(_) => "procedural",
        }
    }

    /// Whether the shape comes from a generator rather than a file — and
    /// therefore whether [`Piece::sockets`] is empty by design.
    pub fn is_procedural(self) -> bool {
        matches!(self, Geometry::Procedural(_))
    }
}

/// One catalog entry.
pub struct Piece {
    /// Stable identity, and what the database stores. For a GLB piece this is
    /// its mesh path.
    pub id: &'static str,
    pub kind: PieceKind,
    pub display_name: &'static str,
    pub palette_group: PaletteGroup,
    pub geometry: Geometry,
    /// Authored sockets, in bbox-anchor form. **Empty for procedural pieces**,
    /// whose sockets come from the generator's end frames instead.
    pub sockets: Vec<SocketDef>,
}

// ---------------------------------------------------------------------------
// Socket sets
// ---------------------------------------------------------------------------

fn grab() -> SocketDef {
    SocketDef::new("grab", SocketType::Grab, BboxAnchor::Center)
}

/// A deck: a bottom mount so it rests on the ground rather than half-buried, a
/// top surface things stand on, four top edges that butt against neighbouring
/// decks, and four inset top corners for trusses to stand on.
///
/// `top` is a **frame**, not a spot. A piece placed on it carries `(u, v)`
/// across the deck (`luma_scene::venue`), so the discrete socket and the
/// cursor's actual hit point are the same answer expressed twice — which is
/// what lets a CDJ on a riser be a *relation* and move when the riser does.
/// Before the venue graph the two fought, and the socket was left out.
fn floor_sockets() -> Vec<SocketDef> {
    let edge = |name, anchor, tangent| {
        SocketDef::new(name, SocketType::FloorEdge, anchor)
            .tangent(tangent)
            // Edge mode keeps the held deck upright when it snaps to an
            // adjacent one: only the in-edge tangent flips, not the up axis.
            .mode(SocketMode::Edge)
    };
    // Corner normals are +Y so a truss stands vertically; corner anchors have
    // no natural direction, so the normal has to be explicit.
    let corner = |name, anchor, offset| {
        SocketDef::new(name, SocketType::FloorCorner, anchor)
            .offset(offset)
            .normal(DVec3::Y)
    };
    let i = TRUSS_INSET_M;
    vec![
        grab(),
        SocketDef::new("bottom", SocketType::BottomMount, BboxAnchor::Bottom).normal(DVec3::NEG_Y),
        SocketDef::new("top", SocketType::FloorTop, BboxAnchor::Top).normal(DVec3::Y),
        edge("edge_front", BboxAnchor::TopFront, DVec3::X),
        edge("edge_back", BboxAnchor::TopBack, DVec3::X),
        edge("edge_left", BboxAnchor::TopLeft, DVec3::Z),
        edge("edge_right", BboxAnchor::TopRight, DVec3::Z),
        corner(
            "corner_fl",
            BboxAnchor::TopFrontLeft,
            DVec3::new(i, 0.0, -i),
        ),
        corner(
            "corner_fr",
            BboxAnchor::TopFrontRight,
            DVec3::new(-i, 0.0, -i),
        ),
        corner("corner_bl", BboxAnchor::TopBackLeft, DVec3::new(i, 0.0, i)),
        corner(
            "corner_br",
            BboxAnchor::TopBackRight,
            DVec3::new(-i, 0.0, i),
        ),
    ]
}

/// Anything whose whole attachment story is "it sits on a flat thing".
fn mount_sockets(name: &str, socket_type: SocketType) -> Vec<SocketDef> {
    vec![
        grab(),
        SocketDef::new(name, socket_type, BboxAnchor::Bottom).normal(DVec3::NEG_Y),
    ]
}

fn stand_sockets() -> Vec<SocketDef> {
    let pole = SPEAKER_STAND_POLE_OFFSET;
    vec![
        grab().offset(pole),
        SocketDef::new("top", SocketType::StandTop, BboxAnchor::Top)
            .offset(pole)
            .normal(DVec3::Y),
        SocketDef::new("base", SocketType::StandBottom, BboxAnchor::Bottom)
            .offset(pole)
            .normal(DVec3::NEG_Y),
    ]
}

fn guardrail_sockets() -> Vec<SocketDef> {
    let end = |name, anchor, x: f64, normal| {
        SocketDef::new(name, SocketType::RailEnd, anchor)
            .offset(DVec3::new(x, 0.0, 0.0))
            .normal(normal)
            .tangent(DVec3::Z)
    };
    vec![
        grab(),
        SocketDef::new("bottom", SocketType::BottomMount, BboxAnchor::Bottom).normal(DVec3::NEG_Y),
        end("end_a", BboxAnchor::Left, 0.012, DVec3::NEG_X),
        end("end_b", BboxAnchor::Right, -0.012, DVec3::X),
    ]
}

/// A cable cover lies flat with its long axis along Z; its two short faces
/// chain to other covers.
fn cable_cover_sockets() -> Vec<SocketDef> {
    let end = |name, anchor, z: f64, normal| {
        SocketDef::new(name, SocketType::CableEnd, anchor)
            .offset(DVec3::new(0.0, 0.0, z))
            .normal(normal)
            .tangent(DVec3::X)
    };
    let inset = CABLE_COVER_END_INSET_M;
    vec![
        grab(),
        SocketDef::new("mount", SocketType::EquipmentMount, BboxAnchor::Bottom)
            .normal(DVec3::NEG_Y),
        end("end_front", BboxAnchor::Front, -inset, DVec3::Z),
        end("end_back", BboxAnchor::Back, inset, DVec3::NEG_Z),
    ]
}

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

fn build() -> Vec<Piece> {
    use PaletteGroup::*;
    use PieceKind::*;

    let mesh = |id: &'static str,
                kind: PieceKind,
                display_name: &'static str,
                palette_group: PaletteGroup,
                sockets: Vec<SocketDef>| Piece {
        id,
        kind,
        display_name,
        palette_group,
        geometry: Geometry::Mesh { path: id },
        sockets,
    };
    let speaker = |id, name| {
        mesh(
            id,
            Speaker,
            name,
            Speakers,
            mount_sockets("mount", SocketType::SpeakerMount),
        )
    };
    let equipment = |id, kind, name| {
        mesh(
            id,
            kind,
            name,
            Equipment,
            mount_sockets("mount", SocketType::EquipmentMount),
        )
    };
    let procedural = |id, display_name, family| Piece {
        id,
        kind: Truss,
        display_name,
        palette_group: Trusses,
        geometry: Geometry::Procedural(family),
        sockets: Vec::new(),
    };

    vec![
        mesh(
            "stage_lab/stage_praticavel_1x1.glb",
            Floor,
            "Stage Deck 1×1m",
            Stage,
            floor_sockets(),
        ),
        // Same socket topology as the 1×1; the bbox does the size scaling.
        mesh(
            "stage_lab/stage_praticavel_2x1x1.glb",
            Floor,
            "Stage Deck 2×1m",
            Stage,
            floor_sockets(),
        ),
        procedural("truss/straight", "Truss · straight", Family::Truss),
        procedural("truss/corner", "Truss · corner box", Family::Corner),
        procedural("truss/hinge", "Truss · hinge", Family::Hinge),
        speaker("stage_lab/speaker_dbr15.glb", "Yamaha DBR15"),
        speaker("stage_lab/speaker_dual18sub.glb", "Dual 18\" Sub"),
        speaker("stage_lab/speaker_event_212a.glb", "Event 212A"),
        speaker("stage_lab/speaker_jbl_vtx_v20.glb", "JBL VTX V20"),
        mesh(
            "stage_lab/speaker_stand.glb",
            Stand,
            "Speaker Stand",
            Accessories,
            stand_sockets(),
        ),
        equipment("stage_lab/cdj_3000x.glb", Cdj, "CDJ-3000"),
        equipment("stage_lab/mixer_djm_a9.glb", Mixer, "DJM-A9 Mixer"),
        mesh(
            "stage_lab/guardrail.glb",
            Guardrail,
            "Guardrail",
            Accessories,
            guardrail_sockets(),
        ),
        mesh(
            "stage_lab/cable_cover.glb",
            CableCover,
            "Cable Cover",
            Accessories,
            cable_cover_sockets(),
        ),
    ]
}

static CATALOG: LazyLock<Vec<Piece>> = LazyLock::new(build);

/// Every piece, in palette order within each group.
pub fn pieces() -> &'static [Piece] {
    &CATALOG
}

/// The piece with this id, or `None` if the id is unknown — a venue may still
/// hold rows naming a piece the catalog has dropped.
pub fn piece(id: &str) -> Option<&'static Piece> {
    pieces().iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique() {
        let ids: HashSet<&str> = pieces().iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), pieces().len());
    }

    #[test]
    fn mesh_id_is_its_path() {
        for p in pieces() {
            if let Geometry::Mesh { path } = p.geometry {
                assert_eq!(p.id, path, "{}: id and mesh path disagree", p.id);
            }
        }
    }

    /// Procedural pieces must not carry authored sockets, and mesh pieces must
    /// carry some — including exactly one grab, which the solver needs as the
    /// cursor's reference.
    #[test]
    fn socket_authoring_matches_geometry() {
        for p in pieces() {
            if p.geometry.is_procedural() {
                assert!(p.sockets.is_empty(), "{}: procedural, yet authored", p.id);
                continue;
            }
            let grabs = p
                .sockets
                .iter()
                .filter(|s| s.socket_type == SocketType::Grab)
                .count();
            assert_eq!(grabs, 1, "{}: wants exactly one grab socket", p.id);
            let names: HashSet<&str> = p.sockets.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names.len(), p.sockets.len(), "{}: duplicate socket", p.id);
        }
    }

    /// Every mesh piece has at least one socket that can actually be *held*,
    /// or it can never be placed against anything.
    #[test]
    fn every_mesh_piece_can_be_placed() {
        for p in pieces() {
            if p.geometry.is_procedural() {
                continue;
            }
            assert!(
                p.sockets
                    .iter()
                    .any(|s| s.socket_type.polarity().can_be_held()),
                "{}: no held-side socket",
                p.id
            );
        }
    }
}
