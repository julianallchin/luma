//! Socket model for the stage builder — a port of
//! `src/features/stage/lib/sockets.ts`.
//!
//! Each mesh declares a set of named anchor points in its **local asset frame**
//! (Y-up, as glTF mandates). Sockets carry a type tag and a normal; matching
//! sockets magnetize together during placement and drag (see [`crate::snap`]).
//!
//! Authoring is done relative to the mesh's **bounding box** (measured at load
//! time) rather than raw local coordinates, so socket positions stay correct
//! regardless of where the modeler placed the GLB pivot.
//!
//! Frame reminder: `+X` right, `+Y` up, `+Z` front (toward the camera).
//! `top` = +Y, `bottom` = -Y, `front` = +Z, `back` = -Z, `right` = +X,
//! `left` = -X.

use crate::aabb::DAabb;
use glam::DVec3;

// ---------------------------------------------------------------------------
// Socket types: kind + polarity
// ---------------------------------------------------------------------------

/// What a socket *is*, geometrically — the equivalence class two sockets must
/// share before they can mate at all.
///
/// The kind answers "same joint?" and [`Polarity`] answers "which half?".
/// Together they replace the hand-maintained held→host adjacency list this
/// module used to carry: a thirteen-entry table is a lookup table pretending
/// to be a rule, and it drifted between its Rust and TypeScript copies, which
/// is why the golden vectors exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SocketKind {
    /// A flat plane something rests on: deck tops, stand tops, the ground, and
    /// the underside of everything that sits on them.
    Surface,
    /// The square end face of the truss family, and the point on a deck corner
    /// where one lands.
    TrussEnd,
    /// A mid-edge that butts against another mid-edge — deck to deck, rail to
    /// rail, rail to deck.
    Edge,
    /// A cable cover end, chaining into runs.
    CableEnd,
    /// The placement reference. Mates with nothing: it is the only kind with
    /// no [`Polarity::Male`] or [`Polarity::Neutral`] member, so
    /// [`SocketType::mates`] rejects every pair involving it without a special
    /// case.
    Grab,
}

impl SocketKind {
    pub const ALL: [SocketKind; 5] = [
        SocketKind::Surface,
        SocketKind::TrussEnd,
        SocketKind::Edge,
        SocketKind::CableEnd,
        SocketKind::Grab,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SocketKind::Surface => "surface",
            SocketKind::TrussEnd => "truss_end",
            SocketKind::Edge => "edge",
            SocketKind::CableEnd => "cable_end",
            SocketKind::Grab => "grab",
        }
    }
}

/// Which half of a joint a socket is.
///
/// `Male` is a plug and is only ever *held*; `Female` is a receptacle and is
/// only ever a *host*; `Neutral` self-mates and can be either. That is the
/// whole of the directionality the old adjacency list encoded by hand.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Polarity {
    Male,
    Female,
    Neutral,
}

impl Polarity {
    /// Whether a socket with this polarity may be the moving half.
    pub fn can_be_held(self) -> bool {
        matches!(self, Polarity::Male | Polarity::Neutral)
    }

    /// Whether a socket with this polarity may be the stationary half.
    pub fn can_host(self) -> bool {
        matches!(self, Polarity::Female | Polarity::Neutral)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Polarity::Male => "male",
            Polarity::Female => "female",
            Polarity::Neutral => "neutral",
        }
    }
}

/// How much a mated piece may still turn about the socket normal.
///
/// This is the *only* freedom a snapped piece has (`docs/design/venue-graph.md`
/// — snapped pieces get no transform gizmo), so it is a property of the socket
/// rather than of the editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RollFreedom {
    /// Bolted: the mate fully determines the pose.
    Fixed,
    /// Quantized: legal rolls are whole multiples of this many degrees.
    Steps(f64),
    /// Continuous — a piece sitting on a surface may yaw freely.
    Free,
}

