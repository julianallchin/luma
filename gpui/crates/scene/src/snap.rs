//! Socket-based snap solver — a port of `src/features/stage/lib/snap.ts`.
//!
//! The held piece (the one following the cursor) has a `Grab` socket that the
//! cursor pulls toward a target world point. The solver iterates every
//! "useful" socket on the held piece against every compatible socket on every
//! other piece in the scene.
//!
//! For each candidate (held socket `Sh`, host socket `Sho` on host piece `Ph`):
//!
//! 1. Mate the pair through [`crate::venue::place_on`] with
//!    [`crate::venue::SurfacePlacement::FLUSH`] — the one copy of the mate
//!    arithmetic in the crate, shared with the venue resolver.
//! 2. Score the candidate by how close the host socket comes to the cursor —
//!    in **pixels** when the caller hands in an [`Aim`], which is what makes
//!    "snap to the corner I am pointing at" mean the corner on screen rather
//!    than the one that happens to be nearest in metres. Without an aim the
//!    score is world metres, which is what the port goldens pin.
//!
//! Non-ground hosts are evaluated first; if the best score is within
//! [`attach_radius`], the snap is applied and the held piece's parent
//! becomes `Ph`. Otherwise a surface under the cursor is offered, then ground,
//! then free placement.
//!
//! This is the **candidate search**, and it is the half of snapping the venue
//! resolver does not do: [`crate::venue::resolve`] mates a pair the graph has
//! already chosen, and this chooses the pair. Phase 4's drag-snap is its
//! caller, which is why it stays even with no production caller yet.
//!
//! Frame: asset/world space is Y-up here, f64. See the crate docs.

use crate::sockets::{ResolvedSocket, SocketMode, SocketType};
use crate::venue::{place_on, NodeKind, SurfacePlacement};
use glam::{DMat3, DMat4, DQuat, DVec2, DVec3};
use std::collections::HashMap;

/// Sockets-by-mesh accessor. In production this is backed by the mesh cache;
/// in tests a `HashMap` is enough, which is what keeps the solver testable
/// without loading a single GLB.
pub trait SocketLookup {
    fn sockets(&self, mesh_path: &str) -> &[ResolvedSocket];
}

impl SocketLookup for HashMap<String, Vec<ResolvedSocket>> {
    fn sockets(&self, mesh_path: &str) -> &[ResolvedSocket] {
        self.get(mesh_path).map_or(&[], Vec::as_slice)
    }
}

/// Metres — how close the host socket must come to the cursor for a discrete
/// snap to win over the surface / ground fallbacks. The radius used when the
/// caller has no camera to aim with; see [`Aim`] for the one it uses when it
/// does.
pub const ATTACH_THRESHOLD: f64 = 0.5;

/// Metres — how far the cursor must travel from a joint the builder has
/// *already* latched onto before that joint lets go.
///
/// The pair with [`ATTACH_THRESHOLD`] is hysteresis, and the two numbers are
/// written together because only their ordering matters: strictly larger than
/// the snap-in radius, or a held piece sitting exactly on the boundary chatters
/// between snapped and free once per pointer sample. It is not read by
/// [`solve_snap`] — the search has no memory of what was latched last frame, and
/// giving it one would make the same input answer two different ways. The
/// caller holding the latch is the caller that can apply it.
pub const DETACH_THRESHOLD: f64 = 0.8;

/// Pixels — how near the pointer a host socket must project to take hold.
///
/// The screen-space twin of [`ATTACH_THRESHOLD`], and the production number:
/// pointing is done in pixels, so the radius that decides is in pixels too.
pub const ATTACH_PX: f64 = 24.0;

/// Pixels — how far the pointer must travel from a latched joint before it
/// lets go. The screen-space twin of [`DETACH_THRESHOLD`], in the same ratio.
pub const DETACH_PX: f64 = 38.0;

/// Pixels — two host sockets this close to each other on screen are the same
/// aim, and depth decides between them. Without it the winner of a pair a
/// pixel apart is arithmetic noise, which is how a truss bolted itself to the
/// deck corner *behind* the one under the cursor.
const DEPTH_TIE_PX: f64 = 4.0;

