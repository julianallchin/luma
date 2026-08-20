/**
 * Golden-vector characterization test for the socket snap solver.
 *
 * This test does NOT describe intended behaviour — `snap.test.ts` does that.
 * It pins the solver's exact numeric output so a rewrite (notably the planned
 * Rust port) has to reproduce it bit-for-bit-ish, including all the places a
 * port silently diverges:
 *
 *   - quaternion handedness and the 180° flips (face = flip about the host
 *     tangent, edge = identity/no flip),
 *   - matrix multiply order in `host.world · hostFrame · flip · heldFrame⁻¹`,
 *   - three.js Y-up vs a Z-up convention,
 *   - the edge-mode `outward` opposition test and the face-mode
 *     same-type/parallel-normal side test,
 *   - candidate scoring (min of host-socket-to-cursor and held-grab-to-cursor)
 *     and the ATTACH_THRESHOLD cutoff,
 *   - tie-breaking between equally scored candidates (first-wins iteration
 *     order over held sockets × pieces × host sockets).
 *
 * ---------------------------------------------------------------------------
 * GOLDEN FILE
 * ---------------------------------------------------------------------------
 *   harness/goldens/stage-snap.json  — 59 cases: canonical socket pairs across
 *   a table of host world matrices (identity, translation, yaw 90/180/270, a
 *   non-axis-aligned yaw 37°, a two-deep parent chain), a sweep across both
 *   sides of ATTACH_THRESHOLD, degenerate/extreme inputs, and a seeded
 *   mulberry32 fuzz tail.
 *
 * Produced by executing the module — see the generator, which is the ONLY
 * thing that writes the file:
 *   bun run src/features/stage/lib/__tests__/snap-goldens.gen.ts
 * This test regenerates nothing; it loads the JSON, rebuilds each recorded
 * input, re-runs `solveSnap`, and compares.
 *
 * ---------------------------------------------------------------------------
 * TOLERANCE
 * ---------------------------------------------------------------------------
 * Structural fields (parentId, socket names, host type) must match exactly.
 * Floats — the 16 world-matrix elements, the score, and the runner-up score —
 * are compared with an absolute tolerance of 1e-6, which is the quantum the
 * goldens are rounded to. That is deliberately loose enough to absorb
 * cross-engine ULP noise in trig/sqrt and tight enough that any real change
 * in the solve (a flipped axis, a reordered multiply, a different tie-break)
 * fails. Note the tolerance is absolute, so the one 1e6-magnitude case is the
 * strictest in relative terms.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { Matrix4, Vector3 } from "three";
import { describe, expect, it } from "vitest";
import { solveSnap } from "../snap";
import {
	type GoldenCase,
	hydrate,
	round6,
	type ScoreJson,
} from "./snap-goldens.gen";

const TOL = 1e-6;

const GOLDEN_PATH = resolve(
	__dirname,
	"../../../../../harness/goldens/stage-snap.json",
);

const GOLDENS: GoldenCase[] = JSON.parse(readFileSync(GOLDEN_PATH, "utf8"));

function encodeScore(s: number): ScoreJson {
	return Number.isFinite(s) ? round6(s) : "Infinity";
}

function expectScore(actual: number, expected: ScoreJson, label: string) {
	if (expected === "Infinity") {
		expect(encodeScore(actual), label).toBe("Infinity");
		return;
	}
	expect(Number.isFinite(actual), `${label} must be finite`).toBe(true);
	expect(Math.abs(actual - expected), `${label} Δ`).toBeLessThanOrEqual(TOL);
}

describe("solveSnap — golden vectors", () => {
	it("golden file is present and sized as expected", () => {
		expect(GOLDENS.length).toBe(59);
		expect(new Set(GOLDENS.map((c) => c.case)).size).toBe(GOLDENS.length);
	});

	for (const gold of GOLDENS) {
		it(gold.case, () => {
			const r = solveSnap(hydrate(gold.input));

			expect(r.parentId, "parentId").toBe(gold.output.parentId);
			expect(r.match, "match").toEqual(gold.output.match);
			expectScore(r.score, gold.output.score, "score");

			const world = new Matrix4()
				.compose(r.position, r.quaternion, new Vector3(1, 1, 1))
				.toArray();
			expect(world.length).toBe(gold.output.worldMatrix.length);
			for (let i = 0; i < world.length; i++) {
				expect(
					Math.abs(world[i] - gold.output.worldMatrix[i]),
					`worldMatrix[${i}] Δ (got ${world[i]}, want ${gold.output.worldMatrix[i]})`,
				).toBeLessThanOrEqual(TOL);
			}

			// Runner-up probe: same input with the winning host excluded. Pins
			// the second-choice snap, so a port that resolves an exact score
			// tie the other way is caught even when the winner still matches.
			// (Only pass 1 honours `excludeId` — the surface fallback does not,
			// which is why some surface cases report the same parent again.)
			if (gold.output.runnerUp === null) {
				expect(
					!gold.input.excludeId && r.parentId !== null,
					"runnerUp recorded as null",
				).toBe(false);
				return;
			}
			const probe = solveSnap({
				...hydrate(gold.input),
				excludeId: r.parentId ?? undefined,
			});
			expect(probe.parentId, "runnerUp.parentId").toBe(
				gold.output.runnerUp.parentId,
			);
			expect(probe.match?.hostSocket ?? null, "runnerUp.hostSocket").toBe(
				gold.output.runnerUp.hostSocket,
			);
			expectScore(probe.score, gold.output.runnerUp.score, "runnerUp.score");
		});
	}
});
