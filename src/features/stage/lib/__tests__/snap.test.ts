/**
 * Tests for the socket-based snap solver.
 *
 * All tests build a self-contained scene: synthetic mesh definitions, an
 * in-memory socket lookup, scene pieces with known world matrices. No GLB
 * loading, no global cache — the solver is fully deterministic here.
 *
 * Convention reminders (three.js, Y-up):
 *   +X = right, +Y = up, +Z = front (toward camera).
 *   Held sockets' normals point OUTWARD from the held piece.
 *   Two compatible sockets snap face-to-face (mode: "face") with normals
 *   opposing, or side-by-side (mode: "edge") with normals parallel and
 *   tangents reversed.
 */

import { Matrix4, Quaternion, Vector3 } from "three";
import { describe, expect, it } from "vitest";
import { type ScenePiece, type SnapSurface, solveSnap } from "../snap";
import type { ResolvedSocket } from "../sockets";

// ---------------------------------------------------------------------------
// Synthetic mesh definitions (sockets in piece-local space, three.js Y-up).
//
// These mimic what `resolveSocket` would produce for a few canonical pieces,
// but bypass the bbox-anchor resolution so the math being tested is
// transparent.
// ---------------------------------------------------------------------------

/** 1×1×0.6m deck, centered at origin in piece-local. Bbox center is at
 * (0, 0.3, 0); `outward` reflects which side of the bbox each socket sits on. */
const DECK_SOCKETS: ResolvedSocket[] = [
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
];

/** 1.22m straight truss, length along the X axis, centered at origin. */
const TRUSS_SOCKETS: ResolvedSocket[] = [
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
];

/** A 1m-tall speaker stand. Centroid at origin; top at +0.5, base at -0.5. */
const STAND_SOCKETS: ResolvedSocket[] = [
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
];

/** A speaker. 0.4m tall; mount on the bottom face. */
const SPEAKER_SOCKETS: ResolvedSocket[] = [
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
];

const FIXTURES = {
	deck: DECK_SOCKETS,
	truss: TRUSS_SOCKETS,
	stand: STAND_SOCKETS,
	speaker: SPEAKER_SOCKETS,
};

function lookup(mesh: string): ResolvedSocket[] {
	return FIXTURES[mesh as keyof typeof FIXTURES] ?? [];
}

function placeAt(
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

// Convenience: assert two Vector3-ish values are componentwise within tol.
function expectVec(
	actual: Vector3,
	expected: [number, number, number],
	tol = 1e-3,
) {
	expect(actual.x).toBeCloseTo(expected[0], 3);
	expect(actual.y).toBeCloseTo(expected[1], 3);
	expect(actual.z).toBeCloseTo(expected[2], 3);
	// also assert tol-aware (the precision arg above already does 1e-3)
	const dx = Math.abs(actual.x - expected[0]);
	const dy = Math.abs(actual.y - expected[1]);
	const dz = Math.abs(actual.z - expected[2]);
	expect(dx + dy + dz).toBeLessThan(tol * 3);
}

// ---------------------------------------------------------------------------
// Test scenarios
// ---------------------------------------------------------------------------

describe("solveSnap — face mode (truss → floor_corner)", () => {
	it("truss endpoint snaps to a deck corner and stands upright", () => {
		const deck = placeAt("d1", "deck");
		const cursor = new Vector3(-0.35, 0.6, 0.35); // exact corner_fl world pos
		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: cursor,
			pieces: [deck],
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBe("d1");
		expect(r.match?.hostSocket).toBe("corner_fl");
		// Held grab is at (0,0,0) in truss-local. After snap, it should sit
		// half a truss length above the corner (1.22m truss → 0.61m up).
		expectVec(r.position, [-0.35, 0.6 + 0.61, 0.35]);
		// Truss's local +X axis should map to world +Y (truss standing up).
		const xAxis = new Vector3(1, 0, 0).applyQuaternion(r.quaternion);
		expectVec(xAxis, [0, 1, 0]);
	});

	it("score is small when cursor is right on the corner", () => {
		const deck = placeAt("d1", "deck");
		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: new Vector3(-0.35, 0.6, 0.35),
			pieces: [deck],
			shiftHeld: false,
			lookupSockets: lookup,
		});
		expect(r.score).toBeLessThan(0.01);
	});

	it("dragging a truss whose centroid is near a corner's natural snap pose re-snaps to that corner", () => {
		// Reproduces the user's report: a truss already attached to one
		// corner is being dragged toward another. The cursor (truss grab
		// socket world position) is far above the floor (0.6m above the
		// host corner, where a standing truss's centroid naturally sits),
		// so host_socket-to-cursor distance exceeds the threshold. The
		// solver must also consider held_grab_after_snap_to_cursor.
		const deck = placeAt("d1", "deck");
		// Natural centroid position when snapping a 1.22m truss to corner_fr
		// (at world (0.35, 0.6, 0.35)) is (0.35, 1.21, 0.35).
		const cursor = new Vector3(0.35, 0.6 + 0.61, 0.35);
		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: cursor,
			pieces: [deck],
			excludeId: "self", // simulate dragging — host candidates exclude held
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBe("d1");
		expect(r.match?.hostSocket).toBe("corner_fr");
	});
});