/// The closed socket vocabulary. Each variant is a named point on a piece; what
/// it may attach to is derived from its [`SocketKind`] and [`Polarity`], never
/// listed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SocketType {
    /// Placement reference — the cursor follows this socket.
    Grab,
    /// Top surface of a deck (host).
    FloorTop,
    /// Mid-edge of a deck top.
    FloorEdge,
    /// Corner of a deck top, inset by the truss radius.
    FloorCorner,
    /// End of any truss.
    TrussEnd,
    /// Top of a speaker stand.
    StandTop,
    /// Bottom of a speaker stand.
    StandBottom,
    /// Bottom of a speaker.
    SpeakerMount,
    /// Bottom of a CDJ / mixer / cable cover.
    EquipmentMount,
    /// Bottom of any "sits on a flat surface" piece (deck, guardrail, ...).
    BottomMount,
    /// End of a guardrail.
    RailEnd,
    /// End of a cable cover (chains end-to-end).
    CableEnd,
    /// The implicit ground plane (a virtual host).
    Ground,
}

impl SocketType {
    /// Every variant, in declaration order. The generated TypeScript binding
    /// and the polarity-equivalence test both sweep this.
    pub const ALL: [SocketType; 13] = {
        use SocketType::*;
        [
            Grab,
            FloorTop,
            FloorEdge,
            FloorCorner,
            TrussEnd,
            StandTop,
            StandBottom,
            SpeakerMount,
            EquipmentMount,
            BottomMount,
            RailEnd,
            CableEnd,
            Ground,
        ]
    };

    /// The joint this socket belongs to. Two sockets of different kinds never
    /// mate.
    pub fn kind(self) -> SocketKind {
        use SocketType::*;
        match self {
            Grab => SocketKind::Grab,
            FloorTop | StandTop | Ground | BottomMount | StandBottom | SpeakerMount
            | EquipmentMount => SocketKind::Surface,
            TrussEnd | FloorCorner => SocketKind::TrussEnd,
            FloorEdge | RailEnd => SocketKind::Edge,
            CableEnd => SocketKind::CableEnd,
        }
    }

    /// Which half of its joint this socket is. See [`Polarity`].
    pub fn polarity(self) -> Polarity {
        use SocketType::*;
        match self {
            // Receptacles: things get put *on* them, they are never carried
            // onto something else. `Grab` is one so that it drops out of the
            // held set without a name check.
            Grab | FloorTop | StandTop | Ground | FloorCorner => Polarity::Female,
            // Plugs: the undersides.
            BottomMount | StandBottom | SpeakerMount | EquipmentMount => Polarity::Male,
            // Self-mating.
            TrussEnd | FloorEdge | RailEnd | CableEnd => Polarity::Neutral,
        }
    }

    /// Default roll freedom for a socket of this type, which a [`SocketDef`]
    /// may override for a piece whose joint differs from its type's.
    pub fn roll(self) -> RollFreedom {
        use SocketType::*;
        match self {
            // A truss bolts to a plate on a fixed bolt circle. The section is
            // square, so the geometry would admit `Steps(90.0)`; whether the
            // builder offers that quarter turn is a phase-4 UX call, and until
            // it does, claiming the freedom would be a lie about what the
            // editor can express.
            TrussEnd | FloorCorner => RollFreedom::Fixed,
            // Edge joints are tangent-aligned by construction.
            FloorEdge | RailEnd | CableEnd => RollFreedom::Fixed,
            // Anything resting on a plane may yaw about its normal.
            Grab | FloorTop | StandTop | Ground | BottomMount | StandBottom | SpeakerMount
            | EquipmentMount => RollFreedom::Free,
        }
    }

    /// Whether a held socket of type `self` may mate a host socket of type
    /// `host`.
    ///
    /// The whole rule: same kind, the held half may be held, the host half may
    /// host. Polarities are opposed-or-neutral by construction — the only
    /// pairs the two `can_*` tests admit are (Male, Female), (Male, Neutral),
    /// (Neutral, Female) and (Neutral, Neutral).
    pub fn mates(self, host: SocketType) -> bool {
        self.kind() == host.kind() && self.polarity().can_be_held() && host.polarity().can_host()
    }

