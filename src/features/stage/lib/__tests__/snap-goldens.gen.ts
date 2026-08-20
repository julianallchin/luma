/**
 * GENERATOR + SHARED SCHEMA for the snap-solver golden vectors.
 *
 * ---------------------------------------------------------------------------
 * HOW THE GOLDENS WERE PRODUCED
 * ---------------------------------------------------------------------------
 *   bun run src/features/stage/lib/__tests__/snap-goldens.gen.ts
 *
 * writes  harness/goldens/stage-snap.json  by executing `solveSnap` over the
 * case table below. `snap.golden.test.ts` never calls this generator — it
 * loads the JSON and re-runs the solver against the *recorded inputs*, so the
 * only thing shared with this file is the schema + `hydrate`/`encode` helpers
 * (kept here rather than duplicated in the test).
 *
 * Re-run this ONLY when the solver's behaviour is intentionally changed, and
 * review the JSON diff line by line — an unexplained diff is the bug the
 * goldens exist to catch.
 *
 * ---------------------------------------------------------------------------
 * SERIALIZATION CONVENTIONS
 * ---------------------------------------------------------------------------
 * - Matrices are 16 numbers in three.js `Matrix4.toArray()` order, i.e.
 *   COLUMN-MAJOR. A port that stores row-major must transpose before
 *   comparing.
 * - Frame is three.js Y-up, right-handed: +X right, +Y up, +Z toward camera.
 *   Quaternions are (x, y, z, w).
 * - The snap result is recorded as a composed world matrix rather than
 *   position+quaternion, because quaternion double-cover (q ≡ -q) makes the
 *   raw components unstable across decompose implementations while the
 *   matrix is unique.
 * - All floats are rounded to 1e-6 before writing (`round6`) so ULP noise
 *   across engines/platforms does not churn the goldens. `-0` is normalised
 *   to `0`.
 * - `Infinity` (free-placement score) is encoded as the string "Infinity";
 *   JSON has no literal for it.
 */

import { Matrix4, Quaternion, Vector3 } from "three";
import {
	ATTACH_THRESHOLD,
	type ScenePiece,
	type SnapInput,
	type SnapSurface,
	solveSnap,
} from "../snap";
import type { SocketType } from "../sockets";
import { lookup } from "./snap-fixtures";

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

export type Vec3Tuple = [number, number, number];
export type QuatTuple = [number, number, number, number];
/** 16 numbers, column-major (three.js `Matrix4.toArray()` order). */
export type Mat4Tuple = number[];
export type ScoreJson = number | "Infinity";

export interface GoldenPiece {
	id: string;
	meshPath: string;
	worldMatrix: Mat4Tuple;
}

export interface GoldenSurface {
	pieceId: string | null;
	hostMatrix: Mat4Tuple;
	localPoint: Vec3Tuple;
	localNormal: Vec3Tuple;
	type: SocketType;
}

export interface GoldenInput {
	heldMeshPath: string;
	cursorWorld: Vec3Tuple;
	currentQuaternion?: QuatTuple;
	pieces: GoldenPiece[];
	excludeId?: string;
	shiftHeld: boolean;
	surface?: GoldenSurface;
}

export interface GoldenOutput {
	parentId: string | null;
	match: {
		heldSocket: string;
		hostSocket: string;
		hostId: string | null;
		hostType: SocketType;
	} | null;
	score: ScoreJson;
	/** Composed world transform of the held piece after the snap. */
	worldMatrix: Mat4Tuple;
	/**
	 * Tie-break probe. Re-runs the same input with the winning host piece
	 * excluded, so the second-choice snap is pinned too: a port that breaks
	 * an exact score tie the other way, or that ranks the runner-up
	 * differently, shows up here rather than silently passing.
	 *
	 * `null` when the case already sets `excludeId`, or when the winner has
	 * no host piece to exclude (ground / free placement).
	 */
	runnerUp: {
		parentId: string | null;
		hostSocket: string | null;
		score: ScoreJson;
	} | null;
}

export interface GoldenCase {
	case: string;
	input: GoldenInput;
	output: GoldenOutput;
}