describe("solveSnap — edge mode (floor ↔ floor)", () => {
	it("a second deck snaps next to the first, both upright", () => {
		const deck1 = placeAt("d1", "deck");
		// Cursor near deck1's back edge (world (0, 0.6, -0.5)).
		const r = solveSnap({
			heldMeshPath: "deck",
			cursorWorld: new Vector3(0, 0.6, -0.5),
			pieces: [deck1],
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBe("d1");
		expect(r.match?.hostSocket).toMatch(/edge_/);

		// Second deck's +Y should remain +Y in world (upright).
		const yAxis = new Vector3(0, 1, 0).applyQuaternion(r.quaternion);
		expectVec(yAxis, [0, 1, 0]);

		// Second deck's piece-local origin should be one deck-length away
		// from deck1's origin, on the side of the matched edge.
		// deck1's back edge is at z = -0.5 in world. Second deck's front edge
		// should coincide → second deck centered at z = -1, y = 0.
		expectVec(r.position, [0, 0, -1]);
	});

	it("cursor hovering on top of host deck snaps to the *nearest* edge", () => {
		// Reproduces the user's report: hover over the existing deck and
		// expect the new deck to land beside it, not on top of it. Cursor
		// is biased toward the back edge.
		const deck1 = placeAt("d1", "deck");
		const cursor = new Vector3(0, 0.6, -0.3);
		const surface: SnapSurface = {
			pieceId: "d1",
			hostMatrix: new Matrix4(),
			localPoint: new Vector3(0, 0.6, -0.3),
			localNormal: new Vector3(0, 1, 0),
			type: "floor_top",
		};

		const r = solveSnap({
			heldMeshPath: "deck",
			cursorWorld: cursor,
			pieces: [deck1],
			surface,
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.match?.hostSocket).toBe("edge_back");
		// Held deck should NOT overlap host. New deck centroid at z=-1.
		expectVec(r.position, [0, 0, -1]);
	});

	it("hovering near the right edge places the new deck to the right", () => {
		const deck1 = placeAt("d1", "deck");
		const cursor = new Vector3(0.3, 0.6, 0);
		const surface: SnapSurface = {
			pieceId: "d1",
			hostMatrix: new Matrix4(),
			localPoint: new Vector3(0.3, 0.6, 0),
			localNormal: new Vector3(0, 1, 0),
			type: "floor_top",
		};

		const r = solveSnap({
			heldMeshPath: "deck",
			cursorWorld: cursor,
			pieces: [deck1],
			surface,
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.match?.hostSocket).toBe("edge_right");
		// New deck centroid one deck-length to the right of host (x=+1).
		expectVec(r.position, [1, 0, 0]);
	});

	it("dead-center hover (all edges equidistant) still produces a non-overlapping placement", () => {
		// The pathological case the user saw: cursor right in the middle of
		// the host deck top. All four edges are exactly equidistant. Whichever
		// edge wins the tie, the resulting position must not overlap the host
		// deck's bbox.
		const deck1 = placeAt("d1", "deck");
		const r = solveSnap({
			heldMeshPath: "deck",
			cursorWorld: new Vector3(0, 0.6, 0),
			pieces: [deck1],
			surface: {
				pieceId: "d1",
				hostMatrix: new Matrix4(),
				localPoint: new Vector3(0, 0.6, 0),
				localNormal: new Vector3(0, 1, 0),
				type: "floor_top",
			},
			shiftHeld: false,
			lookupSockets: lookup,
		});

		// Either x = ±1 or z = ±1; never both zero.
		const offset = Math.max(Math.abs(r.position.x), Math.abs(r.position.z));
		expect(offset).toBeGreaterThan(0.9);
	});
});

describe("solveSnap — speaker on stand", () => {
	it("speaker_mount snaps to stand_top", () => {
		const stand = placeAt("s1", "stand", [0, 0.5, 0]); // base at y=0
		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld: new Vector3(0, 1.0, 0), // exact stand-top world pos
			pieces: [stand],
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBe("s1");
		expect(r.match?.hostSocket).toBe("top");
		// Speaker mount is at (0, -0.2) in speaker-local. After snap the
		// mount should be at the stand top (y=1.0). So speaker origin at
		// y = 1.0 + 0.2 = 1.2.
		expectVec(r.position, [0, 1.2, 0]);
		// Speaker right-side up.
		const yAxis = new Vector3(0, 1, 0).applyQuaternion(r.quaternion);
		expectVec(yAxis, [0, 1, 0]);
	});
});

describe("solveSnap — surface fallback (speaker anywhere on deck)", () => {
	it("speaker lands at the surface hit point with the deck as parent", () => {
		const deck = placeAt("d1", "deck"); // 1m deck, top at y=0.6
		// Cursor on the deck top, off-center.
		const cursor = new Vector3(0.3, 0.6, 0.2);
		const surface: SnapSurface = {
			pieceId: "d1",
			hostMatrix: new Matrix4(), // deck at origin
			localPoint: new Vector3(0.3, 0.6, 0.2),
			localNormal: new Vector3(0, 1, 0),
			type: "floor_top",
		};

		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld: cursor,
			pieces: [deck],
			surface,
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBe("d1");
		// Speaker mount at speaker-local (0,-0.2). Mount should land at hit
		// point → speaker origin at y = 0.6 + 0.2 = 0.8.
		expectVec(r.position, [0.3, 0.8, 0.2]);
	});

	it("a truss near a floor corner snaps to the discrete corner, not the surface", () => {
		// floor_corner is a real discrete socket. A truss should snap to
		// it when the cursor is near, even if the deck-top surface is also
		// hit — corners win because truss_end is not compatible with
		// floor_top (surface type), only with floor_corner.
		const deck = placeAt("d1", "deck");
		const cursor = new Vector3(-0.35, 0.6, 0.35); // exact corner_fl
		const surface: SnapSurface = {
			pieceId: "d1",
			hostMatrix: new Matrix4(),
			localPoint: new Vector3(-0.35, 0.6, 0.35),
			localNormal: new Vector3(0, 1, 0),
			type: "floor_top",
		};

		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: cursor,
			pieces: [deck],
			surface,
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBe("d1");
		expect(r.match?.hostSocket).toBe("corner_fl");
	});

	it("equipment dragged on a deck preserves its rotation around the vertical axis", () => {
		// The user rotated a CDJ/A9 by 45° around Y, then moves it slightly.
		// The snap must align the mount with the deck top but preserve the
		// spin around the shared normal so the user's facing is kept.
		const deck = placeAt("d1", "deck");
		const cursorWorld = new Vector3(0.2, 0.8, 0.1);
		const surface: SnapSurface = {
			pieceId: "d1",
			hostMatrix: new Matrix4(),
			localPoint: new Vector3(0.2, 0.6, 0.1),
			localNormal: new Vector3(0, 1, 0),
			type: "floor_top",
		};
		const userAngle = Math.PI / 4;
		const currentQuaternion = new Quaternion().setFromAxisAngle(
			new Vector3(0, 1, 0),
			userAngle,
		);
		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld,
			pieces: [deck],
			surface,
			excludeId: "self",
			currentQuaternion,
			shiftHeld: false,
			lookupSockets: lookup,
		});

		// Apply both rotations to a reference vector — should land at the
		// same place, regardless of the snap's intrinsic orientation.
		const ref = new Vector3(1, 0, 0);
		const expected = ref.clone().applyQuaternion(currentQuaternion);
		const actual = ref.clone().applyQuaternion(r.quaternion);
		expect(actual.x).toBeCloseTo(expected.x, 2);
		expect(actual.y).toBeCloseTo(expected.y, 2);
		expect(actual.z).toBeCloseTo(expected.z, 2);
	});

	it("equipment dragged above a deck stays on the deck given surface input (no ground drop)", () => {
		// Reproduces the user's "A9 on a deck snaps to the floor on drag"
		// report. The gizmo-drag path must pass a `surface` to the solver
		// (computed via downward raycast from the held piece). Without
		// surface, Pass 1 finds nothing (deck has no discrete floor_top),
		// Pass 2 is skipped, Pass 3 ground fires → equipment teleports
		// to Y=0.
		const deck = placeAt("d1", "deck");
		const cursorWorld = new Vector3(0.2, 0.8, 0.1);
		const surface: SnapSurface = {
			pieceId: "d1",
			hostMatrix: new Matrix4(),
			localPoint: new Vector3(0.2, 0.6, 0.1),
			localNormal: new Vector3(0, 1, 0),
			type: "floor_top",
		};
		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld,
			pieces: [deck],
			surface,
			excludeId: "self",
			shiftHeld: false,
			lookupSockets: lookup,
		});
		expect(r.parentId).toBe("d1");
		expect(r.match?.hostType).toBe("floor_top");
	});
});

