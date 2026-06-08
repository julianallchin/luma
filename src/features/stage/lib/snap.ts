/**
 * Socket-based snap solver.
 *
 * Held piece (the one following the cursor) has a `grab` socket that the
 * cursor pulls toward a target world point. The solver iterates every
 * "useful" socket on the held piece against every compatible socket on
 * every other piece in the scene.
 *
 * For each candidate (held socket Sh, host socket Sho on host piece P_h):
 *   1. Build local-space frames for Sh and Sho (normal = +Z, tangent = +X).
 *   2. Compute the held piece's world transform that puts Sh on top of
 *      Sho, with the relative orientation chosen by the socket `mode`:
 *
 *        face mode → 180° flip about the host socket's X (tangent) axis
 *                   so normals oppose (face-to-face).
 *        edge mode → 180° flip about the host socket's Z (normal) axis
 *                   so normals stay parallel but tangents reverse
 *                   (side-by-side along a shared edge).
 *
 *   3. Score the candidate by how close the host socket's world position
 *      is to the cursor target. This is what actually drives "snap when
 *      the user clicks near a corner" — not where the held piece's
 *      centroid lands.
 *
 * Non-ground hosts are evaluated first; if the best score is within
 * {@link ATTACH_THRESHOLD}, the snap is applied and the held piece's
 * parent becomes P_h. Otherwise, ground is offered as a fallback (held
 * mount socket on the cursor's ground projection). Otherwise, free
 * placement.
 */

import { Matrix4, Quaternion, Vector3 } from "three";
import {
	COMPATIBLE,
	type ResolvedSocket,
	type SocketMode,
	type SocketType,
} from "./sockets";

/**
 * Looks up the resolved sockets for a given mesh path. In production this
 * is backed by `mesh-cache.getMeshSockets`; in tests it can be a static
 * Map so the solver is fully deterministic without GLB loading.
 */
export type SocketLookup = (meshPath: string) => ResolvedSocket[];

export const ATTACH_THRESHOLD = 0.5; // metres — host socket distance to cursor

export interface ScenePiece {
	id: string;
	meshPath: string;
	worldMatrix: Matrix4;
}

/**
 * Continuous surface under the cursor (e.g. the top of a deck). Treated as
 * a virtual host socket at the cursor's hit point, parent set to the
 * piece that was hit. Used as a "scatter on top" fallback when no
 * discrete socket is close enough.
 */
export interface SnapSurface {
	pieceId: string | null;
	hostMatrix: Matrix4;
	localPoint: Vector3;
	localNormal: Vector3;
	/** The socket type the surface represents — drives compat (e.g. `"floor_top"`). */
	type: SocketType;
}

export interface SnapInput {
	heldMeshPath: string;
	cursorWorld: Vector3;
	currentQuaternion?: Quaternion;
	pieces: ScenePiece[];
	/** Exclude this piece (e.g. the one being dragged) from snap targets. */
	excludeId?: string;
	shiftHeld: boolean;
	/** Optional surface under the cursor (deck top, etc.). */
	surface?: SnapSurface;
	/** Sockets-by-mesh accessor. Defaults to the production mesh cache. */
	lookupSockets: SocketLookup;
}

export interface SnapMatch {
	heldSocket: string;
	hostSocket: string;
	hostId: string | null; // null for ground / free placement
	hostType: SocketType;
}

export interface SnapResult {
	position: Vector3;
	quaternion: Quaternion;
	parentId: string | null;
	match: SnapMatch | null;
	score: number;
}

// ---------------------------------------------------------------------------
// Socket frame helpers
// ---------------------------------------------------------------------------

function buildSocketFrame(socket: ResolvedSocket): Matrix4 {
	// Frame: origin at socket.position; +Z = normal; +X = tangent; +Y = Z×X.
	const z = socket.normal.clone().normalize();
	const x = socket.tangent.clone().normalize();
	x.sub(z.clone().multiplyScalar(z.dot(x))).normalize();
	const y = new Vector3().crossVectors(z, x).normalize();

	const m = new Matrix4();
	m.makeBasis(x, y, z);
	m.setPosition(socket.position);
	return m;
}

// Face mode: held piece is flipped 180° about the host socket's X (tangent)
// axis so the two normals oppose. Used for "speaker mount lands on stand
// top", "truss endpoint stands up from a deck corner", etc.
const FLIP_X_180 = new Matrix4().makeRotationX(Math.PI);