// ---------------------------------------------------------------------------
// Encode / hydrate
// ---------------------------------------------------------------------------

export function round6(n: number): number {
	if (!Number.isFinite(n)) return n;
	const r = Math.round(n * 1e6) / 1e6;
	return Object.is(r, -0) ? 0 : r;
}

const m4 = (m: Matrix4): Mat4Tuple => m.toArray().map(round6);
const v3 = (v: Vector3): Vec3Tuple => [round6(v.x), round6(v.y), round6(v.z)];
const score = (s: number): ScoreJson =>
	Number.isFinite(s) ? round6(s) : "Infinity";

export function toMatrix(a: Mat4Tuple): Matrix4 {
	return new Matrix4().fromArray(a);
}

/** Rebuild a live `SnapInput` from its recorded JSON form. */
export function hydrate(input: GoldenInput): SnapInput {
	const pieces: ScenePiece[] = input.pieces.map((p) => ({
		id: p.id,
		meshPath: p.meshPath,
		worldMatrix: toMatrix(p.worldMatrix),
	}));
	const surface: SnapSurface | undefined = input.surface && {
		pieceId: input.surface.pieceId,
		hostMatrix: toMatrix(input.surface.hostMatrix),
		localPoint: new Vector3(...input.surface.localPoint),
		localNormal: new Vector3(...input.surface.localNormal),
		type: input.surface.type,
	};
	return {
		heldMeshPath: input.heldMeshPath,
		cursorWorld: new Vector3(...input.cursorWorld),
		currentQuaternion: input.currentQuaternion
			? new Quaternion(...input.currentQuaternion)
			: undefined,
		pieces,
		excludeId: input.excludeId,
		shiftHeld: input.shiftHeld,
		surface,
		lookupSockets: lookup,
	};
}

/** Run the solver on a recorded input and encode the result. */
export function runCase(input: GoldenInput): GoldenOutput {
	const live = hydrate(input);
	const r = solveSnap(live);
	const world = new Matrix4().compose(
		r.position,
		r.quaternion,
		new Vector3(1, 1, 1),
	);

	let runnerUp: GoldenOutput["runnerUp"] = null;
	if (!input.excludeId && r.parentId) {
		const probe = solveSnap({ ...hydrate(input), excludeId: r.parentId });
		runnerUp = {
			parentId: probe.parentId,
			hostSocket: probe.match?.hostSocket ?? null,
			score: score(probe.score),
		};
	}

	return {
		parentId: r.parentId,
		match: r.match ? { ...r.match } : null,
		score: score(r.score),
		worldMatrix: m4(world),
		runnerUp,
	};
}

// ---------------------------------------------------------------------------
// Case table
// ---------------------------------------------------------------------------

const I = () => new Matrix4();
const T = (x: number, y: number, z: number) =>
	new Matrix4().makeTranslation(x, y, z);
const yaw = (deg: number) => new Matrix4().makeRotationY((deg * Math.PI) / 180);
const mul = (...ms: Matrix4[]) =>
	ms.reduce((acc, m) => acc.multiply(m), new Matrix4());

/**
 * Host placements: identity, pure translation, the three axis-aligned yaws,
 * a non-axis-aligned yaw, and a two-deep parent chain (parent yaw45 @ (2,0,0)
 * composed with a child offset) — the composition order is exactly where a
 * row-major/column-major port diverges.
 */
const HOST_MATRICES: { name: string; m: Matrix4 }[] = [
	{ name: "identity", m: I() },
	{ name: "translate", m: T(2, 0, -1.5) },
	{ name: "yaw90", m: mul(T(1, 0, 1), yaw(90)) },
	{ name: "yaw180", m: mul(T(1, 0, 1), yaw(180)) },
	{ name: "yaw270", m: mul(T(1, 0, 1), yaw(270)) },
	{ name: "yaw37", m: mul(T(-0.7, 0, 0.3), yaw(37)) },
	{ name: "nested2", m: mul(T(2, 0, 0), yaw(45), T(0.5, 0.25, 0)) },
];

/** Canonical held/host pairs, each with the host-socket local point the
 * cursor is aimed at (the exact expected snap point). */
