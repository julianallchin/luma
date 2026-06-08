/**
 * Socket model for the stage builder.
 *
 * Each mesh declares a set of named anchor points in its **local three.js
 * frame** (Y-up). Sockets carry a type tag and a normal vector; matching
 * sockets magnetize together during placement and drag.
 *
 * Authoring is done relative to the mesh's **bounding box** (measured at
 * load time via `Box3.setFromObject`) rather than raw local coordinates,
 * so socket positions stay correct regardless of where the modeler placed
 * the GLB pivot.
 */

import type { Box3, Vector3 as Vec3 } from "three";
import { Vector3 } from "three";

// ---------------------------------------------------------------------------
// Socket types + compatibility
// ---------------------------------------------------------------------------

export type SocketType =
	| "grab" // placement reference (cursor follows this)
	| "floor_top" // top surface of a deck (host)
	| "floor_edge" // mid-edge of deck top
	| "floor_corner" // corner of deck top, inset by truss radius
	| "truss_end" // end of any truss
	| "stand_top" // top of a speaker stand
	| "stand_bottom" // bottom of a speaker stand
	| "speaker_mount" // bottom of a speaker
	| "equipment_mount" // bottom of CDJ / mixer / cable cover
	| "bottom_mount" // bottom of any "sits on a flat surface" piece (deck, guardrail, ...)
	| "rail_end" // end of guardrail
	| "cable_end" // end of a cable cover (chains end-to-end)
	| "ground"; // implicit Y=0 plane (virtual host)

/**
 * Held-side -> list of host-side socket types it can attach to.
 *
 * Snapping is asymmetric in design (the held piece is moving, the host is
 * stationary). For socket types that snap together symmetrically (truss
 * ends, floor edges, rail ends), both pieces list the other in their
 * compatibility — the solver iterates held-piece sockets and looks for
 * matches on hosts, so the symmetric case still works.
 */
export const COMPATIBLE: Record<SocketType, SocketType[]> = {
	grab: [],
	floor_top: [],
	floor_edge: ["floor_edge"],
	floor_corner: [],
	truss_end: ["truss_end", "floor_corner"],
	stand_top: [],
	stand_bottom: ["floor_top", "ground"],
	speaker_mount: ["stand_top", "floor_top", "ground"],
	equipment_mount: ["floor_top", "ground"],
	bottom_mount: ["floor_top", "ground"],
	rail_end: ["rail_end", "floor_edge"],
	cable_end: ["cable_end"],
	ground: [],
};

/**
 * Debug colors used by the socket overlay. Keep distinct so different
 * socket types are visually separable when stacked.
 */
export const SOCKET_COLOR: Record<SocketType, string> = {
	grab: "#facc15", // yellow (debug only)
	floor_top: "#60a5fa", // light blue
	floor_edge: "#22d3ee", // cyan
	floor_corner: "#3b82f6", // blue
	truss_end: "#f0abfc", // magenta
	stand_top: "#34d399", // green
	stand_bottom: "#10b981", // dark green
	speaker_mount: "#f87171", // red
	equipment_mount: "#fb923c", // orange
	bottom_mount: "#a3e635", // lime
	rail_end: "#c084fc", // purple
	cable_end: "#ec4899", // pink
	ground: "#94a3b8", // slate
};

// ---------------------------------------------------------------------------
// Bbox-relative authoring
// ---------------------------------------------------------------------------

/**
 * 27 named anchor points on an axis-aligned bbox: 1 centroid, 6 face
 * centres, 12 edge midpoints, 8 corners. Convention is three.js Y-up:
 * `top` = +Y, `bottom` = -Y, `front` = +Z, `back` = -Z, `right` = +X,
 * `left` = -X.
 */
export type BboxAnchor =
	| "center"
	// faces (1 axis named)
	| "top"
	| "bottom"
	| "front"
	| "back"
	| "left"
	| "right"
	// edges (2 axes named)
	| "top_front"
	| "top_back"
	| "top_left"
	| "top_right"
	| "bottom_front"
	| "bottom_back"
	| "bottom_left"
	| "bottom_right"
	| "front_left"
	| "front_right"
	| "back_left"
	| "back_right"
	// corners (3 axes named — order: top/bottom, front/back, left/right)
	| "top_front_left"
	| "top_front_right"
	| "top_back_left"
	| "top_back_right"
	| "bottom_front_left"
	| "bottom_front_right"
	| "bottom_back_left"
	| "bottom_back_right";

function anchorSigns(anchor: BboxAnchor): [number, number, number] {
	// Returns [x, y, z] in {-1, 0, +1} for each axis.
	const parts = anchor.split("_");
	let sx = 0;
	let sy = 0;
	let sz = 0;
	for (const p of parts) {
		switch (p) {
			case "left":
				sx = -1;
				break;
			case "right":
				sx = +1;
				break;
			case "bottom":
				sy = -1;
				break;
			case "top":
				sy = +1;
				break;
			case "back":
				sz = -1;
				break;
			case "front":
				sz = +1;
				break;
			case "center":
				break;
		}
	}
	return [sx, sy, sz];
}

/**
 * Resolve `(anchor, bbox)` into a local-space point. `bbox` is the
 * measured Box3 of the loaded mesh (in the mesh's local frame).
 */