// Edge mode: held piece copies the host's orientation exactly. Only a
// translation is needed to put the held socket on top of the host socket.
// Used for "deck side-by-side with deck": both decks upright, tangents
// running in the same direction along the shared edge.
const EDGE_IDENTITY = new Matrix4();

function flipFor(mode: SocketMode): Matrix4 {
	return mode === "edge" ? EDGE_IDENTITY : FLIP_X_180;
}

// ---------------------------------------------------------------------------
// Implicit ground host (re-positioned to cursor each call)
// ---------------------------------------------------------------------------

function makeGroundPiece(cursor: Vector3): ScenePiece {
	const m = new Matrix4().setPosition(cursor.x, 0, cursor.z);
	return { id: "__ground__", meshPath: "__ground__", worldMatrix: m };
}

const GROUND_SOCKET: ResolvedSocket = {
	name: "ground",
	type: "ground",
	position: new Vector3(0, 0, 0),
	normal: new Vector3(0, 1, 0),
	tangent: new Vector3(1, 0, 0),
	mode: "face",
	outward: new Vector3(0, 1, 0),
};

// ---------------------------------------------------------------------------
// Candidate evaluation
// ---------------------------------------------------------------------------

interface Candidate {
	matrix: Matrix4;
	score: number;
	match: SnapMatch;
}

/**
 * Edge-mode requires the two sockets to sit on **opposing sides** of their
 * pieces (so the pieces end up next to each other, not overlapping). We
 * compare their `outward` vectors (in piece-local) — for edge mode with
 * identity relative rotation, the two pieces share orientation, so
 * opposing in piece-local ⇔ opposing in world.
 */
const EDGE_OUTWARD_THRESHOLD = -0.3;

/**
 * Replace the snap pose's rotation **around the shared normal axis** with
 * the user's current rotation around that same axis, while keeping the
 * snap's orientation on the other two axes (so the mount stays aligned
 * with the host surface). Pivot is the host socket world position — the
 * point where the two sockets meet — so the position constraint stays
 * satisfied.
 *
 * Without this, a CDJ rotated 45° on a deck and then nudged sideways
 * snaps back to the solver's "canonical" rotation (typically identity),
 * undoing the user's free spin around the vertical.
 */
function preserveTwist(
	matrix: Matrix4,
	currentQuaternion: Quaternion,
	hostSocket: ResolvedSocket,
	hostWorldMatrix: Matrix4,
): void {
	const snapPos = new Vector3();
	const snapQ = new Quaternion();
	const snapScale = new Vector3();
	matrix.decompose(snapPos, snapQ, snapScale);

	// Shared normal in world (axis we want to preserve rotation around).
	const sharedNormal = hostSocket.normal
		.clone()
		.transformDirection(hostWorldMatrix)
		.normalize();

	// Rotation from snap to current (what the user has applied "extra").
	const rel = currentQuaternion.clone().multiply(snapQ.clone().invert());

	// Extract only the twist component around the shared normal.
	const v = new Vector3(rel.x, rel.y, rel.z);
	const proj = sharedNormal.clone().multiplyScalar(v.dot(sharedNormal));
	const relTwist = new Quaternion(proj.x, proj.y, proj.z, rel.w);
	if (relTwist.lengthSq() < 1e-8) return;
	relTwist.normalize();

	// Pivot at host socket world position.
	const pivot = hostSocket.position.clone().applyMatrix4(hostWorldMatrix);

	const offset = snapPos.clone().sub(pivot).applyQuaternion(relTwist);
	const newPos = pivot.add(offset);
	const newQ = relTwist.multiply(snapQ);

	matrix.compose(newPos, newQ, snapScale);
}