const PAIRS: {
	name: string;
	held: string;
	hostMesh: string;
	hostSocketLocal: Vec3Tuple;
}[] = [
	// deck + deck, edge mode (180° about the host normal, outward check).
	{
		name: "deck-edge",
		held: "deck",
		hostMesh: "deck",
		hostSocketLocal: [0, 0.6, -0.5],
	},
	// truss + truss, face mode, self-mating parallel-normal pair.
	{
		name: "truss-end",
		held: "truss",
		hostMesh: "truss",
		hostSocketLocal: [0.61, 0, 0],
	},
	// truss_end onto floor_corner, face mode, perpendicular normals.
	{
		name: "truss-corner",
		held: "truss",
		hostMesh: "deck",
		hostSocketLocal: [-0.35, 0.6, 0.35],
	},
	// speaker_mount onto stand_top, face mode.
	{
		name: "speaker-stand",
		held: "speaker",
		hostMesh: "stand",
		hostSocketLocal: [0, 0.5, 0],
	},
];

function pieceOf(id: string, meshPath: string, m: Matrix4): GoldenPiece {
	return { id, meshPath, worldMatrix: m4(m) };
}

function worldPoint(local: Vec3Tuple, m: Matrix4): Vector3 {
	return new Vector3(...local).applyMatrix4(m);
}

// --- seeded fuzz -----------------------------------------------------------