export function resolveAnchor(anchor: BboxAnchor, bbox: Box3): Vec3 {
	const center = bbox.getCenter(new Vector3());
	const size = bbox.getSize(new Vector3());
	const [sx, sy, sz] = anchorSigns(anchor);
	return new Vector3(
		center.x + (sx * size.x) / 2,
		center.y + (sy * size.y) / 2,
		center.z + (sz * size.z) / 2,
	);
}

// ---------------------------------------------------------------------------
// Socket definition (authoring shape)
// ---------------------------------------------------------------------------

/**
 * How two compatible sockets meet:
 *
 *  - `"face"` (default): the two normals **oppose** (face-to-face contact).
 *    Example: a speaker_mount (down-facing) lands on a stand_top (up-facing).
 *    The held piece is rotated 180° around the host socket's tangent.
 *
 *  - `"edge"`: the two normals stay **parallel** while the tangents become
 *    **anti-parallel**. Example: two stage decks joined edge-to-edge —
 *    both tops still face up, but the shared edge runs in opposite
 *    directions on each deck. The held piece is rotated 180° around the
 *    host socket's normal.
 */
export type SocketMode = "face" | "edge";

export interface SocketDef {
	name: string;
	type: SocketType;
	/** Where on the bbox the socket sits. */
	anchor: BboxAnchor;
	/**
	 * Offset from the anchor in metres, three.js local-space (Y-up).
	 * Use to inset a corner socket inward, lift a face socket above the
	 * mesh surface, etc.
	 */
	offset?: [number, number, number];
	/**
	 * Outward direction (unit vector, three.js Y-up). The piece's "facing"
	 * at this socket. If absent, derived from the anchor face (e.g. `top`
	 * implies +Y). Required for corner / edge anchors where the natural
	 * direction is ambiguous.
	 */
	normal?: [number, number, number];
	/**
	 * In-plane tangent (perpendicular to normal). Used by edge / rail
	 * sockets where rotation about the normal matters (two floor_edge
	 * sockets need to be tangent-aligned for the decks to lie colinearly).
	 */
	tangent?: [number, number, number];
	/** See {@link SocketMode}. Defaults to `"face"`. */
	mode?: SocketMode;
}

/** Resolved socket in mesh local space: position + orthonormal frame. */
export interface ResolvedSocket {
	name: string;
	type: SocketType;
	position: Vec3;
	normal: Vec3;
	tangent: Vec3;
	mode: SocketMode;
	/**
	 * Unit vector from the piece's bbox centre to the socket position.
	 * Used by edge-mode pairing to ensure two matched sockets sit on
	 * **opposite sides** of their respective pieces (so the pieces end up
	 * next to each other, not overlapping).
	 */
	outward: Vec3;
}

/**
 * Default normal for an anchor when not explicitly given. For face anchors
 * the answer is unambiguous; for edges we pick the vertical face's outward
 * direction (so `top_front` defaults to +Y); for corners the caller must
 * provide a normal explicitly.
 */
function defaultNormal(anchor: BboxAnchor): Vec3 | null {
	const [sx, sy, sz] = anchorSigns(anchor);
	const nonZero = (sx !== 0 ? 1 : 0) + (sy !== 0 ? 1 : 0) + (sz !== 0 ? 1 : 0);
	if (nonZero === 1) {
		return new Vector3(sx, sy, sz);
	}
	if (nonZero === 2) {
		// Edge anchor — prefer the Y axis if it's involved (most edges of
		// interest are on top or bottom faces), else fall back to whichever
		// axis is non-zero.
		if (sy !== 0) return new Vector3(0, sy, 0);
		if (sz !== 0) return new Vector3(0, 0, sz);
		return new Vector3(sx, 0, 0);
	}
	return null; // corners or center: caller must specify
}

export function resolveSocket(def: SocketDef, bbox: Box3): ResolvedSocket {
	const base = resolveAnchor(def.anchor, bbox);
	if (def.offset) {
		base.x += def.offset[0];
		base.y += def.offset[1];
		base.z += def.offset[2];
	}

	const normal = def.normal
		? new Vector3(...def.normal).normalize()
		: (defaultNormal(def.anchor) ?? new Vector3(0, 1, 0));

	let tangent: Vec3;
	if (def.tangent) {
		tangent = new Vector3(...def.tangent).normalize();
	} else {
		// Derive a perpendicular tangent. Pick world-up if it's not parallel
		// to normal; otherwise pick world-X.
		const up = new Vector3(0, 1, 0);
		const candidate =
			Math.abs(normal.dot(up)) < 0.99 ? up : new Vector3(1, 0, 0);
		tangent = new Vector3().crossVectors(normal, candidate).normalize();
	}

	const bboxCenter = bbox.getCenter(new Vector3());
	const outward = base.clone().sub(bboxCenter);
	if (outward.lengthSq() < 1e-6) {
		// Socket sits exactly at the bbox centre (rare; "grab" or "center"
		// anchors) — fall back to the normal as a reasonable default.
		outward.copy(normal);
	} else {
		outward.normalize();
	}

	return {
		name: def.name,
		type: def.type,
		position: base,
		normal,
		tangent,
		mode: def.mode ?? "face",
		outward,
	};
}
