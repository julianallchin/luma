/**
 * Golden-vector characterization test for `sockets.ts` — the *input* side of
 * the snap solver.
 *
 * What it pins:
 *   - `resolveAnchor`: the whole 27-member `BboxAnchor` vocabulary, swept
 *     against four Box3 shapes (unit cube at origin, off-centre deck whose
 *     pivot is not the centroid, a degenerate zero-Y box, a large asymmetric
 *     box). One case per box, output keyed by anchor.
 *   - `resolveSocket`: position / normal / tangent / outward / mode for a
 *     matrix of `SocketDef`s covering every branch:
 *       * `defaultNormal`'s three regimes (face → unambiguous, edge → prefers
 *         Y then Z then X, corner/center → null → +Y fallback),
 *       * the `|dot(normal, up)| >= 0.99` tangent branch from both sides
 *         (a straddling 0.994997 case and a 0.988488 case) and on both signs
 *         of Y — the cross-product argument order here decides the tangent's
 *         sign, which edge-mode snapping rotates about,
 *       * offset absent / present / large,
 *       * explicit normal absent / axis / unnormalized diagonal / zero,
 *       * explicit tangent absent / unnormalized / non-perpendicular,
 *       * mode absent / face / edge,
 *       * `outward`'s 1e-6 degenerate-length fallback, straddled from both
 *         sides (lengthSq 2.5e-7 vs 8e-6).
 *
 * Tolerance: outputs are compared with `toBeCloseTo` at 9 decimals; the
 * goldens were recorded rounded to 1e-9 (with -0 normalized to 0). Exact
 * equality would be over-tight for cross-product/normalize float noise across
 * platforms; 1e-9 is far tighter than any real behaviour change.
 *
 * This test REGENERATES NOTHING at runtime — it only reads the JSON.
 *
 * ---------------------------------------------------------------------------
 * HOW THE GOLDENS WERE PRODUCED
 * ---------------------------------------------------------------------------
 * A throwaway script (not checked in) built the same case table below, called
 * the real `resolveAnchor` / `resolveSocket`, rounded every component to 1e-9,
 * and wrote `harness/goldens/stage-sockets.json`. It was run with
 * `bun harness/goldens/.gen-sockets.ts` from the repo root and then deleted.
 * To re-record after an *intentional* behaviour change, re-create such a
 * script (the case inputs are all in the golden file's `input` field, so it
 * can simply read the JSON, re-run the functions, and rewrite `output`).
 */

import { Box3, Vector3 } from "three";
import { describe, expect, it } from "vitest";
import goldens from "@/../harness/goldens/stage-sockets.json";
import {
	type BboxAnchor,
	resolveAnchor,
	resolveSocket,
	type SocketDef,
} from "../sockets";

const PRECISION = 9;

type Vec3Tuple = [number, number, number];
type BoxTuple = [number[], number[]];

interface AnchorCase {
	case: string;
	input: { fn: "resolveAnchor"; bbox: BoxTuple; anchors: BboxAnchor[] };
	output: Record<string, number[]>;
}

interface SocketCase {
	case: string;
	input: { fn: "resolveSocket"; bbox: BoxTuple; def: SocketDef };
	output: {
		name: string;
		type: string;
		mode: string;
		position: number[];
		normal: number[];
		tangent: number[];
		outward: number[];
	};
}

const cases = goldens as unknown as (AnchorCase | SocketCase)[];

function makeBox([min, max]: BoxTuple): Box3 {
	return new Box3(
		new Vector3(min[0], min[1], min[2]),
		new Vector3(max[0], max[1], max[2]),
	);
}

function tuple(v: Vector3): Vec3Tuple {
	return [v.x, v.y, v.z];
}

function expectVec(actual: Vec3Tuple, expected: number[], label: string) {
	for (let i = 0; i < 3; i++) {
		expect(actual[i], `${label}[${i}]`).toBeCloseTo(expected[i], PRECISION);
	}
}

describe("sockets golden vectors", () => {
	it("golden file is populated and unique", () => {
		expect(cases.length).toBeGreaterThanOrEqual(15);
		expect(new Set(cases.map((c) => c.case)).size).toBe(cases.length);
	});

	const anchorCases = cases.filter(
		(c): c is AnchorCase => c.input.fn === "resolveAnchor",
	);
	const socketCases = cases.filter(
		(c): c is SocketCase => c.input.fn === "resolveSocket",
	);

	describe("resolveAnchor", () => {
		for (const c of anchorCases) {
			it(c.case, () => {
				const bbox = makeBox(c.input.bbox);
				// every anchor in the golden is exercised, and the golden must
				// cover the full 27-member vocabulary
				expect(Object.keys(c.output)).toHaveLength(27);
				for (const anchor of c.input.anchors) {
					expectVec(
						tuple(resolveAnchor(anchor, bbox)),
						c.output[anchor],
						`${c.case}/${anchor}`,
					);
				}
			});
		}
	});

	describe("resolveSocket", () => {
		for (const c of socketCases) {
			it(c.case, () => {
				const got = resolveSocket(c.input.def, makeBox(c.input.bbox));
				expect(got.name).toBe(c.output.name);
				expect(got.type).toBe(c.output.type);
				expect(got.mode).toBe(c.output.mode);
				expectVec(tuple(got.position), c.output.position, "position");
				expectVec(tuple(got.normal), c.output.normal, "normal");
				expectVec(tuple(got.tangent), c.output.tangent, "tangent");
				expectVec(tuple(got.outward), c.output.outward, "outward");
			});
		}
	});
});