/// Edge-mode requires the two sockets to sit on **opposing sides** of their
/// pieces, so the pieces end up next to each other rather than overlapping. We
/// compare their `outward` vectors in piece-local space — with edge mode's
/// identity relative rotation the two pieces share orientation, so opposing in
/// piece-local ⇔ opposing in world.
const EDGE_OUTWARD_THRESHOLD: f64 = -0.3;

/// |dot| above which two same-type face-mode sockets count as having parallel
/// normals, which triggers the opposite-side check in [`solve_snap`].
const PARALLEL_NORMAL_THRESHOLD: f64 = 0.9;

/// Below this squared length the user's extra twist is nothing, and rotating
/// by it would only add float noise.
const TWIST_EPSILON: f64 = 1e-8;

/// Above this |dot| with [`WORLD_UP`], a surface normal is parallel to up and
/// the derived tangent falls back to +X.
const PERPENDICULAR_UP_PARALLEL: f64 = 0.99;

/// The world up axis, as the *socket layer* sees it: +Y, because sockets are
/// authored in asset space and glTF is Y-up (see the crate docs). This is the
/// solver's only world-frame assumption — the ground plane and
/// `derive_perpendicular` are the whole of it. Moving the socket layer into
/// the renderer's Z-up frame means flipping this constant, converting at the
/// scene boundary, and re-recording `harness/goldens/stage-snap.json`.
pub const WORLD_UP: DVec3 = DVec3::Y;

/// Synthetic host id for a snap onto a surface with no owning piece.
const SURFACE_ID: &str = "__surface__";
/// Synthetic host id for the implicit ground plane.
const GROUND_ID: &str = "__ground__";

/// Where the pointer is, and how a world point lands next to it.
///
/// Which socket a gesture *means* is an aiming question, so it is answered in
/// pixels: two deck corners 0.7 m apart can be one pixel apart edge-on, and
/// scoring them in metres let iteration order rather than the operator pick
/// between them. Only the **choice** looks at the screen — the mate itself is
/// still [`crate::venue::place_on`] in world space, so nothing about where a
/// piece ends up depends on the camera.
///
/// The projection is a closure rather than a camera because the solver has no
/// business knowing what a camera is: the caller already has one, and handing
/// in a function keeps this crate free of the renderer's frames.
pub struct Aim<'a> {
    /// The pointer, in the pixel space `project` answers in.
    pub cursor: DVec2,
    /// A world point as `(x, y, depth)` in pixels and metres-along-the-view,
    /// or `None` for a point behind the eye — which cannot be aimed at.
    pub project: &'a dyn Fn(DVec3) -> Option<DVec3>,
}

impl Aim<'_> {
    /// How far a world point falls from the pointer, and how far along the
    /// view it is. `None` when it cannot be aimed at at all.
    #[must_use]
    pub fn reach(&self, at: DVec3) -> Option<(f64, f64)> {
        let p = (self.project)(at)?;
        Some((self.cursor.distance(p.truncate()), p.z))
    }
}

/// How near the pointer a host socket must come to take hold, in whichever
/// space `aim` measures — pixels with a camera, metres without.
#[must_use]
pub fn attach_radius(aim: Option<&Aim<'_>>) -> f64 {
    if aim.is_some() {
        ATTACH_PX
    } else {
        ATTACH_THRESHOLD
    }
}

/// How far the pointer must leave a latched joint before it lets go, in the
/// same space [`attach_radius`] measures. Strictly the larger of the two — see
/// [`DETACH_THRESHOLD`] for why the pair exists at all.
#[must_use]
pub fn detach_radius(aim: Option<&Aim<'_>>) -> f64 {
    if aim.is_some() {
        DETACH_PX
    } else {
        DETACH_THRESHOLD
    }
}

#[derive(Clone, Debug)]
pub struct ScenePiece {
    pub id: String,
    pub mesh_path: String,
    pub world_matrix: DMat4,
}

