//! Geometry for the stage catalog: bounding boxes for the GLB pieces, end
//! frames for the generated ones, and the resolved sockets both produce.
//!
//! [`mod@luma_scene::catalog`] is the catalog — what pieces exist, what they are
//! called, where their authored sockets sit. It cannot resolve any of that on
//! its own: an authored socket is a bbox anchor and the bbox is in a file, and
//! a generated piece has no authored sockets at all. This module is where the
//! two meet, and it is the only place that knows both.
//!
//! Everything here is in the socket layer's frame — glTF Y-up, piece-local —
//! which is also `crate::truss`'s local space, so an end frame is already a
//! socket frame and needs no conversion.

use crate::assets::Library;
use crate::luminaire::{is_procedural, model_kind, ModelKind};
use crate::scene_desc::{Definition, Procedural};
use crate::truss::{Face, FaceSet};
use glam::{DVec3, Mat4, Vec3};
use luma_scene::aabb::DAabb;
use luma_scene::catalog::{pieces, Family, Geometry, Part, Piece, Rest};
use luma_scene::snap::SocketLookup;
use luma_scene::sockets::{resolve_socket, ResolvedSocket, SocketType};
use luma_scene::venue::{Node, NodeKind, NodeSockets, Params};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Palette default span for the straight truss, in metres. Quantized to whole
/// panels by [`crate::truss::Truss::new`].
pub const DEFAULT_TRUSS_SPAN_M: f32 = 3.0;

/// Palette default deflection for a hinge, in degrees.
pub const DEFAULT_HINGE_ANGLE_DEG: f32 = 90.0;

/// Palette default open faces for a corner block: an L, which is the block a
/// rig actually turns a corner with. Every other way count is the same entry
/// with more faces opened.
pub const DEFAULT_CORNER_FACES: [Face; 2] = [Face::NegX, Face::NegZ];

/// The parameters a family starts at when dragged out of the palette. A placed
/// node overrides them (`venue_node_params`, phase 3).
#[must_use]
pub fn default_params(family: Family) -> Procedural {
    match family {
        Family::Truss => Procedural::Truss {
            span: DEFAULT_TRUSS_SPAN_M,
        },
        Family::Corner => Procedural::Corner {
            faces: FaceSet::of(DEFAULT_CORNER_FACES),
        },
        Family::Hinge => Procedural::Hinge {
            angle: DEFAULT_HINGE_ANGLE_DEG,
        },
    }
}

/// The sockets of a generated piece: one per open face, plus the grab the
/// cursor follows.
///
/// Face names come from the generator's own vocabulary (`-x`, `+y`, …) rather
/// than an index, so a corner that gains a way keeps the names of the ways it
/// already had — a venue row naming `face_-x` must not start meaning a
/// different face because a sibling was opened.
#[must_use]
pub fn procedural_sockets(params: Procedural) -> Vec<ResolvedSocket> {
    // Name and frame together: which faces a family offers and where they are
    // is one decision per face, and splitting it into two parallel lists is
    // how the two came to disagree.
    let ends: Vec<(String, crate::truss::EndFrame)> = match params {
        // **Every plate, not only the ways.** All six faces of a block are
        // plated (`Corner::plates`) — an open one merely adds a coupler — and a
        // plate is exactly what a stick bolts to. Naming only the ways left a
        // corner with one free socket the moment it was itself mounted, so
        // "put the next run on *that* face" had no face to point at.
        Procedural::Corner { .. } => Face::ALL
            .into_iter()
            .map(|face| (face_socket_name(face), face.frame()))
            .collect(),
        // The two ends of a stick, named as the ripped truss GLBs named them,
        // so a venue built before the generator landed still reads.
        Procedural::Truss { .. } | Procedural::Hinge { .. } => {
            let names: Vec<String> = if matches!(params, Procedural::Truss { .. }) {
                vec!["end_a".into(), "end_b".into()]
            } else {
                vec!["leaf_fixed".into(), "leaf_swinging".into()]
            };
            let frames = params.end_frames();
            debug_assert_eq!(
                names.len(),
                frames.len(),
                "socket names and end frames must agree; both walk the generator's face order"
            );
            names.into_iter().zip(frames).collect()
        }
    };

    let grab =
        ResolvedSocket::from_frame("grab", SocketType::Grab, DVec3::ZERO, DVec3::Y, DVec3::X);
    std::iter::once(grab)
        .chain(ends.into_iter().map(|(name, f)| {
            ResolvedSocket::from_frame(
                &name,
                SocketType::TrussEnd,
                f.position.as_dvec3(),
                f.normal.as_dvec3(),
                f.up.as_dvec3(),
            )
        }))
        .chain(mount_faces(params))
        .chain(footings(params))
        .collect()
}