describe("solveSnap — ground fallback", () => {
	it("speaker lands on the ground at the cursor when nothing else accepts", () => {
		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld: new Vector3(5, 0, 3),
			pieces: [],
			shiftHeld: false,
			lookupSockets: lookup,
		});

		expect(r.parentId).toBeNull();
		// Speaker mount at speaker-local (0,-0.2). On ground (y=0), mount
		// goes to y=0, so speaker origin at y=0.2.
		expectVec(r.position, [5, 0.2, 3]);
	});

	it("first deck placed on an empty stage sits on top of the ground (Y=0), not half-buried", () => {
		// Reproduces the user's report: placing the first deck on an empty
		// scene shouldn't put its centroid at cursor.y (which would bury
		// half the deck under the ground). The deck's bottom should rest
		// on Y=0.
		const r = solveSnap({
			heldMeshPath: "deck",
			cursorWorld: new Vector3(2, 0, 1),
			pieces: [],
			shiftHeld: false,
			lookupSockets: lookup,
		});
		// Deck bottom socket is at piece-local y=0 (geometry y=0 to 0.6 with
		// pivot at bottom; matches the test fixture). Wrapper should sit at
		// world y=0.
		expect(r.position.y).toBeCloseTo(0, 3);
	});
});

describe("solveSnap — compatibility filtering", () => {
	it("truss does not snap to stand_top (incompatible)", () => {
		const stand = placeAt("s1", "stand", [0, 0.5, 0]);
		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: new Vector3(0, 1.0, 0), // on stand top
			pieces: [stand],
			shiftHeld: false,
			lookupSockets: lookup,
		});
		expect(r.parentId).toBeNull(); // no snap → ground/free fallback
	});
});