function evaluateCandidate(
	heldSocket: ResolvedSocket,
	hostSocket: ResolvedSocket,
	host: ScenePiece,
	cursorWorld: Vector3,
	heldGrab: ResolvedSocket | null,
	currentQuaternion?: Quaternion,
): Candidate {
	const heldLocal = buildSocketFrame(heldSocket);
	const hostLocal = buildSocketFrame(hostSocket);
	const flip = flipFor(heldSocket.mode);

	const heldLocalInv = heldLocal.clone().invert();
	const matrix = new Matrix4()
		.multiplyMatrices(host.worldMatrix, hostLocal)
		.multiply(flip)
		.multiply(heldLocalInv);

	// For face-mode snaps, preserve the user's rotation around the shared
	// normal axis. (Edge mode is identity-flip — no extra freedom.)
	if (heldSocket.mode === "face" && currentQuaternion) {
		preserveTwist(matrix, currentQuaternion, hostSocket, host.worldMatrix);
	}

	// Hybrid score = min(host_socket_to_cursor, held_grab_after_snap_to_cursor).
	//
	// Two intuitions for placement give us two different cursor targets:
	//   - "Click ON the snap target" (corner, edge): host socket world is
	//     near cursor.
	//   - "Drag a piece into its natural snap pose" (e.g., move a truss to
	//     where its centroid would land if attached): held grab world
	//     (after snap) is near cursor.
	// Using the min lets the solver accept either path. Without the
	// held-grab term, dragging a tall piece (truss extending up from a
	// corner) never re-snaps because its centroid is always too high.
	const hostWorld = hostSocket.position.clone().applyMatrix4(host.worldMatrix);
	const hostScore = hostWorld.distanceTo(cursorWorld);
	let score = hostScore;
	if (heldGrab) {
		const heldGrabWorld = heldGrab.position.clone().applyMatrix4(matrix);
		const heldScore = heldGrabWorld.distanceTo(cursorWorld);
		if (heldScore < score) score = heldScore;
	}

	return {
		matrix,
		score,
		match: {
			heldSocket: heldSocket.name,
			hostSocket: hostSocket.name,
			hostId: host.id === "__ground__" ? null : host.id,
			hostType: hostSocket.type,
		},
	};
}

function freePlacement(
	input: SnapInput,
	heldGrab: ResolvedSocket | null,
): SnapResult {
	const q = input.currentQuaternion?.clone() ?? new Quaternion();
	const position = input.cursorWorld.clone();
	if (heldGrab) {
		// Offset position so grab socket lands on cursor.
		const grabRotated = heldGrab.position.clone().applyQuaternion(q);
		position.sub(grabRotated);
	}
	return {
		position,
		quaternion: q,
		parentId: null,
		match: null,
		score: Number.POSITIVE_INFINITY,
	};
}

// ---------------------------------------------------------------------------
// Main solver
// ---------------------------------------------------------------------------