/// Continuous surface under the cursor (e.g. the top of a deck). Treated as a
/// virtual host socket at the cursor's hit point, with the hit piece as
/// parent. Used as a "scatter on top" fallback when no discrete socket is
/// close enough.
///
/// The raycast that produces this is deliberately *outside* the solver: the
/// caller hands in the hit, which is what keeps the solver pure math.
#[derive(Clone, Debug)]
pub struct SnapSurface {
    pub piece_id: Option<String>,
    pub host_matrix: DMat4,
    pub local_point: DVec3,
    pub local_normal: DVec3,
    /// The socket type the surface represents — drives compatibility.
    pub surface_type: SocketType,
}

pub struct SnapInput<'a, L: SocketLookup + ?Sized> {
    pub held_mesh_path: &'a str,
    pub cursor_world: DVec3,
    pub current_quaternion: Option<DQuat>,
    pub pieces: &'a [ScenePiece],
    /// Exclude this piece (e.g. the one being dragged) from snap targets.
    pub exclude_id: Option<&'a str>,
    pub shift_held: bool,
    /// Optional surface under the cursor (deck top, etc.).
    pub surface: Option<&'a SnapSurface>,
    /// How to measure "near the cursor" when choosing between host sockets.
    /// `Some` whenever the caller has a camera — which in the builder is
    /// always. `None` scores in world metres, which is what the port goldens
    /// pin.
    pub aim: Option<&'a Aim<'a>>,
    pub lookup_sockets: &'a L,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapMatch {
    pub held_socket: String,
    pub host_socket: String,
    /// `None` for ground / free placement.
    pub host_id: Option<String>,
    pub host_type: SocketType,
}

#[derive(Clone, Debug)]
pub struct SnapResult {
    pub position: DVec3,
    pub quaternion: DQuat,
    pub parent_id: Option<String>,
    pub matched: Option<SnapMatch>,
    /// How far the winning host socket fell from the cursor, in the space
    /// [`SnapInput::aim`] measures — pixels with an [`Aim`], metres without.
    /// `f64::INFINITY` for free placement.
    pub score: f64,
}

// ---------------------------------------------------------------------------
// three.js semantics
// ---------------------------------------------------------------------------

/// three.js `Vector3.normalize()`: zero-length stays zero rather than NaN.
fn normalize(v: DVec3) -> DVec3 {
    v.normalize_or_zero()
}

/// three.js `Vector3.applyMatrix4()`, including the perspective divide.
fn apply_matrix4(v: DVec3, m: &DMat4) -> DVec3 {
    let p = *m * v.extend(1.0);
    p.truncate() / p.w
}

/// three.js `Vector3.transformDirection()`: upper-3×3 multiply, then normalize.
fn transform_direction(v: DVec3, m: &DMat4) -> DVec3 {
    normalize(DMat3::from_mat4(*m) * v)
}

/// three.js `Matrix4.decompose()` → `(translation, rotation, scale)`.
fn decompose(m: &DMat4) -> (DVec3, DQuat, DVec3) {
    let (scale, rotation, translation) = m.to_scale_rotation_translation();
    (translation, rotation, scale)
}

// ---------------------------------------------------------------------------
// Implicit ground host (re-positioned to the cursor each call)
// ---------------------------------------------------------------------------

fn make_ground_piece(cursor: DVec3) -> ScenePiece {
    ScenePiece {
        id: GROUND_ID.to_string(),
        mesh_path: GROUND_ID.to_string(),
        world_matrix: DMat4::from_translation(cursor.reject_from(WORLD_UP)),
    }
}

fn ground_socket() -> ResolvedSocket {
    ResolvedSocket {
        name: "ground".to_string(),
        socket_type: SocketType::Ground,
        position: DVec3::ZERO,
        normal: WORLD_UP,
        tangent: DVec3::X,
        mode: SocketMode::Face,
        outward: WORLD_UP,
        roll: SocketType::Ground.roll(),
    }
}

fn derive_perpendicular(normal: DVec3) -> DVec3 {
    let candidate = if normal.dot(WORLD_UP).abs() < PERPENDICULAR_UP_PARALLEL {
        WORLD_UP
    } else {
        DVec3::X
    };
    normalize(normal.cross(candidate))
}

// ---------------------------------------------------------------------------
// Candidate evaluation
// ---------------------------------------------------------------------------