    /// The wire name, shared with the TS side and the golden vectors.
    pub fn as_str(self) -> &'static str {
        use SocketType::*;
        match self {
            Grab => "grab",
            FloorTop => "floor_top",
            FloorEdge => "floor_edge",
            FloorCorner => "floor_corner",
            TrussEnd => "truss_end",
            StandTop => "stand_top",
            StandBottom => "stand_bottom",
            SpeakerMount => "speaker_mount",
            EquipmentMount => "equipment_mount",
            BottomMount => "bottom_mount",
            RailEnd => "rail_end",
            CableEnd => "cable_end",
            Ground => "ground",
        }
    }

    pub fn from_name(name: &str) -> Option<SocketType> {
        use SocketType::*;
        Some(match name {
            "grab" => Grab,
            "floor_top" => FloorTop,
            "floor_edge" => FloorEdge,
            "floor_corner" => FloorCorner,
            "truss_end" => TrussEnd,
            "stand_top" => StandTop,
            "stand_bottom" => StandBottom,
            "speaker_mount" => SpeakerMount,
            "equipment_mount" => EquipmentMount,
            "bottom_mount" => BottomMount,
            "rail_end" => RailEnd,
            "cable_end" => CableEnd,
            "ground" => Ground,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Bbox-relative authoring
// ---------------------------------------------------------------------------

/// 27 named anchor points on an axis-aligned bbox: 1 centroid, 6 face centres,
/// 12 edge midpoints, 8 corners.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BboxAnchor {
    Center,
    // faces (1 axis named)
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
    // edges (2 axes named)
    TopFront,
    TopBack,
    TopLeft,
    TopRight,
    BottomFront,
    BottomBack,
    BottomLeft,
    BottomRight,
    FrontLeft,
    FrontRight,
    BackLeft,
    BackRight,
    // corners (3 axes named — order: top/bottom, front/back, left/right)
    TopFrontLeft,
    TopFrontRight,
    TopBackLeft,
    TopBackRight,
    BottomFrontLeft,
    BottomFrontRight,
    BottomBackLeft,
    BottomBackRight,
}

/// The single anchor table: name ↔ variant ↔ per-axis signs in `{-1, 0, +1}`.
/// Every other anchor function reads this, so the vocabulary cannot drift.
const ANCHORS: [(BboxAnchor, &str, [f64; 3]); 27] = {
    use BboxAnchor::*;
    [
        (Center, "center", [0.0, 0.0, 0.0]),
        (Top, "top", [0.0, 1.0, 0.0]),
        (Bottom, "bottom", [0.0, -1.0, 0.0]),
        (Front, "front", [0.0, 0.0, 1.0]),
        (Back, "back", [0.0, 0.0, -1.0]),
        (Left, "left", [-1.0, 0.0, 0.0]),
        (Right, "right", [1.0, 0.0, 0.0]),
        (TopFront, "top_front", [0.0, 1.0, 1.0]),
        (TopBack, "top_back", [0.0, 1.0, -1.0]),
        (TopLeft, "top_left", [-1.0, 1.0, 0.0]),
        (TopRight, "top_right", [1.0, 1.0, 0.0]),
        (BottomFront, "bottom_front", [0.0, -1.0, 1.0]),
        (BottomBack, "bottom_back", [0.0, -1.0, -1.0]),
        (BottomLeft, "bottom_left", [-1.0, -1.0, 0.0]),
        (BottomRight, "bottom_right", [1.0, -1.0, 0.0]),
        (FrontLeft, "front_left", [-1.0, 0.0, 1.0]),
        (FrontRight, "front_right", [1.0, 0.0, 1.0]),
        (BackLeft, "back_left", [-1.0, 0.0, -1.0]),
        (BackRight, "back_right", [1.0, 0.0, -1.0]),
        (TopFrontLeft, "top_front_left", [-1.0, 1.0, 1.0]),
        (TopFrontRight, "top_front_right", [1.0, 1.0, 1.0]),
        (TopBackLeft, "top_back_left", [-1.0, 1.0, -1.0]),
        (TopBackRight, "top_back_right", [1.0, 1.0, -1.0]),
        (BottomFrontLeft, "bottom_front_left", [-1.0, -1.0, 1.0]),
        (BottomFrontRight, "bottom_front_right", [1.0, -1.0, 1.0]),
        (BottomBackLeft, "bottom_back_left", [-1.0, -1.0, -1.0]),
        (BottomBackRight, "bottom_back_right", [1.0, -1.0, -1.0]),
    ]
};

impl BboxAnchor {
    pub const ALL: [BboxAnchor; 27] = {
        let mut all = [BboxAnchor::Center; 27];
        let mut i = 0;
        while i < 27 {
            all[i] = ANCHORS[i].0;
            i += 1;
        }
        all
    };

    fn entry(self) -> &'static (BboxAnchor, &'static str, [f64; 3]) {
        ANCHORS
            .iter()
            .find(|(a, _, _)| *a == self)
            .expect("every BboxAnchor variant is in ANCHORS")
    }

    pub fn as_str(self) -> &'static str {
        self.entry().1
    }

    pub fn from_name(name: &str) -> Option<BboxAnchor> {
        ANCHORS
            .iter()
            .find(|(_, n, _)| *n == name)
            .map(|(a, _, _)| *a)
    }

    /// Per-axis signs in `{-1, 0, +1}`.
    pub fn signs(self) -> DVec3 {
        DVec3::from_array(self.entry().2)
    }

    /// How many axes the anchor names: 0 = centre, 1 = face, 2 = edge,
    /// 3 = corner.
    fn named_axes(self) -> u32 {
        self.signs()
            .to_array()
            .iter()
            .filter(|s| **s != 0.0)
            .count() as u32
    }
}