/// A generated piece's local bounds — the procedural twin of the measurement
/// [`CatalogSockets::load`] takes off a GLB, in the same authored local space
/// (Y-up, a truss's span along X, origin at the centre).
#[must_use]
pub fn procedural_bounds(params: Procedural) -> DAabb {
    use crate::truss::{Corner, Truss, OUTER_M};
    use glam::DVec3;
    let outer = f64::from(OUTER_M);
    match params {
        Procedural::Truss { span } => {
            let half = f64::from(Truss::new(span).span_m()) / 2.0;
            DAabb::new(
                DVec3::new(-half, -outer, -outer),
                DVec3::new(half, outer, outer),
            )
        }
        Procedural::Corner { faces } => {
            let _ = Corner::new(faces);
            DAabb::new(DVec3::splat(-outer), DVec3::splat(outer))
        }
        Procedural::Hinge { angle } => {
            // The fixed leaf's box, unioned with the swinging leaf's box
            // carried through the same turn the mesh applies (`Hinge::turn`
            // about the pin at the `-Z` edge). The angle changes the
            // envelope, so the envelope is computed from it — the old
            // fixed-size box was wrong at every angle but 90°.
            let angle = f64::from(crate::truss::Hinge::new(angle).angle_deg()).to_radians();
            let pin = glam::DVec3::new(0.0, 0.0, -outer);
            let turn = glam::DMat4::from_translation(pin)
                * glam::DMat4::from_rotation_y(angle)
                * glam::DMat4::from_translation(-pin);
            let mut lo = DVec3::new(-outer, -outer, -outer);
            let mut hi = DVec3::new(0.0, outer, outer);
            for x in [0.0, outer] {
                for y in [-outer, outer] {
                    for z in [-outer, outer] {
                        let p = turn.transform_point3(glam::DVec3::new(x, y, z));
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
            }
            DAabb::new(lo, hi)
        }
    }
}

/// The name of the socket a generated piece lies down on.
pub const SEAT_SOCKET: &str = "seat";

/// The name of the socket a stick stands up on.
pub const BASE_SOCKET: &str = "base";

/// The two ways a generated piece can be **put down** rather than bolted on.
///
/// A truss end is `Neutral` and lives in the `TrussEnd` joint; the floor is
/// `Ground` and lives in the `Surface` joint, so no socket the generator
/// already had could ever meet a floor. Without these a stick could not stand
/// in a room at all — only hang off something that was already there — and
/// "place a truss on the floor" would have no spelling.
///
/// - **`seat`** is the underside centre: the piece lies flat, resting on the
///   section's own outer face. Every family has one, at the same place, because
///   every family's section is that same box.
/// - **`base`** is the upstream end taken as a foot, which is what makes a
///   **tower**: the same generator, stood on end. Only the straight family has
///   one — a block or a hinge stood on a way is a piece bolted to something,
///   not a piece put down.
fn footings(params: Procedural) -> Vec<ResolvedSocket> {
    let seat = ResolvedSocket::from_frame(
        SEAT_SOCKET,
        SocketType::BottomMount,
        DVec3::new(0.0, f64::from(-crate::truss::OUTER_M), 0.0),
        DVec3::NEG_Y,
        DVec3::X,
    );
    let Procedural::Truss { .. } = params else {
        return vec![seat];
    };
    let foot = params.end_frames()[0];
    vec![
        seat,
        ResolvedSocket::from_frame(
            BASE_SOCKET,
            SocketType::BottomMount,
            foot.position.as_dvec3(),
            foot.normal.as_dvec3(),
            DVec3::Y,
        ),
    ]
}

/// The four long sides of a stick, as surfaces a clamp can go on.
///
/// A truss's *ends* bolt structure together; its *sides* are what a rig hangs
/// off, and until these existed there was no host socket on a truss a fixture
/// could mate at all. Only the straight family has them: a corner block's faces
/// are its ways, and a hinge's are its leaves.
///
/// Each face's **tangent is the span axis**, which is the whole reason the
/// distribution vocabulary needs no per-piece rule: `u` on a truss face is
/// metres along the run, measured from its middle, for a stick of one panel or
/// twelve. The normal points out of the face, and beam is the mount normal, so
/// naming `face_-y` is the whole of "hang them underneath, pointing down".
fn mount_faces(params: Procedural) -> Vec<ResolvedSocket> {
    let Procedural::Truss { .. } = params else {
        return Vec::new();
    };
    [Face::NegY, Face::PosY, Face::NegZ, Face::PosZ]
        .into_iter()
        .map(|face| {
            ResolvedSocket::from_frame(
                &face_socket_name(face),
                SocketType::TrussFace,
                (face.normal() * crate::truss::OUTER_M).as_dvec3(),
                face.normal().as_dvec3(),
                DVec3::X,
            )
        })
        .collect()
}

/// The socket name for one face of a generated piece. One spelling, used by the
/// corner's ways and the stick's mount faces alike, so a venue row naming
/// `face_-z` means the same side whichever family it is on.
#[must_use]
pub fn face_socket_name(face: Face) -> String {
    format!("face_{}", face.as_str())
}

// ---------------------------------------------------------------------------
// Assemblies
// ---------------------------------------------------------------------------

/// One part of an assembly, resolved: which GLB, and where it stands in the
/// assembly's own local space.
#[derive(Clone, Debug)]
pub struct Placement {
    /// Mesh path under the mesh root.
    pub mesh: &'static str,
    /// Part-local to assembly-local.
    pub transform: Mat4,
}

/// Lay an assembly out against the GLBs its parts name.
///
/// The **one** place the layout rule lives, so a booth's draws, its collision
/// box, its selection cage and its palette thumbnail cannot disagree about
/// where a player sits. Each part is turned, then slid until its bbox centre
/// lands on its authored plan offset and its underside lands on the surface it
/// rests on — the ground at `y = 0`, or the measured top of the ground part.
///
/// # Errors
/// Propagates the asset library's failure to load a part's GLB.
///
/// # Panics
/// Debug-only, on an assembly with no [`Rest::Ground`] part: the deck height
/// every other part stands on would have nothing to come from.
pub fn assembly_placements(
    parts: &[Part],
    library: &mut Library,
) -> anyhow::Result<Vec<Placement>> {
    // Measured, turned bounds per part, in the order the parts are authored.
    let mut turned = Vec::with_capacity(parts.len());
    for part in parts {
        let (lo, hi) = library.get(part.mesh)?.bounds();
        turned.push(turn_bounds(lo, hi, part.quarter_turns));
    }
    let deck_top = parts
        .iter()
        .zip(&turned)
        .find(|(p, _)| p.rest == Rest::Ground)
        .map_or(0.0, |(_, (lo, hi))| hi.y - lo.y);
    debug_assert!(
        parts.iter().any(|p| p.rest == Rest::Ground),
        "an assembly needs a part on the ground for the rest to stand on"
    );

    Ok(parts
        .iter()
        .zip(&turned)
        .map(|(part, (lo, hi))| {
            let base = match part.rest {
                Rest::Ground => 0.0,
                Rest::Deck => deck_top,
            };
            let centre = (*lo + *hi) * 0.5;
            let slide = Vec3::new(
                part.plan.x as f32 - centre.x,
                base - lo.y,
                part.plan.y as f32 - centre.z,
            );
            Placement {
                mesh: part.mesh,
                transform: Mat4::from_translation(slide)
                    * Mat4::from_rotation_y(quarter_turn_radians(part.quarter_turns)),
            }
        })
        .collect())
}

/// An assembly's own local bounds: the union of its laid-out parts, which is
/// the box the operator sees and therefore the box it collides by. Measuring
/// only the deck was the old floating-gear bug in the other direction.
///
/// # Errors
/// As [`assembly_placements`].
pub fn assembly_bounds(parts: &[Part], library: &mut Library) -> anyhow::Result<DAabb> {
    let placements = assembly_placements(parts, library)?;
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for (part, placement) in parts.iter().zip(&placements) {
        let (plo, phi) = library.get(part.mesh)?.bounds();
        for corner in corners(plo, phi) {
            let p = placement.transform.transform_point3(corner);
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    Ok(DAabb::new(lo.as_dvec3(), hi.as_dvec3()))
}

/// How far an assembly reaches from its own origin, from the authored plan
/// offsets alone.
///
/// Deliberately not the measured union: [`crate::scene_desc::Piece`] cannot
/// open a GLB, which is the same reason a lone mesh falls back to its scale
/// there. Half a metre stands in for the outermost part's own half-width, so
/// the answer errs wide — a camera that frames a little too much beats one
/// that cuts the booth in half.
#[must_use]
pub fn assembly_half_extent(parts: &[Part]) -> f32 {
    const PART_REACH_M: f32 = 0.5;
    parts
        .iter()
        .map(|p| p.plan.abs().max_element() as f32)
        .fold(0.0_f32, f32::max)
        + PART_REACH_M
}

fn quarter_turn_radians(quarter_turns: i8) -> f32 {
    f32::from(quarter_turns) * std::f32::consts::FRAC_PI_2
}

/// A local bbox after `quarter_turns` about `+Y`. Quarter turns keep a box
/// axis-aligned, so this stays a box rather than widening to a hull.
fn turn_bounds(lo: Vec3, hi: Vec3, quarter_turns: i8) -> (Vec3, Vec3) {
    let turn = Mat4::from_rotation_y(quarter_turn_radians(quarter_turns));
    let mut out_lo = Vec3::splat(f32::INFINITY);
    let mut out_hi = Vec3::splat(f32::NEG_INFINITY);
    for corner in corners(lo, hi) {
        let p = turn.transform_point3(corner);
        out_lo = out_lo.min(p);
        out_hi = out_hi.max(p);
    }
    (out_lo, out_hi)
}

fn corners(lo: Vec3, hi: Vec3) -> [Vec3; 8] {
    [
        Vec3::new(lo.x, lo.y, lo.z),
        Vec3::new(hi.x, lo.y, lo.z),
        Vec3::new(lo.x, hi.y, lo.z),
        Vec3::new(hi.x, hi.y, lo.z),
        Vec3::new(lo.x, lo.y, hi.z),
        Vec3::new(hi.x, lo.y, hi.z),
        Vec3::new(lo.x, hi.y, hi.z),
        Vec3::new(hi.x, hi.y, hi.z),
    ]
}

/// Every catalog piece's sockets, resolved once.
///
/// Eager rather than lazy because [`SocketLookup`] hands out borrowed slices:
/// a cache that filled on demand would need interior mutability, and there are
/// fourteen pieces. The unresolvable case is reported at construction, so the
/// solver never has to have an opinion about a missing asset.
pub struct CatalogSockets {
    by_id: HashMap<String, Vec<ResolvedSocket>>,
    /// Each mesh piece's measured local bounds. Kept rather than discarded
    /// after the sockets are authored against it, because "how long is this
    /// face" has the same answer and the same one measurement behind it
    /// ([`crate::face`]); re-opening the GLB to ask again would be a second
    /// reading of one number.
    bounds: HashMap<String, DAabb>,
}

impl CatalogSockets {
    /// Resolve the whole catalog against the GLBs under `meshes_root`.
    ///
    /// # Errors
    /// Fails if a mesh piece's GLB is missing or unreadable, or if it measures
    /// empty — a piece with no bounding box has every socket at the origin,
    /// which would snap silently and wrongly rather than loudly.
    pub fn load(meshes_root: impl Into<std::path::PathBuf>) -> anyhow::Result<Self> {
        let mut library = Library::new(meshes_root);
        let mut by_id = HashMap::new();
        let mut bounds = HashMap::new();
        for piece in pieces() {
            let (sockets, measured) = resolve(piece, &mut library)?;
            by_id.insert(piece.id.to_string(), sockets);
            if let Some(measured) = measured {
                bounds.insert(piece.id.to_string(), measured);
            }
        }
        Ok(Self { by_id, bounds })
    }

    /// One mesh piece's measured local bounds, or `None` for a generated piece
    /// — whose size is a function of its node's parameters, not of a file.
    #[must_use]
    pub fn bounds(&self, piece_id: &str) -> Option<DAabb> {
        self.bounds.get(piece_id).copied()
    }

    /// The pieces this holds sockets for, in catalog order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        pieces().iter().map(|p| p.id)
    }
}

/// One piece's sockets: authored against the measured bbox, or read off the
/// generator's end frames.
fn resolve(
    piece: &Piece,
    library: &mut Library,
) -> anyhow::Result<(Vec<ResolvedSocket>, Option<DAabb>)> {
    match piece.geometry {
        Geometry::Procedural(family) => Ok((procedural_sockets(default_params(family)), None)),
        // Both measured, and measured the same way: an assembly's box is the
        // union of its laid-out parts, so its authored sockets anchor against
        // what the operator sees rather than against its biggest part.
        Geometry::Mesh { .. } | Geometry::Assembly(_) => {
            let bbox = if let Geometry::Assembly(parts) = piece.geometry {
                assembly_bounds(parts, library)?
            } else {
                let (lo, hi) = library.get(piece.id)?.bounds();
                DAabb::new(lo.as_dvec3(), hi.as_dvec3())
            };
            let path = piece.id;
            anyhow::ensure!(
                bbox.size().max_element() > 0.0,
                "{path}: measured an empty bounding box"
            );
            let sockets = piece
                .sockets
                .iter()
                .map(|def| resolve_socket(def, &bbox))
                .collect();
            Ok((sockets, Some(bbox)))
        }
    }
}

impl SocketLookup for CatalogSockets {
    fn sockets(&self, piece_id: &str) -> &[ResolvedSocket] {
        self.by_id.get(piece_id).map_or(&[], Vec::as_slice)
    }
}

// ---------------------------------------------------------------------------
// The venue graph's view of the same geometry
// ---------------------------------------------------------------------------

/// The parameters a node's generator is standing at.
///
/// A placed node overrides the palette default one key at a time, so an absent
/// `span` means "the default span" rather than zero — which is why this reads
/// through [`default_params`] rather than constructing a [`Procedural`] from
/// scratch. `faces` is the generator's own bitmask, stored as a number because
/// `venue_node_params` is `(key, value)` and a second encoding for one column
/// would be a second thing to keep true.
#[must_use]
pub fn node_params(family: Family, params: &Params) -> Procedural {
    match (family, default_params(family)) {
        (Family::Truss, Procedural::Truss { span }) => Procedural::Truss {
            #[allow(clippy::cast_possible_truncation)]
            span: params.get("span", f64::from(span)) as f32,
        },
        (Family::Hinge, Procedural::Hinge { angle }) => Procedural::Hinge {
            #[allow(clippy::cast_possible_truncation)]
            angle: params.get("angle", f64::from(angle)) as f32,
        },
        (Family::Corner, Procedural::Corner { faces }) => Procedural::Corner {
            faces: params.lookup("faces").map_or(faces, face_set_from_bits),
        },
        // `default_params` is total over `Family`, so the pairs above are
        // exhaustive; matching on both halves is what makes that checkable
        // rather than asserted.
        (_, other) => other,
    }
}

/// A [`FaceSet`] out of the number `venue_node_params` holds, and back.
///
/// One bit per [`Face`], in [`Face::ALL`] order. Out-of-range bits are dropped
/// rather than refused: `Corner::new` widens anything under two ways to a
/// through-box, so every input names a corner that exists.
#[must_use]
pub fn face_set_from_bits(bits: f64) -> FaceSet {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bits = bits.round().clamp(0.0, 63.0) as u32;
    FaceSet::of(
        Face::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| bits & (1 << i) != 0)
            .map(|(_, f)| f),
    )
}

/// The inverse of [`face_set_from_bits`] — what `set_params` writes.
#[must_use]
pub fn face_set_bits(faces: FaceSet) -> f64 {
    f64::from(
        Face::ALL
            .into_iter()
            .enumerate()
            .filter(|(_, f)| faces.contains(*f))
            .fold(0_u32, |bits, (i, _)| bits | (1 << i)),
    )
}

/// Sockets for a venue-graph node: authored against a GLB's bbox, or read off
/// the generator's end frames **at the node's own parameters**.
///
/// [`CatalogSockets`] answers the same question for a *catalog* entry, at the
/// palette default. A placed 6 m truss has its ends 6 m apart, so the resolver
/// cannot use that answer, and this is the wrapper that supplies the difference.
pub struct VenueSockets {
    catalog: CatalogSockets,
    housings: Housings,
}

impl VenueSockets {
    /// Resolve the catalog once, against the GLBs under `meshes_root`, with
    /// `definitions` as the answer to "what is this fixture".
    ///
    /// The fixture bundle is a second library and a second parser, neither of
    /// which belongs here, so it arrives as a lookup. It is not optional: a
    /// supply that cannot say how tall a light is places every light wrong, and
    /// wrong-by-default is what buried sixty percent of every yoke inside the
    /// truss it hung from. [`NoFixtures`] is the honest way to say a caller has
    /// no lights to place.
    ///
    /// # Errors
    /// As [`CatalogSockets::load`], plus the bundled fixture meshes: a housing
    /// whose mesh is missing cannot be measured, and a clamp guessed at the
    /// origin is the bug this exists to close.
    pub fn load(
        meshes_root: impl Into<std::path::PathBuf>,
        definitions: Arc<dyn FixtureDefinitions>,
    ) -> anyhow::Result<Self> {
        let meshes_root = meshes_root.into();
        let mut library = Library::new(meshes_root.clone());
        let housings = Housings::measure(&mut library, definitions)?;
        Ok(Self {
            catalog: CatalogSockets::load(meshes_root)?,
            housings,
        })
    }

    /// The catalog view, for the drag-time solver.
    #[must_use]
    pub fn catalog(&self) -> &CatalogSockets {
        &self.catalog
    }
}

/// What a fixture *is*, keyed the way a fixture node names it: by the bundle
/// path its patch row carries.
///
/// The one thing the socket supply cannot work out for itself. Parsing a QLC+
/// bundle is the app's job and reading a venue's patch needs a database, so the
/// two meet here as a lookup rather than as a dependency.
pub trait FixtureDefinitions: Send + Sync {
    /// The definition `fixture_path` names, or `None` when the bundle no longer
    /// has it — a venue outlives a fixture library.
    fn definition(&self, fixture_path: &str) -> Option<Definition>;
}

/// A supply for a caller with no fixture bundle at all: every fixture is
/// unknown, and unknown is said out loud rather than guessed at.
///
/// Placing a light against this puts its clamp at its own origin, which is
/// where a light with no stated size has to hang. Use it for a room of pieces,
/// not for a rig.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFixtures;

impl FixtureDefinitions for NoFixtures {
    fn definition(&self, _fixture_path: &str) -> Option<Definition> {
        None
    }
}

/// A supply that answers every fixture path with one stated housing.
///
/// Not a shortcut past [`FixtureDefinitions`] — a statement. A caller with no
/// bundle (a golden rig, a room previewed away from its library) still has to
/// say what its lights are shaped like, and saying "they are all this one" out
/// loud beats hanging every one of them by its pivot.
///
/// [`Self::stock_head`] is the housing both venue-pose goldens are pinned
/// against, stated here once so the two crates that rebuild that rig cannot
/// state it differently.
pub struct StatedHousing(pub Definition);

impl StatedHousing {
    /// A 300 x 500 x 300 mm moving head.
    #[must_use]
    pub fn stock_head() -> Self {
        Self(Definition {
            kind: "Moving Head".into(),
            modes: Vec::new(),
            physical: Some(crate::scene_desc::Physical {
                dimensions: Some(crate::scene_desc::Dimensions {
                    width: 300.0,
                    height: 500.0,
                    depth: 300.0,
                }),
                layout: None,
                lens: None,
            }),
        })
    }
}

impl FixtureDefinitions for StatedHousing {
    fn definition(&self, _fixture_path: &str) -> Option<Definition> {
        Some(self.0.clone())
    }
}

/// Where a fixture's housing ends, so the clamp can be put there.
///
/// A fixture's node origin is its **pivot** — the point a head pans and tilts
/// about, which is what the bundled meshes are authored around — and that is
/// nowhere near the top of the body: `moving_head.glb` carries 60% of its
/// height above its own origin. The clamp plane is that top, in metres above
/// the origin, and it is the whole of what makes a light hang *off* a truss
/// rather than through it.
///
/// The six bundled meshes are measured once, beside the catalog's own
/// measurement and out of the same [`Library`]; the per-definition answer is
/// memoized by bundle path, because a venue solve asks it once per light per
/// read and reopening a GLB for it would be the one thing that makes the solve
/// not free.
struct Housings {
    /// Each model kind's mesh, in its own authored space, keyed by mesh file.
    meshes: HashMap<&'static str, DAabb>,
    definitions: Arc<dyn FixtureDefinitions>,
    standoffs: RwLock<HashMap<String, f64>>,
}

impl Housings {
    fn measure(
        library: &mut Library,
        definitions: Arc<dyn FixtureDefinitions>,
    ) -> anyhow::Result<Self> {
        let mut meshes = HashMap::new();
        for kind in ModelKind::ALL {
            let relative = format!("qlc/{}", kind.mesh());
            let (lo, hi) = library.get(&relative)?.bounds();
            let bbox = DAabb::new(lo.as_dvec3(), hi.as_dvec3());
            anyhow::ensure!(
                bbox.size().y > 0.0,
                "{relative}: measured no height, so a clamp cannot be put on it"
            );
            meshes.insert(kind.mesh(), bbox);
        }
        Ok(Self {
            meshes,
            definitions,
            standoffs: RwLock::new(HashMap::new()),
        })
    }

    /// How far above its own origin `fixture_path`'s clamp sits, in metres.
    fn standoff(&self, fixture_path: &str) -> f64 {
        if let Some(known) = self
            .standoffs
            .read()
            .ok()
            .and_then(|memo| memo.get(fixture_path).copied())
        {
            return known;
        }
        let answer = self
            .definitions
            .definition(fixture_path)
            .map_or(0.0, |def| self.measure_definition(&def));
        if let Ok(mut memo) = self.standoffs.write() {
            memo.insert(fixture_path.to_string(), answer);
        }
        answer
    }

    fn measure_definition(&self, def: &Definition) -> f64 {
        clamp_standoff(def, |kind| self.meshes.get(kind.mesh()).copied())
    }
}

/// The clamp plane of one definition: how far its housing reaches above its own
/// origin, given a way to measure the bundled meshes.
///
/// The single definition of "where does this body stop", shared by the socket
/// supply that hangs the light and by the frame builder that draws it. Two
/// answers here is a rig whose bodies do not touch the truss they are bolted
/// to.
///
/// A **modelled** kind is its mesh's `+Y` extent, scaled the way
/// `housing_draws` scales the mesh itself — to the definition's physical
/// height. Everything else is drawn as its dimension box turned onto the mount
/// normal, so its clamp is half its depth: the box is centred on the origin, so
/// its back face is `depth / 2` above it.
#[must_use]
pub fn clamp_standoff(def: &Definition, mesh: impl Fn(ModelKind) -> Option<DAabb>) -> f64 {
    let dims = def.dimensions_m();
    let half_depth = f64::from(dims[2]) / 2.0;
    if is_procedural(def) {
        return half_depth;
    }
    let Some(bbox) = model_kind(def).and_then(mesh) else {
        return half_depth;
    };
    let extent = bbox.size().y;
    if extent <= 0.0 {
        return 0.0;
    }
    bbox.max.y / extent * f64::from(dims[1])
}

impl NodeSockets for VenueSockets {
    /// A node's local box: measured off the GLB, or computed from the
    /// generator at the node's own parameters.
    ///
    /// `None` for a fixture (its housing is the QLC+ definition's business, not
    /// the solver's) and for a `catalog_ref` the catalog has dropped — the
    /// authoring layer treats "unknown" as "cannot say whether it is in the
    /// way", which is the honest answer and the one a guessed box would hide.
    fn bounds(&self, node: &Node) -> Option<DAabb> {
        let catalog_ref = node.catalog_ref.as_deref()?;
        if node.kind == NodeKind::Fixture {
            return None;
        }
        match luma_scene::catalog::piece(catalog_ref).map(|p| p.geometry) {
            Some(Geometry::Procedural(family)) => {
                Some(procedural_bounds(node_params(family, &node.params)))
            }
            Some(Geometry::Mesh { .. } | Geometry::Assembly(_)) => self.catalog.bounds(catalog_ref),
            None => None,
        }
    }

    fn is_known(&self, node: &Node) -> bool {
        // A fixture names a patch row, which was never a catalog entry.
        node.kind == NodeKind::Fixture
            || node
                .catalog_ref
                .as_deref()
                .is_some_and(|id| luma_scene::catalog::piece(id).is_some())
    }

    fn sockets(&self, node: &Node) -> Vec<ResolvedSocket> {
        let Some(catalog_ref) = node.catalog_ref.as_deref() else {
            return Vec::new();
        };
        // A fixture's `catalog_ref` is a `fixtures` row id, not a piece: it
        // hangs off its host's socket and needs only one of its own to hang by.
        if node.kind == NodeKind::Fixture {
            return vec![fixture_clamp(self.housings.standoff(catalog_ref))];
        }
        match luma_scene::catalog::piece(catalog_ref).map(|p| p.geometry) {
            Some(Geometry::Procedural(family)) => {
                procedural_sockets(node_params(family, &node.params))
            }
            // Both measured: an assembly's authored sockets resolve against
            // its union box exactly as a lone mesh's resolve against its own.
            Some(Geometry::Mesh { .. } | Geometry::Assembly(_)) => {
                self.catalog.sockets(catalog_ref).to_vec()
            }
            // A venue outlives a catalog: the four ripped truss GLBs left the
            // palette, and rows naming them did not. Such a piece keeps an
            // origin to hang by, so its pose survives; the resolver reports it
            // as `UnknownCatalogRef` so nobody mistakes surviving for fine.
            None => vec![origin_mount()],
        }
    }
}

/// The socket every node has whether or not anything authored one: its own
/// origin, facing down, so it can rest on a surface.
///
/// It is what an unrecognised piece is placed by, and it is deliberately
/// `Male` — it can be held, never hosted, so nothing can be bolted *to* a piece
/// whose geometry is unknown.
#[must_use]
pub fn origin_mount() -> ResolvedSocket {
    ResolvedSocket::from_frame(
        ORIGIN_SOCKET,
        SocketType::BottomMount,
        DVec3::ZERO,
        DVec3::NEG_Y,
        DVec3::X,
    )
}

/// The name of the socket [`origin_mount`] declares.
pub const ORIGIN_SOCKET: &str = "origin";

/// The one socket every fixture has: the clamp, `standoff` metres above its
/// own origin, facing up.
///
/// A fixture is not a catalog piece — it is a row in the patch — so this is the
/// whole of its geometry as far as placing it goes. Two things are load-bearing
/// about the frame:
///
/// - the **normal is `+Y`**, which is where a clamp points: at the surface it
///   grips. The beam is the other way, and it stays the other way because a
///   face mate opposes the two normals — which is exactly the half turn that
///   used to be written out by hand in `mate_suffix`.
/// - the **position is not the origin**. A fixture's origin is its pivot, not
///   its top; see [`clamp_standoff`]. Placing by the origin hung every housing
///   through the truss instead of under it.
#[must_use]
pub fn fixture_clamp(standoff: f64) -> ResolvedSocket {
    ResolvedSocket::from_frame(
        FIXTURE_CLAMP_SOCKET,
        SocketType::EquipmentMount,
        DVec3::Y * standoff,
        DVec3::Y,
        DVec3::X,
    )
}

/// The name of the socket [`fixture_clamp`] declares.
pub const FIXTURE_CLAMP_SOCKET: &str = "clamp";

#[cfg(test)]
mod tests {
    use super::*;

    fn meshes_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes")
    }

    /// The load-bearing one: every piece in the catalog resolves every socket
    /// it declares, against the asset that actually ships.
    #[test]
    fn every_catalog_piece_resolves_its_sockets() {
        let sockets = CatalogSockets::load(meshes_root()).expect("catalog resolves");
        for piece in pieces() {
            let resolved = sockets.sockets(piece.id);
            let want = if piece.geometry.is_procedural() {
                // grab + one bolt face per face the family offers + the
                // stick's four mount sides + its footings. A block offers all
                // six of its plates; a stick and a hinge offer their ends.
                match piece.geometry {
                    Geometry::Procedural(f) => {
                        let params = default_params(f);
                        let bolt_faces = match f {
                            Family::Corner => Face::ALL.len(),
                            Family::Truss | Family::Hinge => params.end_frames().len(),
                        };
                        bolt_faces + 1 + mount_faces(params).len() + footings(params).len()
                    }
                    Geometry::Mesh { .. } | Geometry::Assembly(_) => unreachable!(),
                }
            } else {
                piece.sockets.len()
            };
            assert_eq!(resolved.len(), want, "{}: socket count", piece.id);
            assert!(
                resolved.iter().any(|s| s.socket_type == SocketType::Grab),
                "{}: no grab socket",
                piece.id
            );
            for s in resolved {
                assert!(
                    s.position.is_finite() && s.normal.is_normalized(),
                    "{}/{}: degenerate frame",
                    piece.id,
                    s.name
                );
            }
        }
    }

    /// Sockets are authored against the bbox, so a piece whose GLB is metres
    /// across must have sockets metres apart — this catches a mesh loaded at
    /// the wrong scale, which resolves "successfully" into a piece the size of
    /// a coin.
    #[test]
    fn deck_sockets_span_the_deck() {
        let sockets = CatalogSockets::load(meshes_root()).expect("catalog resolves");
        let deck = sockets.sockets("stage_lab/stage_praticavel_1x1.glb");
        let left = deck.iter().find(|s| s.name == "edge_left").expect("left");
        let right = deck.iter().find(|s| s.name == "edge_right").expect("right");
        let width = (right.position - left.position).length();
        assert!(
            (0.9..1.1).contains(&width),
            "1×1 m deck measured {width} m across"
        );
    }

    /// A block offers all six of its plates, whichever of them are open ways.
    /// Its ways are where the lattice runs through; every face is still a
    /// plate, and a plate is what the next stick bolts to — so which face a
    /// run leaves on is a thing the pointer can choose.
    #[test]
    fn a_corner_offers_every_plate_not_only_its_ways() {
        let names = |faces| -> Vec<String> {
            procedural_sockets(Procedural::Corner { faces })
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        let all = [
            "grab", "face_-x", "face_+x", "face_-y", "face_+y", "face_-z", "face_+z", "seat",
        ];
        assert_eq!(
            names(FaceSet::of([Face::NegX, Face::PosY, Face::PosZ])),
            all
        );
        // The ways change the mesh, never the faces on offer.
        assert_eq!(names(FaceSet::THROUGH), all);
    }

    /// Two generated pieces mate through the same socket vocabulary the GLB
    /// pieces use: a truss end is a truss end whatever produced it.
    #[test]
    fn generated_ends_are_truss_ends() {
        for family in [Family::Truss, Family::Corner, Family::Hinge] {
            for s in procedural_sockets(default_params(family)) {
                // `TrussFace` joined the vocabulary with the stick's four long
                // sides: a *face* hosts a clamp, an *end* bolts structure, and
                // keeping them different types is what makes hanging a light
                // off a bolt plate a refusal rather than a short face.
                // `BottomMount` is the footing a piece is *put down* on —
                // see `footings`. It is the same type the deck's `bottom` and
                // the speaker's `mount` carry, which is what lets a truss rest
                // on a floor at all.
                assert!(matches!(
                    s.socket_type,
                    SocketType::Grab
                        | SocketType::TrussEnd
                        | SocketType::TrussFace
                        | SocketType::BottomMount
                ));
            }
        }
    }

    /// Only the stick has mount faces: a corner block's faces are its ways and
    /// a hinge's are its leaves, both of which bolt rather than host.
    #[test]
    fn only_the_straight_family_has_mount_faces() {
        let names = |family| -> Vec<String> {
            procedural_sockets(default_params(family))
                .into_iter()
                .filter(|s| s.socket_type == SocketType::TrussFace)
                .map(|s| s.name)
                .collect()
        };
        assert_eq!(
            names(Family::Truss),
            ["face_-y", "face_+y", "face_-z", "face_+z"]
        );
        assert!(names(Family::Corner).is_empty());
        assert!(names(Family::Hinge).is_empty());
    }
}