struct Candidate {
    matrix: DMat4,
    score: f64,
    /// Metres along the view to the host socket — `Some` only under an
    /// [`Aim`], because it is only a tiebreak between two sockets the pointer
    /// is equally on.
    depth: Option<f64>,
    matched: SnapMatch,
}

impl Candidate {
    /// Whether this is the one the operator is pointing at, against the best
    /// so far. Nearer the pointer wins; two within [`DEPTH_TIE_PX`] are the
    /// same aim, and then the nearer one along the view wins — so a socket on
    /// the far side of a deck cannot be latched through it.
    fn beats(&self, best: &Candidate) -> bool {
        match (self.depth, best.depth) {
            (Some(mine), Some(theirs)) if (self.score - best.score).abs() <= DEPTH_TIE_PX => {
                mine < theirs
            }
            _ => self.score < best.score,
        }
    }
}

/// Replace the snap pose's rotation **around the shared normal axis** with the
/// user's current rotation around that same axis, keeping the snap's
/// orientation on the other two axes so the mount stays aligned with the host
/// surface. The pivot is the host socket's world position — where the two
/// sockets meet — so the position constraint stays satisfied.
///
/// Without this, a CDJ rotated 45° on a deck and then nudged sideways snaps
/// back to the solver's canonical rotation, undoing the user's free spin
/// around the vertical.
fn preserve_twist(
    matrix: &mut DMat4,
    current_quaternion: DQuat,
    host_socket: &ResolvedSocket,
    host_world_matrix: &DMat4,
) {
    let (snap_pos, snap_q, snap_scale) = decompose(matrix);

    // Shared normal in world — the axis we want to preserve rotation around.
    let shared_normal = normalize(transform_direction(host_socket.normal, host_world_matrix));

    // Rotation from snap to current: what the user applied "extra".
    let rel = current_quaternion * snap_q.conjugate();

    // Keep only the twist component around the shared normal.
    let v = DVec3::new(rel.x, rel.y, rel.z);
    let proj = shared_normal * v.dot(shared_normal);
    let rel_twist = DQuat::from_xyzw(proj.x, proj.y, proj.z, rel.w);
    if rel_twist.length_squared() < TWIST_EPSILON {
        return;
    }
    let rel_twist = rel_twist.normalize();

    let pivot = apply_matrix4(host_socket.position, host_world_matrix);
    let new_pos = pivot + rel_twist * (snap_pos - pivot);
    let new_q = rel_twist * snap_q;

    *matrix = DMat4::from_scale_rotation_translation(snap_scale, new_q, new_pos);
}

fn evaluate_candidate(
    held_socket: &ResolvedSocket,
    host_socket: &ResolvedSocket,
    host: &ScenePiece,
    cursor_world: DVec3,
    aim: Option<&Aim<'_>>,
    held_grab: Option<&ResolvedSocket>,
    current_quaternion: Option<DQuat>,
) -> Option<Candidate> {
    // The mate itself is `venue::place_on` with nothing added: same socket
    // frames, same face/edge flip, one copy. What this solver contributes is
    // the *search* — which pair, scored against the cursor — not the
    // arithmetic.
    let mut matrix = place_on(
        host.world_matrix,
        host_socket,
        held_socket,
        NodeKind::Piece,
        SurfacePlacement::FLUSH,
    );

    // Face-mode snaps preserve the user's rotation around the shared normal.
    // (Edge mode's relative rotation is identity — no extra freedom.)
    if held_socket.mode == SocketMode::Face {
        if let Some(q) = current_quaternion {
            preserve_twist(&mut matrix, q, host_socket, &host.world_matrix);
        }
    }

    let host_world = apply_matrix4(host_socket.position, &host.world_matrix);
    // Under an aim the score is the one thing the gesture actually said: how
    // far the host socket is from the pointer, on screen. The held-grab term
    // below is deliberately absent here — it exists to rescue a *drag*, and it
    // let a socket the operator was nowhere near win because the piece would
    // happen to hang near the cursor once mated.
    let (score, depth) = match aim {
        Some(aim) => {
            let (distance, depth) = aim.reach(host_world)?;
            (distance, Some(depth))
        }
        None => {
            // Hybrid score = min(host_socket_to_cursor,
            // held_grab_after_snap_to_cursor).
            //
            // Two intuitions for placement give two different cursor targets:
            //   - "click ON the snap target" (corner, edge): the host socket's
            //     world position is near the cursor;
            //   - "drag a piece into its natural snap pose": the held grab
            //     socket, after the snap, is near the cursor.
            // Taking the min lets the solver accept either path. Without the
            // held-grab term, dragging a tall piece (a truss extending up from
            // a corner) never re-snaps, because its centroid is always too
            // high.
            let mut score = host_world.distance(cursor_world);
            if let Some(grab) = held_grab {
                let held_grab_world = apply_matrix4(grab.position, &matrix);
                score = score.min(held_grab_world.distance(cursor_world));
            }
            (score, None)
        }
    };

    Some(Candidate {
        matrix,
        score,
        depth,
        matched: SnapMatch {
            held_socket: held_socket.name.clone(),
            host_socket: host_socket.name.clone(),
            host_id: (host.id != GROUND_ID).then(|| host.id.clone()),
            host_type: host_socket.socket_type,
        },
    })
}