/// Resolve `(anchor, bbox)` into a local-space point.
pub fn resolve_anchor(anchor: BboxAnchor, bbox: &DAabb) -> DVec3 {
    bbox.center() + anchor.signs() * bbox.size() * 0.5
}

// ---------------------------------------------------------------------------
// Socket definition (authoring shape)
// ---------------------------------------------------------------------------

/// How two compatible sockets meet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum SocketMode {
    /// The two normals **oppose** (face-to-face contact) — a down-facing
    /// `SpeakerMount` lands on an up-facing `StandTop`. The held piece is
    /// rotated 180° about the host socket's tangent.
    #[default]
    Face,
    /// The held piece takes the host's orientation **unchanged**, so only a
    /// translation puts the held socket on the host socket — two stage decks
    /// joined edge to edge, both tops up, tangents running the same way.
    ///
    /// (The TS docstrings claimed edge mode rotates 180° about the host
    /// normal. `flipFor()` has always returned identity; the prose was wrong
    /// and the code is what shipped. Corrected here, in the port.)
    Edge,
}

impl SocketMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SocketMode::Face => "face",
            SocketMode::Edge => "edge",
        }
    }

    pub fn from_name(name: &str) -> Option<SocketMode> {
        match name {
            "face" => Some(SocketMode::Face),
            "edge" => Some(SocketMode::Edge),
            _ => None,
        }
    }
}

/// Authored socket, before bbox resolution.
#[derive(Clone, Debug)]
pub struct SocketDef {
    pub name: String,
    pub socket_type: SocketType,
    /// Where on the bbox the socket sits.
    pub anchor: BboxAnchor,
    /// Offset from the anchor in metres, asset-local (Y-up). Insets a corner
    /// socket inward, lifts a face socket above the mesh surface, etc.
    pub offset: Option<DVec3>,
    /// Outward direction — the piece's "facing" at this socket. Derived from
    /// the anchor face when absent (e.g. `top` implies +Y); required for
    /// corner anchors, where the natural direction is ambiguous.
    pub normal: Option<DVec3>,
    /// In-plane tangent. Used by edge / rail sockets, where rotation about the
    /// normal matters (two `FloorEdge` sockets must be tangent-aligned for the
    /// decks to lie colinearly).
    pub tangent: Option<DVec3>,
    pub mode: SocketMode,
    /// Overrides [`SocketType::roll`] for this one socket.
    pub roll: Option<RollFreedom>,
}

impl SocketDef {
    /// A socket at `anchor` with every optional field defaulted: no offset, a
    /// normal derived from the anchor face, a tangent derived from the normal,
    /// [`SocketMode::Face`], and the type's own roll freedom.
    pub fn new(name: &str, socket_type: SocketType, anchor: BboxAnchor) -> Self {
        Self {
            name: name.to_string(),
            socket_type,
            anchor,
            offset: None,
            normal: None,
            tangent: None,
            mode: SocketMode::Face,
            roll: None,
        }
    }

    #[must_use]
    pub fn offset(mut self, offset: DVec3) -> Self {
        self.offset = Some(offset);
        self
    }

    #[must_use]
    pub fn normal(mut self, normal: DVec3) -> Self {
        self.normal = Some(normal);
        self
    }

    #[must_use]
    pub fn tangent(mut self, tangent: DVec3) -> Self {
        self.tangent = Some(tangent);
        self
    }

    #[must_use]
    pub fn mode(mut self, mode: SocketMode) -> Self {
        self.mode = mode;
        self
    }

