/**
 * Synthetic socket tables for the snap solver tests.
 *
 * These mimic what `resolveSocket` would produce for a few canonical pieces,
 * but bypass bbox-anchor resolution so the math under test is transparent —
 * and so no GLB loading / mesh cache is involved. Shared by `snap.test.ts`
 * (behavioural assertions) and `snap.golden.test.ts` (numeric goldens); keep
 * exactly one copy of the tables here.
 *
 * Convention reminders (three.js, Y-up):
 *   +X = right, +Y = up, +Z = front (toward camera).
 *   Held sockets' normals point OUTWARD from the held piece.
 */

import { Matrix4, Vector3 } from "three";
import { SOCKET_ROLL } from "../catalog.generated";
import type { ScenePiece } from "../snap";
import type { ResolvedSocket } from "../sockets";

/**
 * The tables below author the geometry and leave roll freedom to the socket
 * type, which is where it lives for every real piece too.
 */
function withRoll(sockets: Omit<ResolvedSocket, "roll">[]): ResolvedSocket[] {
	return sockets.map((s) => ({ ...s, roll: SOCKET_ROLL[s.type] }));
}

/** 1×1×0.6m deck, pivot at the bottom face. Bbox center is at (0, 0.3, 0);
 * `outward` reflects which side of the bbox each socket sits on. */
export const DECK_SOCKETS: ResolvedSocket[] = withRoll([
	{
		name: "grab",
		type: "grab",
		position: new Vector3(0, 0.3, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, 1, 0),
	},
	{
		name: "bottom",
		type: "bottom_mount",
		position: new Vector3(0, 0, 0),
		normal: new Vector3(0, -1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, -1, 0),
	},
	// No discrete floor_top socket — surface fallback puts equipment at
	// the actual cursor hit point on the deck top.
	{
		name: "edge_front",
		type: "floor_edge",
		position: new Vector3(0, 0.6, 0.5),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "edge",
		outward: new Vector3(0, 0, 1),
	},
	{
		name: "edge_back",
		type: "floor_edge",
		position: new Vector3(0, 0.6, -0.5),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "edge",
		outward: new Vector3(0, 0, -1),
	},
	{
		name: "edge_left",
		type: "floor_edge",
		position: new Vector3(-0.5, 0.6, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(0, 0, 1),
		mode: "edge",
		outward: new Vector3(-1, 0, 0),
	},
	{
		name: "edge_right",
		type: "floor_edge",
		position: new Vector3(0.5, 0.6, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(0, 0, 1),
		mode: "edge",
		outward: new Vector3(1, 0, 0),
	},
	{
		name: "corner_fl",
		type: "floor_corner",
		position: new Vector3(-0.35, 0.6, 0.35),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(-Math.SQRT1_2, 0, Math.SQRT1_2),
	},
	{
		name: "corner_fr",
		type: "floor_corner",
		position: new Vector3(0.35, 0.6, 0.35),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(Math.SQRT1_2, 0, Math.SQRT1_2),
	},
]);

/** 1.22m straight truss, length along the X axis, centered at origin. */
export const TRUSS_SOCKETS: ResolvedSocket[] = withRoll([
	{
		name: "grab",
		type: "grab",
		position: new Vector3(0, 0, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, 1, 0),
	},
	{
		name: "end_a",
		type: "truss_end",
		position: new Vector3(-0.61, 0, 0),
		normal: new Vector3(-1, 0, 0),
		tangent: new Vector3(0, 0, 1),
		mode: "face",
		outward: new Vector3(-1, 0, 0),
	},
	{
		name: "end_b",
		type: "truss_end",
		position: new Vector3(0.61, 0, 0),
		normal: new Vector3(1, 0, 0),
		tangent: new Vector3(0, 0, 1),
		mode: "face",
		outward: new Vector3(1, 0, 0),
	},
]);

/** A 1m-tall speaker stand. Centroid at origin; top at +0.5, base at -0.5. */
export const STAND_SOCKETS: ResolvedSocket[] = withRoll([
	{
		name: "grab",
		type: "grab",
		position: new Vector3(0, 0, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, 1, 0),
	},
	{
		name: "top",
		type: "stand_top",
		position: new Vector3(0, 0.5, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, 1, 0),
	},
	{
		name: "base",
		type: "stand_bottom",
		position: new Vector3(0, -0.5, 0),
		normal: new Vector3(0, -1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, -1, 0),
	},
]);

/** A speaker. 0.4m tall; mount on the bottom face. */
export const SPEAKER_SOCKETS: ResolvedSocket[] = withRoll([
	{
		name: "grab",
		type: "grab",
		position: new Vector3(0, 0, 0),
		normal: new Vector3(0, 1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, 1, 0),
	},
	{
		name: "mount",
		type: "speaker_mount",
		position: new Vector3(0, -0.2, 0),
		normal: new Vector3(0, -1, 0),
		tangent: new Vector3(1, 0, 0),
		mode: "face",
		outward: new Vector3(0, -1, 0),
	},
]);

export const FIXTURES = {
	deck: DECK_SOCKETS,
	truss: TRUSS_SOCKETS,
	stand: STAND_SOCKETS,
	speaker: SPEAKER_SOCKETS,
};

/** In-memory `SocketLookup`. Unknown mesh paths resolve to no sockets. */
export function lookup(mesh: string): ResolvedSocket[] {
	return FIXTURES[mesh as keyof typeof FIXTURES] ?? [];
}

export function placeAt(
	id: string,
	mesh: string,
	pos: [number, number, number] = [0, 0, 0],
): ScenePiece {
	return {
		id,
		meshPath: mesh,
		worldMatrix: new Matrix4().setPosition(pos[0], pos[1], pos[2]),
	};
}