function mulberry32(seed: number): () => number {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = a;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

function buildCases(): { case: string; input: GoldenInput }[] {
	const cases: { case: string; input: GoldenInput }[] = [];

	// 1. Canonical pairs × host placements, cursor exactly on the host socket.
	for (const pair of PAIRS) {
		for (const hm of HOST_MATRICES) {
			const cursor = worldPoint(pair.hostSocketLocal, hm.m);
			cases.push({
				case: `${pair.name}/${hm.name}`,
				input: {
					heldMeshPath: pair.held,
					cursorWorld: v3(cursor),
					pieces: [pieceOf("h1", pair.hostMesh, hm.m)],
					shiftHeld: false,
				},
			});
		}
	}

	// 2. Surface fallback (equipment onto a deck top) at two placements.
	for (const hm of [HOST_MATRICES[0], HOST_MATRICES[5]]) {
		const localPoint: Vec3Tuple = [0.3, 0.6, 0.2];
		const cursor = worldPoint(localPoint, hm.m);
		cases.push({
			case: `surface-floor-top/${hm.name}`,
			input: {
				heldMeshPath: "speaker",
				cursorWorld: v3(cursor),
				pieces: [pieceOf("d1", "deck", hm.m)],
				shiftHeld: false,
				surface: {
					pieceId: "d1",
					hostMatrix: m4(hm.m),
					localPoint,
					localNormal: [0, 1, 0],
					type: "floor_top",
				},
			},
		});
	}

	// 3. Threshold sweep. Both sides of ATTACH_THRESHOLD are pinned, in the
	//    8 compass directions of the XZ plane at the knife-edge radii.
	const COMPASS: Vec3Tuple[] = [
		[1, 0, 0],
		[Math.SQRT1_2, 0, Math.SQRT1_2],
		[0, 0, 1],
		[-Math.SQRT1_2, 0, Math.SQRT1_2],
		[-1, 0, 0],
		[-Math.SQRT1_2, 0, -Math.SQRT1_2],
		[0, 0, -1],
		[Math.SQRT1_2, 0, -Math.SQRT1_2],
	];
	const RADII: { name: string; r: number }[] = [
		{ name: "r0", r: 0 },
		{ name: "r0.05", r: 0.05 },
		{ name: "r-below", r: ATTACH_THRESHOLD - 1e-4 },
		{ name: "r-above", r: ATTACH_THRESHOLD + 1e-4 },
		{ name: "r2", r: 2 },
	];
	// truss→corner: the +X direction gets every radius; +Z gets the three
	// that matter (dead-on plus both sides of the threshold). The deck-edge
	// pair adds the two knife-edge radii in a third direction. Keeping the
	// grid this coarse is deliberate — the golden file stays readable, and
	// the boundary is what actually needs pinning.
	const anchor = worldPoint([-0.35, 0.6, 0.35], I());
	for (const dirIdx of [0, 2]) {
		const radii = dirIdx === 0 ? RADII : [RADII[0], RADII[2], RADII[3]];
		const d = COMPASS[dirIdx];
		for (const rad of radii) {
			const c = anchor
				.clone()
				.add(new Vector3(d[0] * rad.r, d[1] * rad.r, d[2] * rad.r));
			cases.push({
				case: `threshold/truss-corner/dir${dirIdx}/${rad.name}`,
				input: {
					heldMeshPath: "truss",
					cursorWorld: v3(c),
					pieces: [pieceOf("d1", "deck", I())],
					shiftHeld: false,
				},
			});
		}
	}
	for (const rad of [RADII[2], RADII[3]]) {
		const d = COMPASS[6];
		const c = new Vector3(0, 0.6, -0.5).add(
			new Vector3(d[0] * rad.r, d[1] * rad.r, d[2] * rad.r),
		);
		cases.push({
			case: `threshold/deck-edge/dir6/${rad.name}`,
			input: {
				heldMeshPath: "deck",
				cursorWorld: v3(c),
				pieces: [pieceOf("d1", "deck", I())],
				shiftHeld: false,
			},
		});
	}

	// 4. Edge cases / degenerate geometry / extreme values.
	const deckI = pieceOf("d1", "deck", I());
	cases.push(
		{
			case: "edge/empty-scene-ground-fallback",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [5, 0, 3],
				pieces: [],
				shiftHeld: false,
			},
		},
		{
			case: "edge/empty-scene-no-ground-compat-free-placement",
			input: {
				heldMeshPath: "truss",
				cursorWorld: [5, 0, 3],
				pieces: [],
				shiftHeld: false,
			},
		},
		{
			case: "edge/shift-held-overrides-perfect-match",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [0, 1, 0],
				pieces: [pieceOf("s1", "stand", T(0, 0.5, 0))],
				shiftHeld: true,
			},
		},
		{
			case: "edge/shift-held-with-current-rotation",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [0, 1, 0],
				currentQuaternion: [0, Math.SQRT1_2, 0, Math.SQRT1_2],
				pieces: [pieceOf("s1", "stand", T(0, 0.5, 0))],
				shiftHeld: true,
			},
		},
		{
			case: "edge/self-exclusion-prevents-self-snap",
			input: {
				heldMeshPath: "truss",
				cursorWorld: [0.61, 0, 0],
				pieces: [pieceOf("t1", "truss", I())],
				excludeId: "t1",
				shiftHeld: false,
			},
		},
		{
			case: "edge/held-mesh-unknown-no-sockets",
			input: {
				heldMeshPath: "__missing__",
				cursorWorld: [1, 2, 3],
				pieces: [deckI],
				shiftHeld: false,
			},
		},
		{
			case: "edge/host-mesh-unknown-no-sockets",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [0, 0.6, 0],
				pieces: [pieceOf("x1", "__missing__", I())],
				shiftHeld: false,
			},
		},
		{
			case: "edge/incompatible-pair-truss-on-stand",
			input: {
				heldMeshPath: "truss",
				cursorWorld: [0, 1, 0],
				pieces: [pieceOf("s1", "stand", T(0, 0.5, 0))],
				shiftHeld: false,
			},
		},
		{
			case: "edge/extreme-coordinates",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [1e6, 0, -1e6],
				pieces: [pieceOf("d1", "deck", T(1e6, 0, -1e6))],
				shiftHeld: false,
			},
		},
		{
			case: "edge/degenerate-nonuniform-scale-host",
			input: {
				heldMeshPath: "truss",
				cursorWorld: v3(
					worldPoint([-0.35, 0.6, 0.35], new Matrix4().makeScale(2, 1, 0.5)),
				),
				pieces: [pieceOf("d1", "deck", new Matrix4().makeScale(2, 1, 0.5))],
				shiftHeld: false,
			},
		},
		{
			case: "edge/tie-dead-center-hover-four-equidistant-edges",
			input: {
				heldMeshPath: "deck",
				cursorWorld: [0, 0.6, 0],
				pieces: [deckI],
				shiftHeld: false,
				surface: {
					pieceId: "d1",
					hostMatrix: m4(I()),
					localPoint: [0, 0.6, 0],
					localNormal: [0, 1, 0],
					type: "floor_top",
				},
			},
		},
		{
			case: "edge/two-hosts-equidistant",
			input: {
				heldMeshPath: "truss",
				cursorWorld: [0.5, 0.6, 0.35],
				pieces: [deckI, pieceOf("d2", "deck", T(1, 0, 0))],
				shiftHeld: false,
			},
		},
		{
			case: "edge/twist-preserved-45deg-yaw-on-surface",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [0.2, 0.8, 0.1],
				currentQuaternion: [0, Math.sin(Math.PI / 8), 0, Math.cos(Math.PI / 8)],
				pieces: [deckI],
				shiftHeld: false,
				surface: {
					pieceId: "d1",
					hostMatrix: m4(I()),
					localPoint: [0.2, 0.6, 0.1],
					localNormal: [0, 1, 0],
					type: "floor_top",
				},
			},
		},
		{
			case: "edge/tilted-surface-normal",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [0, 0.6, 0],
				pieces: [deckI],
				shiftHeld: false,
				surface: {
					pieceId: "d1",
					hostMatrix: m4(I()),
					localPoint: [0, 0.6, 0],
					localNormal: [0, Math.SQRT1_2, Math.SQRT1_2],
					type: "floor_top",
				},
			},
		},
		{
			case: "edge/surface-with-null-piece-id",
			input: {
				heldMeshPath: "speaker",
				cursorWorld: [0, 0.6, 0],
				pieces: [],
				shiftHeld: false,
				surface: {
					pieceId: null,
					hostMatrix: m4(I()),
					localPoint: [0, 0.6, 0],
					localNormal: [0, 1, 0],
					type: "floor_top",
				},
			},
		},
	);

	// 5. Seeded fuzz. Fixed seed, jittered host pose + cursor. Kept small
	//    (the golden file stays human-readable); widen locally by bumping
	//    FUZZ_N and re-running the generator if a port needs more coverage.
	const rnd = mulberry32(0x5eed_1234);
	const FUZZ_N = 4;
	for (let i = 0; i < FUZZ_N; i++) {
		const pair = PAIRS[i % PAIRS.length];
		const hostM = mul(
			T((rnd() - 0.5) * 6, 0, (rnd() - 0.5) * 6),
			yaw(rnd() * 360),
		);
		const jitter = new Vector3(
			(rnd() - 0.5) * 1.2,
			(rnd() - 0.5) * 1.2,
			(rnd() - 0.5) * 1.2,
		);
		cases.push({
			case: `fuzz/${i}/${pair.name}`,
			input: {
				heldMeshPath: pair.held,
				cursorWorld: v3(worldPoint(pair.hostSocketLocal, hostM).add(jitter)),
				pieces: [pieceOf("h1", pair.hostMesh, hostM)],
				shiftHeld: false,
			},
		});
	}

	return cases;
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

if (import.meta.main) {
	const { writeFileSync, mkdirSync } = await import("node:fs");
	const { dirname, resolve } = await import("node:path");
	const out = resolve(
		__dirname,
		"../../../../../harness/goldens/stage-snap.json",
	);
	const golden: GoldenCase[] = buildCases().map((c) => ({
		case: c.case,
		input: c.input,
		output: runCase(c.input),
	}));
	mkdirSync(dirname(out), { recursive: true });
	// Tab-indented, but with pure-number arrays (vectors, quaternions, the
	// 16-element matrices) collapsed onto one line — a 16-line column of
	// digits is unreadable, and a matrix diff should be one line.
	const json = JSON.stringify(golden, null, "\t").replace(
		/\[\n[\t\n]*(-?[\d.e+-]+,?(?:\n[\t\n]*-?[\d.e+-]+,?)*)\n\t*\]/g,
		(_m, body: string) => `[${body.replace(/\s+/g, " ").trim()}]`,
	);
	writeFileSync(out, `${json}\n`);
	console.log(`wrote ${golden.length} cases -> ${out}`);
}
