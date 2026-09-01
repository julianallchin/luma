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
//! # Three ways a piece has geometry
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
//! - **An assembly.** Several GLBs bolted together at fixed relative places
//!   and handled as one piece — the DJ booth is a deck with a mixer and four
//!   players standing on it. Its parts are *placements*, not a scene graph:
//!   see [`Part`] for why the vocabulary stops where it does.
//!
//! The four ripped truss GLBs the palette used to carry are gone from here for
//! that reason (the files stay on disk; render goldens still compare against
//! them). They were imperial products — 1.22 m and 1.83 m spans, a Q30 at
//! 254 mm — and only mated each other by luck of modelling.

use crate::sockets::{BboxAnchor, SocketDef, SocketMode, SocketType};
use glam::{DVec2, DVec3};
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
    DjBooth,
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
            PieceKind::DjBooth => "dj_booth",
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// Several bundled GLBs at fixed relative places, placed and selected as
    /// one. See [`Part`].
    Assembly(&'static [Part]),
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
            Geometry::Assembly(_) => "assembly",
        }
    }

    /// Whether the shape comes from a generator rather than a file — and
    /// therefore whether [`Piece::sockets`] is empty by design.
    pub fn is_procedural(self) -> bool {
        matches!(self, Geometry::Procedural(_))
    }

    /// The parts of an assembly, or an empty slice for a piece that is one
    /// shape.
    #[must_use]
    pub fn parts(self) -> &'static [Part] {
        match self {
            Geometry::Assembly(parts) => parts,
            _ => &[],
        }
    }
}

/// One catalog entry.
pub struct Piece {
    /// Stable identity, and what the database stores. For a GLB piece this is
    /// its mesh path.
    pub id: &'static str,
    /// The short name the authoring surface names this piece by — `"truss"`,
    /// `"guardrail"`, `"deck"`.
    ///
    /// The *primary* id for a person or an agent; [`Self::id`] is the storage
    /// key and reads as an alias. Both resolve through [`find`], so a venue row
    /// written before short names existed still names a piece.
    pub short: &'static str,
    pub kind: PieceKind,
    pub display_name: &'static str,
    pub palette_group: PaletteGroup,
    pub geometry: Geometry,
    /// Authored sockets, in bbox-anchor form. **Empty for procedural pieces**,
    /// whose sockets come from the generator's end frames instead.
    pub sockets: Vec<SocketDef>,
}

// ---------------------------------------------------------------------------
// Assemblies
// ---------------------------------------------------------------------------

/// Which surface a [`Part`] rests its underside on.
///
/// Two values, and deliberately not a parent index: an assembly here is a
/// thing standing on a thing, and a general nesting vocabulary would be a
/// scene graph invented for one piece. The second assembly that genuinely
/// stacks three deep is the one that earns more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rest {
    /// The floor the assembly stands on: the part's bbox bottom at `y = 0`.
    Ground,
    /// The top of the assembly's [`Rest::Ground`] part — the tabletop.
    Deck,
}

/// One GLB inside an [`Geometry::Assembly`], and where it sits.
///
/// **No heights.** A part says which surface it stands on, never how high that
/// surface is: the deck's own measured bbox supplies that, exactly as an
/// authored socket takes its anchor from measurement rather than a transcribed
/// number. Transcribing 1.01 m here would be the same bug as authoring a
/// socket at a literal coordinate — right until somebody re-exports the mesh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Part {
    /// Mesh path relative to the mesh root.
    pub mesh: &'static str,
    /// Quarter-turns about `+Y` applied to the part before placement, for a
    /// part whose mesh is modelled across the axis the assembly runs along.
    pub quarter_turns: i8,
    /// Where the part's own bbox centre lands in plan, relative to the
    /// assembly's centre — `(x, z)` in the assembly's own local space. A
    /// piece-local axis, deliberately not a stage one: an assembly has no
    /// opinion about which way a room faces.
    pub plan: DVec2,
    /// What its underside sits on.
    pub rest: Rest,
}

impl Part {
    const fn new(mesh: &'static str, plan: DVec2, rest: Rest) -> Self {
        Self {
            mesh,
            quarter_turns: 0,
            plan,
            rest,
        }
    }

    const fn turned(mut self, quarter_turns: i8) -> Self {
        self.quarter_turns = quarter_turns;
        self
    }
}

/// Clear air between two adjacent units on the booth's top.
const BOOTH_GAP_M: f64 = 0.04;