    #[must_use]
    pub fn roll(mut self, roll: RollFreedom) -> Self {
        self.roll = Some(roll);
        self
    }
}

/// Resolved socket in mesh-local space: position plus an orthonormal frame.
#[derive(Clone, Debug)]
pub struct ResolvedSocket {
    pub name: String,
    pub socket_type: SocketType,
    pub position: DVec3,
    pub normal: DVec3,
    pub tangent: DVec3,
    pub mode: SocketMode,
    /// Unit vector from the piece's bbox centre to the socket position. Used
    /// by edge-mode pairing to ensure two matched sockets sit on **opposite
    /// sides** of their pieces, so the pieces end up next to each other rather
    /// than overlapping.
    pub outward: DVec3,
    /// How much the mated piece may still turn about [`Self::normal`].
    pub roll: RollFreedom,
}

impl ResolvedSocket {
    /// A socket straight from an orthonormal frame — position, outward normal,
    /// and the vector fixing its roll — rather than from a bbox anchor.
    ///
    /// This is how the procedural truss family gets its sockets: an open face
    /// already *is* a frame (`luma_render::truss::EndFrame`), so authoring one
    /// would be transcribing geometry that the generator already knows. The
    /// piece's origin is its centre, which is what makes `outward` derivable.
    pub fn from_frame(
        name: &str,
        socket_type: SocketType,
        position: DVec3,
        normal: DVec3,
        up: DVec3,
    ) -> Self {
        let normal = normalize(normal);
        Self {
            name: name.to_string(),
            socket_type,
            position,
            normal,
            tangent: normalize(up),
            mode: SocketMode::Face,
            outward: if position.length_squared() < OUTWARD_DEGENERATE_LENGTH_SQ {
                normal
            } else {
                normalize(position)
            },
            roll: socket_type.roll(),
        }
    }
}

/// three.js `Vector3.normalize()`: divides by the length, or leaves a
/// zero-length vector at zero. `DVec3::normalize` would hand back NaN, and the
/// goldens pin the zero-normal case.
fn normalize(v: DVec3) -> DVec3 {
    v.normalize_or_zero()
}

/// Default normal for an anchor when none is authored. Faces are unambiguous;
/// edges pick the vertical face's outward direction (so `top_front` defaults
/// to +Y); corners and the centre have no answer and the caller must supply
/// one.
fn default_normal(anchor: BboxAnchor) -> Option<DVec3> {
    let s = anchor.signs();
    match anchor.named_axes() {
        1 => Some(s),
        2 => Some(if s.y != 0.0 {
            DVec3::new(0.0, s.y, 0.0)
        } else if s.z != 0.0 {
            DVec3::new(0.0, 0.0, s.z)
        } else {
            DVec3::new(s.x, 0.0, 0.0)
        }),
        _ => None,
    }
}

/// Below this squared length, the socket sits on the bbox centre and `outward`
/// has no meaningful direction — fall back to the normal.
const OUTWARD_DEGENERATE_LENGTH_SQ: f64 = 1e-6;

/// Above this |dot| with world-up, the normal is treated as parallel to up and
/// the tangent is derived against +X instead.
const TANGENT_UP_PARALLEL: f64 = 0.99;

