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
// Socket types + compatibility
// ---------------------------------------------------------------------------

/// The closed socket vocabulary. Adding a variant means teaching
/// [`SocketType::compatible`] what it may attach to.
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
    /// Held-side → host-side socket types it can attach to.
    ///
    /// Snapping is asymmetric by design (the held piece moves, the host is
    /// stationary). Types that snap together symmetrically — truss ends, floor
    /// edges, rail ends — list each other, because the solver only ever
    /// iterates held sockets looking for hosts.
    pub fn compatible(self) -> &'static [SocketType] {
        use SocketType::*;
        match self {
            Grab => &[],
            FloorTop => &[],
            FloorEdge => &[FloorEdge],
            FloorCorner => &[],
            TrussEnd => &[TrussEnd, FloorCorner],
            StandTop => &[],
            StandBottom => &[FloorTop, Ground],
            SpeakerMount => &[StandTop, FloorTop, Ground],
            EquipmentMount => &[FloorTop, Ground],
            BottomMount => &[FloorTop, Ground],
            RailEnd => &[RailEnd, FloorEdge],
            CableEnd => &[CableEnd],
            Ground => &[],
        }
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
        for t in [
            SocketType::Grab,
            SocketType::FloorTop,
            SocketType::FloorEdge,
            SocketType::FloorCorner,
            SocketType::TrussEnd,
            SocketType::StandTop,
            SocketType::StandBottom,
            SocketType::SpeakerMount,
            SocketType::EquipmentMount,
            SocketType::BottomMount,
            SocketType::RailEnd,
            SocketType::CableEnd,
            SocketType::Ground,
        ] {
            assert_eq!(SocketType::from_name(t.as_str()), Some(t));
        }
    }

    #[test]
    fn zero_normal_stays_zero() {
        // three.js normalize() leaves a zero vector alone; glam's would NaN.
        assert_eq!(normalize(DVec3::ZERO), DVec3::ZERO);
    }
}