/// Measured widths of the units the booth stands up, so their spacing is a
/// sum rather than five transcribed positions kept consistent with each other
/// by hand. Only the pitch along the run is authored — depth and height stay
/// measurements, because only the run has slack to distribute.
const MIXER_W_M: f64 = 0.407;
const CDJ_W_M: f64 = 0.343;

/// Mixer dead centre, a player either side of it, another outboard of each.
/// Computed from the two widths and the gap, so "move the players apart" is
/// one number rather than four positions.
const CDJ_INNER_M: f64 = (MIXER_W_M + CDJ_W_M) / 2.0 + BOOTH_GAP_M;
const CDJ_OUTER_M: f64 = CDJ_INNER_M + CDJ_W_M + BOOTH_GAP_M;

/// The quarter-turn that lays a unit's control face along the booth's run.
///
/// The deck is modelled with its 2 m axis on `Z` and the players with their
/// width on `X`, so one of the two has to turn for a row of players to run the
/// length of the deck. Turning the *players* rather than the deck is what
/// keeps the booth landing in a room exactly as the bare deck it is made of
/// lands — the assembly introduces no orientation of its own.
const BOOTH_UNIT_TURN: i8 = 1;

/// The DJ booth: a 2 x 1 m deck — already a metre tall as modelled, which is
/// standing height — wearing a mixer and four players.
///
/// Authored centred on the origin in plan, so the piece's footprint centre
/// *is* its origin and its footing socket is symmetric: the booth is
/// indifferent to which way a room calls upstage.
static DJ_BOOTH_PARTS: &[Part] = &[
    Part::new(DECK_2X1, DVec2::ZERO, Rest::Ground),
    Part::new(MIXER, DVec2::ZERO, Rest::Deck).turned(BOOTH_UNIT_TURN),
    Part::new(CDJ, DVec2::new(0.0, -CDJ_INNER_M), Rest::Deck).turned(BOOTH_UNIT_TURN),
    Part::new(CDJ, DVec2::new(0.0, CDJ_INNER_M), Rest::Deck).turned(BOOTH_UNIT_TURN),
    Part::new(CDJ, DVec2::new(0.0, -CDJ_OUTER_M), Rest::Deck).turned(BOOTH_UNIT_TURN),
    Part::new(CDJ, DVec2::new(0.0, CDJ_OUTER_M), Rest::Deck).turned(BOOTH_UNIT_TURN),
];