pub fn resolve_socket(def: &SocketDef, bbox: &DAabb) -> ResolvedSocket {
    let base = resolve_anchor(def.anchor, bbox) + def.offset.unwrap_or(DVec3::ZERO);

    let normal = match def.normal {
        Some(n) => normalize(n),
        None => default_normal(def.anchor).unwrap_or(DVec3::Y),
    };

    let tangent = match def.tangent {
        Some(t) => normalize(t),
        // Derive a perpendicular: world-up unless the normal is parallel to
        // it, in which case world-X.
        None => {
            let candidate = if normal.dot(DVec3::Y).abs() < TANGENT_UP_PARALLEL {
                DVec3::Y
            } else {
                DVec3::X
            };
            normalize(normal.cross(candidate))
        }
    };

    let delta = base - bbox.center();
    let outward = if delta.length_squared() < OUTWARD_DEGENERATE_LENGTH_SQ {
        normal
    } else {
        normalize(delta)
    };

    ResolvedSocket {
        name: def.name.clone(),
        socket_type: def.socket_type,
        position: base,
        normal,
        tangent,
        mode: def.mode,
        outward,
        roll: def.roll.unwrap_or_else(|| def.socket_type.roll()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_table_is_complete_and_unique() {
        assert_eq!(BboxAnchor::ALL.len(), 27);
        for a in BboxAnchor::ALL {
            assert_eq!(BboxAnchor::from_name(a.as_str()), Some(a));
        }
        let mut names: Vec<&str> = ANCHORS.iter().map(|(_, n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 27);
    }

    #[test]
    fn socket_type_names_round_trip() {
        for t in SocketType::ALL {
            assert_eq!(SocketType::from_name(t.as_str()), Some(t));
        }
    }

    /// The directed `COMPATIBLE` table this module shipped before polarity,
    /// held-side → host-side. Kept here as the reference the new rule is
    /// measured against, and nowhere else.
    fn legacy_compatible(t: SocketType) -> &'static [SocketType] {
        use SocketType::*;
        match t {
            Grab | FloorTop | FloorCorner | StandTop | Ground => &[],
            FloorEdge => &[FloorEdge],
            TrussEnd => &[TrussEnd, FloorCorner],
            StandBottom => &[FloorTop, Ground],
            SpeakerMount => &[StandTop, FloorTop, Ground],
            EquipmentMount | BottomMount => &[FloorTop, Ground],
            RailEnd => &[RailEnd, FloorEdge],
            CableEnd => &[CableEnd],
        }
    }

    /// Every pair the new rule admits that the old table did not, and why.
    ///
    /// The new rule is a strict *superset*: it adds nothing but the pairs the
    /// old table's asymmetry excluded by hand. Each one is a joint that
    /// physically exists — the old table simply never listed it, because a
    /// hand-maintained adjacency list only holds the cases someone thought of.
    const INTENTIONAL_ADDITIONS: [(SocketType, SocketType, &str); 4] = {
        use SocketType::*;
        [
            // "Anything that sits on a flat surface can sit on a stand top."
            // The old table let only a speaker onto a stand; a deck, a rail or
            // a CDJ is the same joint.
            (StandBottom, StandTop, "a stand on a stand top"),
            (EquipmentMount, StandTop, "a CDJ or mixer on a stand top"),
            (BottomMount, StandTop, "a deck or rail on a stand top"),
            // Butting a deck edge against a rail was allowed rail-first and
            // refused deck-first. Which piece the user happens to be dragging
            // is not a property of the joint.
            (FloorEdge, RailEnd, "a deck edge butted to a rail end"),
        ]
    };

    #[test]
    fn polarity_reproduces_the_legacy_table() {
        for held in SocketType::ALL {
            for host in SocketType::ALL {
                let was = legacy_compatible(held).contains(&host);
                let now = held.mates(host);
                let added = INTENTIONAL_ADDITIONS
                    .iter()
                    .any(|(h, o, _)| *h == held && *o == host);
                if was {
                    assert!(
                        now,
                        "polarity dropped a pair the table allowed: {} → {}",
                        held.as_str(),
                        host.as_str()
                    );
                }
                if now && !was {
                    assert!(
                        added,
                        "polarity admits an undocumented pair: {} → {}",
                        held.as_str(),
                        host.as_str()
                    );
                }
            }
        }
        // And every documented addition is actually new, so the list cannot
        // rot into a list of pairs that were always legal.
        for (held, host, why) in INTENTIONAL_ADDITIONS {
            assert!(held.mates(host), "{why}: rule refuses it");
            assert!(
                !legacy_compatible(held).contains(&host),
                "{why}: the old table already allowed it"
            );
        }
    }

    /// The set of sockets the solver will *consider* on a held piece. It used
    /// to be "not a grab, and its compatibility list is non-empty"; it is now
    /// "its polarity can be held". The two must agree, or the golden snap
    /// vectors move.
    #[test]
    fn held_side_filter_is_unchanged() {
        for t in SocketType::ALL {
            let was = t != SocketType::Grab && !legacy_compatible(t).is_empty();
            assert_eq!(
                was,
                t.polarity().can_be_held(),
                "held-side filter changed for {}",
                t.as_str()
            );
        }
    }

    #[test]
    fn zero_normal_stays_zero() {
        // three.js normalize() leaves a zero vector alone; glam's would NaN.
        assert_eq!(normalize(DVec3::ZERO), DVec3::ZERO);
    }
}