describe("solveSnap — threshold", () => {
	it("rejects non-ground snaps further than ATTACH_THRESHOLD", () => {
		const deck = placeAt("d1", "deck");
		// Cursor 5m away from any deck socket.
		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: new Vector3(5, 0, 5),
			pieces: [deck],
			shiftHeld: false,
			lookupSockets: lookup,
		});
		// Truss has no ground-compatible socket → free placement.
		expect(r.parentId).toBeNull();
		expect(r.match).toBeNull();
	});
});

describe("solveSnap — shift disables snap", () => {
	it("returns free placement when shiftHeld is true even with a perfect match", () => {
		const stand = placeAt("s1", "stand", [0, 0.5, 0]);
		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld: new Vector3(0, 1.0, 0),
			pieces: [stand],
			shiftHeld: true,
			lookupSockets: lookup,
		});
		expect(r.parentId).toBeNull();
		expect(r.match).toBeNull();
	});
});

describe("solveSnap — held piece exclusion", () => {
	it("does not let a piece snap to itself when dragged", () => {
		const truss = placeAt("t1", "truss");
		const r = solveSnap({
			heldMeshPath: "truss",
			cursorWorld: new Vector3(0.61, 0, 0), // exactly at t1's end_b
			pieces: [truss],
			excludeId: "t1",
			shiftHeld: false,
			lookupSockets: lookup,
		});
		expect(r.parentId).toBeNull();
	});
});

describe("solveSnap — mode invariants", () => {
	it("face-mode pair leaves normals anti-parallel after snap", () => {
		const stand = placeAt("s1", "stand", [0, 0.5, 0]);
		const r = solveSnap({
			heldMeshPath: "speaker",
			cursorWorld: new Vector3(0, 1.0, 0),
			pieces: [stand],
			shiftHeld: false,
			lookupSockets: lookup,
		});
		// Speaker mount normal is (0,-1,0) in speaker-local. After snap,
		// it should point along -(+Y) = -Y in world, anti-parallel to the
		// stand_top normal (+Y).
		const mountNormalWorld = new Vector3(0, -1, 0).applyQuaternion(
			r.quaternion,
		);
		expect(mountNormalWorld.dot(new Vector3(0, 1, 0))).toBeCloseTo(-1, 2);
	});

	it("edge-mode pair leaves normals parallel after snap", () => {
		const deck1 = placeAt("d1", "deck");
		const r = solveSnap({
			heldMeshPath: "deck",
			cursorWorld: new Vector3(0, 0.6, -0.5),
			pieces: [deck1],
			shiftHeld: false,
			lookupSockets: lookup,
		});
		// Both decks' top normals are +Y in their respective locals.
		// After snap, held deck's +Y should still be world +Y.
		const heldUp = new Vector3(0, 1, 0).applyQuaternion(r.quaternion);
		expect(heldUp.dot(new Vector3(0, 1, 0))).toBeCloseTo(1, 2);
	});
});
