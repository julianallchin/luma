/**
 * Socket model for the stage builder.
 *
 * Each piece declares a set of named anchor points in its **local three.js
 * frame** (Y-up). Sockets carry a type tag and a normal vector; mating
 * sockets magnetize together during placement and drag.
 *
 * Authoring is done relative to the piece's **bounding box** (measured at
 * load time via `Box3.setFromObject`) rather than raw local coordinates,
 * so socket positions stay correct regardless of where the modeler placed
 * the GLB pivot.
 *
 * The vocabulary itself — socket types, their kind and polarity, the anchor
 * names, the catalog — is generated from `gpui/crates/scene/src/catalog.rs`
 * into `./catalog.generated.ts`. This file is the *algorithm*: how an
 * authored def becomes a resolved frame, and when two sockets mate.
 */

import type { Box3, Vector3 as Vec3 } from "three";
import { Vector3 } from "three";
import {
	type BboxAnchor,
	type Polarity,
	type RollFreedom,
	SOCKET_KIND,
	SOCKET_POLARITY,
	SOCKET_ROLL,
	type SocketDef,
	type SocketMode,
	type SocketType,
} from "./catalog.generated";

export type {
	BboxAnchor,
	Polarity,
	RollFreedom,
	SocketDef,
	SocketMode,
	SocketType,
};

// ---------------------------------------------------------------------------
// Polarity
// ---------------------------------------------------------------------------

/**
 * Whether a socket may be the *moving* half of a joint. `male` is a plug,
 * `neutral` self-mates; `female` is a receptacle and only ever a host.
 */
export function canBeHeld(type: SocketType): boolean {
	const p: Polarity = SOCKET_POLARITY[type];
	return p === "male" || p === "neutral";
}

/** Whether a socket may be the *stationary* half of a joint. */
export function canHost(type: SocketType): boolean {
	const p: Polarity = SOCKET_POLARITY[type];
	return p === "female" || p === "neutral";
}

/**
 * Whether a held socket of type `held` mates a host socket of type `host`.
 *
 * The whole rule: same kind, the held half may be held, the host half may
 * host. It replaces a hand-maintained held→host adjacency list — a
 * thirteen-entry table is a lookup table pretending to be a rule, and it
 * drifted between its two copies.
 */
export function socketsMate(held: SocketType, host: SocketType): boolean {
	return (
		SOCKET_KIND[held] === SOCKET_KIND[host] && canBeHeld(held) && canHost(host)
	);
}

/** How much a mated piece may still turn about the socket normal. */
export function socketRoll(def: Pick<SocketDef, "type" | "roll">): RollFreedom {
	return def.roll ?? SOCKET_ROLL[def.type];
}

/**
 * Debug colors used by the socket overlay. Keep distinct so different
 * socket types are visually separable when stacked. Presentation only —
 * which is why it stays here and not in the generated catalog.
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
	/** How much the mated piece may still turn about `normal`. */
	roll: RollFreedom;
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
		roll: socketRoll(def),
	};
}