const DECK_2X1: &str = "stage_lab/stage_praticavel_2x1x1.glb";
const CDJ: &str = "stage_lab/cdj_3000x.glb";
const MIXER: &str = "stage_lab/mixer_djm_a9.glb";

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
            // A rail stands up: chaining by either end keeps its feet down.
            // The Face flip turned a same-name end mate upside down.
            .mode(SocketMode::Upright)
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
            // Same physics as a rail end: the cover lies on the floor and a
            // same-name chain must not flip it over.
            .mode(SocketMode::Upright)
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
                short: &'static str,
                kind: PieceKind,
                display_name: &'static str,
                palette_group: PaletteGroup,
                sockets: Vec<SocketDef>| Piece {
        id,
        short,
        kind,
        display_name,
        palette_group,
        geometry: Geometry::Mesh { path: id },
        sockets,
    };
    let speaker = |id, short, name| {
        mesh(
            id,
            short,
            Speaker,
            name,
            Speakers,
            mount_sockets("mount", SocketType::SpeakerMount),
        )
    };
    let equipment = |id, short, kind, name| {
        mesh(
            id,
            short,
            kind,
            name,
            Equipment,
            mount_sockets("mount", SocketType::EquipmentMount),
        )
    };
    let procedural = |id, short, display_name, family| Piece {
        id,
        short,
        kind: Truss,
        display_name,
        palette_group: Trusses,
        geometry: Geometry::Procedural(family),
        sockets: Vec::new(),
    };

    vec![
        mesh(
            "stage_lab/stage_praticavel_1x1.glb",
            "deck_1x1",
            Floor,
            "Stage Deck 1×1m",
            Stage,
            floor_sockets(),
        ),
        // Same socket topology as the 1×1; the bbox does the size scaling.
        mesh(
            DECK_2X1,
            "deck",
            Floor,
            "Stage Deck 2×1m",
            Stage,
            floor_sockets(),
        ),
        procedural("truss/straight", "truss", "Truss · straight", Family::Truss),
        procedural(
            "truss/corner",
            "corner",
            "Truss · corner box",
            Family::Corner,
        ),
        procedural("truss/hinge", "hinge", "Truss · hinge", Family::Hinge),
        speaker("stage_lab/speaker_dbr15.glb", "dbr15", "Yamaha DBR15"),
        speaker("stage_lab/speaker_dual18sub.glb", "sub18", "Dual 18\" Sub"),
        speaker(
            "stage_lab/speaker_event_212a.glb",
            "event212a",
            "Event 212A",
        ),
        speaker(
            "stage_lab/speaker_jbl_vtx_v20.glb",
            "vtx_v20",
            "JBL VTX V20",
        ),
        mesh(
            "stage_lab/speaker_stand.glb",
            "speaker_stand",
            Stand,
            "Speaker Stand",
            Accessories,
            stand_sockets(),
        ),
        equipment(CDJ, "cdj", Cdj, "CDJ-3000"),
        equipment(MIXER, "mixer", Mixer, "DJM-A9 Mixer"),
        Piece {
            id: "assembly/dj_booth",
            short: "dj_booth",
            kind: DjBooth,
            display_name: "DJ Booth",
            palette_group: Equipment,
            geometry: Geometry::Assembly(DJ_BOOTH_PARTS),
            // One held socket and nothing hosted: the booth already carries
            // its own gear, and its top is a surface a *player* stands on, not
            // one the palette should let a second mixer onto.
            sockets: mount_sockets("bottom", SocketType::BottomMount),
        },
        mesh(
            "stage_lab/guardrail.glb",
            "guardrail",
            Guardrail,
            "Guardrail",
            Accessories,
            guardrail_sockets(),
        ),
        mesh(
            "stage_lab/cable_cover.glb",
            "cable_cover",
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

/// The piece a *caller* named: its short name, its stored id, or its display
/// name, case-insensitively.
///
/// One entry point for every surface a person or an agent types a piece into,
/// so `"truss"`, `"truss/straight"` and `"Truss · straight"` cannot come to mean
/// three different things. [`piece`] stays the exact-id lookup the loader and
/// the renderer use — they hold a stored key, not a name.
#[must_use]
pub fn find(name: &str) -> Option<&'static Piece> {
    let wanted = name.trim();
    pieces()
        .iter()
        .find(|p| p.short == wanted || p.id == wanted)
        .or_else(|| {
            let lower = wanted.to_lowercase();
            pieces().iter().find(|p| {
                p.short.eq_ignore_ascii_case(&lower)
                    || p.id.eq_ignore_ascii_case(&lower)
                    || p.display_name.to_lowercase() == lower
            })
        })
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

    /// Short names are the primary vocabulary, so they have to be as unique as
    /// the storage ids and they have to resolve back to the same piece.
    #[test]
    fn short_names_are_unique_and_resolve() {
        let shorts: HashSet<&str> = pieces().iter().map(|p| p.short).collect();
        assert_eq!(shorts.len(), pieces().len());
        for p in pieces() {
            assert_eq!(find(p.short).map(|f| f.id), Some(p.id));
            assert_eq!(find(p.id).map(|f| f.id), Some(p.id));
            assert_eq!(find(p.display_name).map(|f| f.id), Some(p.id));
        }
        assert!(find("no such piece").is_none());
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

    /// An assembly stands on exactly one part, and every other part stands on
    /// that one. Two ground parts would make "the deck height" ambiguous, and
    /// none would make it nothing.
    #[test]
    fn an_assembly_has_exactly_one_ground_part() {
        for p in pieces() {
            let parts = p.geometry.parts();
            if parts.is_empty() {
                continue;
            }
            let ground = parts.iter().filter(|p| p.rest == Rest::Ground).count();
            assert_eq!(ground, 1, "{}: wants exactly one ground part", p.id);
        }
    }

    /// A part names a mesh the catalog also ships as a piece of its own, so a
    /// booth cannot drift onto a GLB nothing else in the palette uses.
    #[test]
    fn assembly_parts_name_catalog_meshes() {
        let meshes: HashSet<&str> = pieces()
            .iter()
            .filter_map(|p| match p.geometry {
                Geometry::Mesh { path } => Some(path),
                _ => None,
            })
            .collect();
        for p in pieces() {
            for part in p.geometry.parts() {
                assert!(
                    meshes.contains(part.mesh),
                    "{}: {} is not a catalog mesh",
                    p.id,
                    part.mesh
                );
            }
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