fn free_placement(
    current_quaternion: Option<DQuat>,
    cursor_world: DVec3,
    held_grab: Option<&ResolvedSocket>,
) -> SnapResult {
    let q = current_quaternion.unwrap_or(DQuat::IDENTITY);
    let mut position = cursor_world;
    if let Some(grab) = held_grab {
        // Offset so the grab socket lands on the cursor.
        position -= q * grab.position;
    }
    SnapResult {
        position,
        quaternion: q,
        parent_id: None,
        matched: None,
        score: f64::INFINITY,
    }
}

fn result_from(candidate: &Candidate, parent_id: Option<String>) -> SnapResult {
    let (position, quaternion, _) = decompose(&candidate.matrix);
    SnapResult {
        position,
        quaternion,
        parent_id,
        matched: Some(candidate.matched.clone()),
        score: candidate.score,
    }
}

// ---------------------------------------------------------------------------
// Main solver
// ---------------------------------------------------------------------------

pub fn solve_snap<L: SocketLookup + ?Sized>(input: &SnapInput<'_, L>) -> SnapResult {
    let held_sockets = input.lookup_sockets.sockets(input.held_mesh_path);
    let held_grab = held_sockets
        .iter()
        .find(|s| s.socket_type == SocketType::Grab);

    if input.shift_held {
        return free_placement(input.current_quaternion, input.cursor_world, held_grab);
    }

    let useful_held: Vec<&ResolvedSocket> = held_sockets
        .iter()
        .filter(|s| s.socket_type.polarity().can_be_held())
        .collect();

    // Pass 1: non-ground hosts. Best within ATTACH_THRESHOLD wins.
    let mut best_piece: Option<Candidate> = None;
    for held_socket in &useful_held {
        for host in input.pieces {
            if Some(host.id.as_str()) == input.exclude_id {
                continue;
            }
            for host_socket in input.lookup_sockets.sockets(&host.mesh_path) {
                if !held_socket.socket_type.mates(host_socket.socket_type) {
                    continue;
                }
                // Edge mode requires opposing-side sockets, else the pieces
                // stack on top of each other.
                if held_socket.mode == SocketMode::Edge
                    && held_socket.outward.dot(host_socket.outward) > EDGE_OUTWARD_THRESHOLD
                {
                    continue;
                }
                // Face mode between two same-type sockets with parallel
                // normals (self-mating: truss-to-truss, rail-to-rail,
                // cable-to-cable, corner-to-corner stack): require the two
                // sockets to sit on opposite sides along the shared normal
                // axis. Otherwise the 180°-about-tangent flip puts the held
                // piece upside down at an identical score to the correct pose.
                // Perpendicular-normal pairings (a horizontal truss on a
                // vertical box face) are unaffected — only the parallel-normal
                // case is buggy.
                if held_socket.mode == SocketMode::Face
                    && held_socket.socket_type == host_socket.socket_type
                    && held_socket.normal.dot(host_socket.normal).abs() > PARALLEL_NORMAL_THRESHOLD
                {
                    let axis = host_socket.normal;
                    if held_socket.outward.dot(axis) * host_socket.outward.dot(axis) >= 0.0 {
                        continue;
                    }
                }
                let Some(cand) = evaluate_candidate(
                    held_socket,
                    host_socket,
                    host,
                    input.cursor_world,
                    input.aim,
                    held_grab,
                    input.current_quaternion,
                ) else {
                    // Behind the eye: not a socket this gesture can mean.
                    continue;
                };
                if best_piece.as_ref().is_none_or(|b| cand.beats(b)) {
                    best_piece = Some(cand);
                }
            }
        }
    }

    if let Some(best) = best_piece.filter(|b| b.score <= attach_radius(input.aim)) {
        let parent = best.matched.host_id.clone();
        return result_from(&best, parent);
    }

    // Pass 2: surface fallback. If the cursor is over a deck top (or
    // equivalent), a virtual socket at the hit point lands the held piece's
    // mount at the cursor with the surface piece as parent.
    if let Some(surface) = input.surface {
        let surf_host = ScenePiece {
            id: surface
                .piece_id
                .clone()
                .unwrap_or_else(|| SURFACE_ID.to_string()),
            mesh_path: SURFACE_ID.to_string(),
            world_matrix: surface.host_matrix,
        };
        let surf_socket = ResolvedSocket {
            name: "surface".to_string(),
            socket_type: surface.surface_type,
            position: surface.local_point,
            normal: surface.local_normal,
            tangent: derive_perpendicular(surface.local_normal),
            mode: SocketMode::Face,
            outward: surface.local_normal,
            roll: surface.surface_type.roll(),
        };
        let mut best_surf: Option<Candidate> = None;
        for held_socket in &useful_held {
            if !held_socket.socket_type.mates(surface.surface_type) {
                continue;
            }
            // World-scored on purpose: a surface has no named point to aim
            // at, so "nearest to the pointer" is the mate's own distance.
            let Some(cand) = evaluate_candidate(
                held_socket,
                &surf_socket,
                &surf_host,
                input.cursor_world,
                None,
                held_grab,
                input.current_quaternion,
            ) else {
                continue;
            };
            if best_surf.as_ref().is_none_or(|b| cand.score < b.score) {
                best_surf = Some(cand);
            }
        }
        if let Some(best) = best_surf {
            return result_from(&best, surface.piece_id.clone());
        }
    }

    // Pass 3: ground fallback — the WORLD_UP=0 plane, no parent. Used when
    // nothing else (discrete or surface) accepted the held piece.
    let ground_host = make_ground_piece(input.cursor_world);
    let ground = ground_socket();
    for held_socket in &useful_held {
        if !held_socket.socket_type.mates(SocketType::Ground) {
            continue;
        }
        let Some(cand) = evaluate_candidate(
            held_socket,
            &ground,
            &ground_host,
            input.cursor_world,
            None,
            held_grab,
            input.current_quaternion,
        ) else {
            continue;
        };
        return result_from(&cand, None);
    }

    free_placement(input.current_quaternion, input.cursor_world, held_grab)
}