export function solveSnap(input: SnapInput): SnapResult {
	const heldSockets = input.lookupSockets(input.heldMeshPath);
	const heldGrab = heldSockets.find((s) => s.type === "grab") ?? null;

	if (input.shiftHeld) {
		return freePlacement(input, heldGrab);
	}

	const usefulHeld = heldSockets.filter(
		(s) => s.type !== "grab" && COMPATIBLE[s.type]?.length > 0,
	);

	// Pass 1: non-ground hosts. Pick the best within ATTACH_THRESHOLD.
	let bestPiece: Candidate | null = null;
	for (const heldSocket of usefulHeld) {
		const allowed = new Set(COMPATIBLE[heldSocket.type] ?? []);
		if (allowed.size === 0) continue;

		for (const host of input.pieces) {
			if (host.id === input.excludeId) continue;
			const hostSockets = input.lookupSockets(host.meshPath);
			if (hostSockets.length === 0) continue;
			for (const hostSocket of hostSockets) {
				if (!allowed.has(hostSocket.type)) continue;
				// Edge mode requires opposing-side sockets, else the pieces
				// stack on top of each other.
				if (
					heldSocket.mode === "edge" &&
					heldSocket.outward.dot(hostSocket.outward) > EDGE_OUTWARD_THRESHOLD
				) {
					continue;
				}
				// Face mode between two same-type sockets with parallel
				// normals (self-mating: truss-to-truss, rail-to-rail,
				// cable-to-cable, corner-to-corner stack): require the two
				// sockets to sit on opposite sides along the shared normal
				// axis. Otherwise the 180°-about-tangent flip puts the
				// held piece upside down at an identical score to the
				// correct pose. Perpendicular-normal pairings (e.g., a
				// horizontal truss attaching to a vertical box face) are
				// not affected — only the parallel-normal case is buggy.
				if (
					heldSocket.mode === "face" &&
					heldSocket.type === hostSocket.type &&
					Math.abs(heldSocket.normal.dot(hostSocket.normal)) > 0.9
				) {
					const axis = hostSocket.normal;
					const heldSide = heldSocket.outward.dot(axis);
					const hostSide = hostSocket.outward.dot(axis);
					if (heldSide * hostSide >= 0) continue;
				}
				const cand = evaluateCandidate(
					heldSocket,
					hostSocket,
					host,
					input.cursorWorld,
					heldGrab,
					input.currentQuaternion,
				);
				if (!bestPiece || cand.score < bestPiece.score) bestPiece = cand;
			}
		}
	}

	if (bestPiece && bestPiece.score <= ATTACH_THRESHOLD) {
		const position = new Vector3();
		const quaternion = new Quaternion();
		const scale = new Vector3();
		bestPiece.matrix.decompose(position, quaternion, scale);
		return {
			position,
			quaternion,
			parentId: bestPiece.match.hostId,
			match: bestPiece.match,
			score: bestPiece.score,
		};
	}

	// Pass 2: surface fallback. If the cursor is over a deck top (or
	// equivalent), generate a virtual socket at the hit point so the held
	// piece's *_mount lands at the cursor with the surface piece as parent.
	if (input.surface) {
		const surfHost: ScenePiece = {
			id: input.surface.pieceId ?? "__surface__",
			meshPath: "__surface__",
			worldMatrix: input.surface.hostMatrix,
		};
		const tangent = derivePerpendicular(input.surface.localNormal);
		const surfSocket: ResolvedSocket = {
			name: "surface",
			type: input.surface.type,
			position: input.surface.localPoint,
			normal: input.surface.localNormal,
			tangent,
			mode: "face",
			outward: input.surface.localNormal.clone(),
		};
		let bestSurf: Candidate | null = null;
		for (const heldSocket of usefulHeld) {
			const allowed = new Set(COMPATIBLE[heldSocket.type] ?? []);
			if (!allowed.has(input.surface.type)) continue;
			const cand = evaluateCandidate(
				heldSocket,
				surfSocket,
				surfHost,
				input.cursorWorld,
				heldGrab,
				input.currentQuaternion,
			);
			if (!bestSurf || cand.score < bestSurf.score) bestSurf = cand;
		}
		if (bestSurf) {
			const position = new Vector3();
			const quaternion = new Quaternion();
			const scale = new Vector3();
			bestSurf.matrix.decompose(position, quaternion, scale);
			return {
				position,
				quaternion,
				parentId: input.surface.pieceId,
				match: bestSurf.match,
				score: bestSurf.score,
			};
		}
	}

	// Pass 3: ground fallback. The "ground" host is the Y=0 plane with no
	// parent — used when nothing else (discrete or surface) accepted the
	// held piece.
	const groundHost = makeGroundPiece(input.cursorWorld);
	for (const heldSocket of usefulHeld) {
		const allowed = new Set(COMPATIBLE[heldSocket.type] ?? []);
		if (!allowed.has("ground")) continue;
		const cand = evaluateCandidate(
			heldSocket,
			GROUND_SOCKET,
			groundHost,
			input.cursorWorld,
			heldGrab,
			input.currentQuaternion,
		);
		const position = new Vector3();
		const quaternion = new Quaternion();
		const scale = new Vector3();
		cand.matrix.decompose(position, quaternion, scale);
		return {
			position,
			quaternion,
			parentId: null,
			match: cand.match,
			score: cand.score,
		};
	}

	return freePlacement(input, heldGrab);
}

function derivePerpendicular(normal: Vector3): Vector3 {
	const up = new Vector3(0, 1, 0);
	const candidate = Math.abs(normal.dot(up)) < 0.99 ? up : new Vector3(1, 0, 0);
	return new Vector3().crossVectors(normal, candidate).normalize();
}

/**
 * Convert a world transform on the held piece into a parent-local pose,
 * for persistence. Pass `parentWorld = null` to return the world pose
 * unchanged (i.e. detached piece).
 */
export function worldToParentLocal(
	worldMatrix: Matrix4,
	parentWorld: Matrix4 | null,
): { position: Vector3; quaternion: Quaternion } {
	const m =
		parentWorld === null
			? worldMatrix
			: new Matrix4().multiplyMatrices(
					parentWorld.clone().invert(),
					worldMatrix,
				);
	const position = new Vector3();
	const quaternion = new Quaternion();
	const scale = new Vector3();
	m.decompose(position, quaternion, scale);
	return { position, quaternion };
}
