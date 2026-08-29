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
//! 2. Score the candidate by how close the host socket's world position is to
//!    the cursor target. This is what actually drives "snap when the user
//!    clicks near a corner" — not where the held piece's centroid lands.
//!
//! Non-ground hosts are evaluated first; if the best score is within
//! [`ATTACH_THRESHOLD`], the snap is applied and the held piece's parent
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
use glam::{DMat3, DMat4, DQuat, DVec3};
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
/// snap to win over the surface / ground fallbacks.
pub const ATTACH_THRESHOLD: f64 = 0.5;

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
    /// Distance in metres; `f64::INFINITY` for free placement.
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
    matched: SnapMatch,
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
    held_grab: Option<&ResolvedSocket>,
    current_quaternion: Option<DQuat>,
) -> Candidate {
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

    // Hybrid score = min(host_socket_to_cursor, held_grab_after_snap_to_cursor).
    //
    // Two intuitions for placement give two different cursor targets:
    //   - "click ON the snap target" (corner, edge): the host socket's world
    //     position is near the cursor;
    //   - "drag a piece into its natural snap pose": the held grab socket,
    //     after the snap, is near the cursor.
    // Taking the min lets the solver accept either path. Without the held-grab
    // term, dragging a tall piece (a truss extending up from a corner) never
    // re-snaps, because its centroid is always too high.
    let host_world = apply_matrix4(host_socket.position, &host.world_matrix);
    let mut score = host_world.distance(cursor_world);
    if let Some(grab) = held_grab {
        let held_grab_world = apply_matrix4(grab.position, &matrix);
        score = score.min(held_grab_world.distance(cursor_world));
    }

    Candidate {
        matrix,
        score,
        matched: SnapMatch {
            held_socket: held_socket.name.clone(),
            host_socket: host_socket.name.clone(),
            host_id: (host.id != GROUND_ID).then(|| host.id.clone()),
            host_type: host_socket.socket_type,
        },
    }
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
                let cand = evaluate_candidate(
                    held_socket,
                    host_socket,
                    host,
                    input.cursor_world,
                    held_grab,
                    input.current_quaternion,
                );
                if best_piece.as_ref().is_none_or(|b| cand.score < b.score) {
                    best_piece = Some(cand);
                }
            }
        }
    }

    if let Some(best) = best_piece.filter(|b| b.score <= ATTACH_THRESHOLD) {
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
            let cand = evaluate_candidate(
                held_socket,
                &surf_socket,
                &surf_host,
                input.cursor_world,
                held_grab,
                input.current_quaternion,
            );
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
        let cand = evaluate_candidate(
            held_socket,
            &ground,
            &ground_host,
            input.cursor_world,
            held_grab,
            input.current_quaternion,
        );
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