/// Convert a world transform on the held piece into a parent-local pose, for
/// persistence. `parent_world = None` returns the world pose unchanged (a
/// detached piece).
pub fn world_to_parent_local(world_matrix: &DMat4, parent_world: Option<&DMat4>) -> (DVec3, DQuat) {
    let m = match parent_world {
        None => *world_matrix,
        Some(p) => p.inverse() * *world_matrix,
    };
    let (position, quaternion, _) = decompose(&m);
    (position, quaternion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_piece_sits_on_the_up_plane() {
        let p = make_ground_piece(DVec3::new(3.0, 7.5, -2.0));
        let origin = apply_matrix4(DVec3::ZERO, &p.world_matrix);
        assert_eq!(origin, DVec3::new(3.0, 0.0, -2.0));
    }

    /// A host with one socket per face, at `±1` on each axis — the shape a
    /// generated corner block has (`luma_render::catalog::procedural_sockets`
    /// names one `face_*` socket per *open* face), which is what makes "point
    /// at the face you want" expressible at all.
    fn faced_host() -> (Vec<ScenePiece>, HashMap<String, Vec<ResolvedSocket>>) {
        let face = |name: &str, dir: DVec3| ResolvedSocket {
            name: name.to_string(),
            socket_type: SocketType::TrussEnd,
            position: dir,
            normal: dir,
            tangent: DVec3::Y.cross(dir).normalize_or(DVec3::X),
            mode: SocketMode::Face,
            outward: dir,
            roll: SocketType::TrussEnd.roll(),
        };
        let held = ResolvedSocket {
            name: "end_a".to_string(),
            ..face("end_a", DVec3::NEG_X)
        };
        let mut lookup = HashMap::new();
        lookup.insert(
            "host".to_string(),
            vec![
                face("face_+x", DVec3::X),
                face("face_-x", DVec3::NEG_X),
                face("face_+z", DVec3::Z),
                face("face_-z", DVec3::NEG_Z),
            ],
        );
        lookup.insert("held".to_string(), vec![held]);
        (
            vec![ScenePiece {
                id: "host".to_string(),
                mesh_path: "host".to_string(),
                world_matrix: DMat4::IDENTITY,
            }],
            lookup,
        )
    }

    /// Which face a gesture means is the face it is *pointing at*: the same
    /// cursor, two aims, two different faces chosen — and the world cursor
    /// (equidistant from both) never moves.
    #[test]
    fn the_face_under_the_pointer_is_the_one_chosen() {
        let (pieces, lookup) = faced_host();
        // A camera that spreads the four faces across the screen at one depth.
        let project = |at: DVec3| Some(DVec3::new(100.0 + at.x * 50.0 + at.z * 30.0, 100.0, 10.0));
        let choose = |cursor_px: DVec2| {
            let aim = Aim {
                cursor: cursor_px,
                project: &project,
            };
            solve_snap(&SnapInput {
                held_mesh_path: "held",
                cursor_world: DVec3::ZERO,
                current_quaternion: None,
                pieces: &pieces,
                exclude_id: None,
                shift_held: false,
                surface: None,
                aim: Some(&aim),
                lookup_sockets: &lookup,
            })
            .matched
            .map(|m| m.host_socket)
        };
        assert_eq!(choose(DVec2::new(150.0, 100.0)).as_deref(), Some("face_+x"));
        assert_eq!(choose(DVec2::new(130.0, 100.0)).as_deref(), Some("face_+z"));
        assert_eq!(choose(DVec2::new(70.0, 100.0)).as_deref(), Some("face_-z"));
    }

    /// Two faces on one pixel — the block seen edge-on — are the same aim, and
    /// then the near one wins. Without the depth tiebreak the winner is
    /// whichever the iteration order reached first, which is how a piece
    /// bolted itself to the far side of the thing under the cursor.
    #[test]
    fn the_nearer_of_two_faces_on_one_pixel_wins() {
        let (pieces, lookup) = faced_host();
        // Edge-on: every face lands on the same pixel, and only depth differs.
        let project = |at: DVec3| Some(DVec3::new(100.0, 100.0, 10.0 - at.z));
        let aim = Aim {
            cursor: DVec2::new(100.0, 100.0),
            project: &project,
        };
        let matched = solve_snap(&SnapInput {
            held_mesh_path: "held",
            cursor_world: DVec3::ZERO,
            current_quaternion: None,
            pieces: &pieces,
            exclude_id: None,
            shift_held: false,
            surface: None,
            aim: Some(&aim),
            lookup_sockets: &lookup,
        })
        .matched
        .map(|m| m.host_socket);
        // +z is nearest the eye (depth 10 - 1), so it is the one aimed at.
        assert_eq!(matched.as_deref(), Some("face_+z"));
    }

    #[test]
    fn world_to_parent_local_undoes_the_parent() {
        let parent =
            DMat4::from_rotation_y(0.7) * DMat4::from_translation(DVec3::new(1.0, 2.0, 3.0));
        let local = DMat4::from_translation(DVec3::new(0.0, 0.5, 0.0));
        let (p, q) = world_to_parent_local(&(parent * local), Some(&parent));
        assert!(p.abs_diff_eq(DVec3::new(0.0, 0.5, 0.0), 1e-12));
        assert!(q.abs_diff_eq(DQuat::IDENTITY, 1e-12));
    }
}
