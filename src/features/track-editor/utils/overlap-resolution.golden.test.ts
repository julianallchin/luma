import { describe, expect, it } from "vitest";
import goldens from "../../../../harness/goldens/overlap-resolution.json" with {
	type: "json",
};
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { type OverlapAction, resolveOverlaps } from "./overlap-resolution";
import { MIN_ANNOTATION_DURATION } from "./timeline-constants";

/**
 * Golden-vector characterization test for `resolveOverlaps`.
 *
 * This file REGENERATES NOTHING. It loads `harness/goldens/overlap-resolution.json`
 * and asserts that today's outputs deeply equal the recorded ones. The goldens
 * describe current behaviour, not desired behaviour — see "recorded quirks" below.
 *
 * ## How the goldens were produced
 *
 * A throwaway bun script imported this module directly, executed every input
 * below, and serialized `{ case, input, output }` triples. To regenerate after a
 * deliberate behaviour change, re-run an equivalent script: hydrate the compact
 * annotations exactly as `hydrate()` does here, call
 * `resolveOverlaps(anns, regionStart, regionEnd, new Set(zIndexes), new Set(excludeIds))`,
 * and write the results back in the same shape. Do not "fix" a golden by hand.
 *
 * ## Encoding
 *
 * Annotations are stored compactly as `{ id, startTime, endTime, zIndex }`; the
 * remaining `TimelineAnnotation` fields are constants filled in by `hydrate()`.
 * Sets are stored as arrays. Case shapes:
 *   - single scene:  input = scene,                         output = OverlapAction[]
 *   - sweep:         input = { annotations, zIndexes, excludeIds, times },
 *                    output = [{ regionStart, regionEnd, actions }] over every
 *                    ordered pair of `times` (including zero-width and inverted)
 *   - random:        input = { scenes: Scene[] },            output = OverlapAction[][]
 *
 * ## Float tolerance
 *
 * None. Every comparison is a strict deep-equal. All inputs sit on an exact
 * lattice (integers, or values snapped to 0.05) and `resolveOverlaps` only ever
 * emits `regionStart` / `regionEnd` verbatim — it does no arithmetic on the
 * outputs — so recorded numbers are bit-identical, and any drift is a real
 * behaviour change rather than rounding noise. The only tolerance in the file is
 * on the *invariant* check (1e-9), where durations ARE computed by subtraction.
 *
 * ## Recorded quirks (characterized, deliberately not fixed)
 *
 *   - A zero-width region (`start === end`) strictly inside a clip is recorded as
 *     a `split` with `leftEnd === rightStart`, i.e. it duplicates the clip at a
 *     point rather than being a no-op.
 *   - An inverted region (`start > end`) is not normalized: it can emit a split
 *     with `leftEnd > rightStart`, which yields inverted/overlapping intervals.
 *   - The short fixtures at 10/12/14 rely on binary floats (`10.05 - 10` is
 *     slightly below 0.05), so "exactly MIN" is not always exactly MIN. That is
 *     recorded as-is.
 */

type Compact = {
	id: string;
	startTime: number;
	endTime: number;
	zIndex: number;
};

type Scene = {
	annotations: Compact[];
	regionStart: number;
	regionEnd: number;
	zIndexes: number[];
	excludeIds: string[];
};

type GoldenCase = { case: string; input: unknown; output: unknown };

function hydrate(a: Compact): TimelineAnnotation {
	return {
		id: a.id,
		uid: null,
		scoreId: "score",
		patternId: "pattern",
		startTime: a.startTime,
		endTime: a.endTime,
		zIndex: a.zIndex,
		blendMode: "replace",
		args: {},
		createdAt: "created",
		updatedAt: "updated",
	};
}

function run(scene: Scene): OverlapAction[] {
	return resolveOverlaps(
		scene.annotations.map(hydrate),
		scene.regionStart,
		scene.regionEnd,
		new Set(scene.zIndexes),
		new Set(scene.excludeIds),
	);
}

const cases = goldens as GoldenCase[];

/** Durations the module itself claims are legal, per branch. */
function survivingDurations(
	scene: Scene,
	actions: OverlapAction[],
): { id: string; duration: number }[] {
	const byId = new Map(scene.annotations.map((a) => [a.id, a]));
	const out: { id: string; duration: number }[] = [];
	for (const action of actions) {
		const ann = byId.get(action.id);
		if (!ann) continue;
		switch (action.type) {
			case "delete":
				break;
			case "trim-end":
				out.push({ id: ann.id, duration: action.newEndTime - ann.startTime });
				break;
			case "trim-start":
				out.push({ id: ann.id, duration: ann.endTime - action.newStartTime });
				break;
			case "split":
				out.push({ id: ann.id, duration: action.leftEnd - ann.startTime });
				out.push({ id: ann.id, duration: ann.endTime - action.rightStart });
				break;
		}
	}
	return out;
}

describe("resolveOverlaps golden vectors", () => {
	it("covers a meaningful number of cases", () => {
		expect(cases.length).toBeGreaterThanOrEqual(15);
		expect(cases.length).toBeLessThanOrEqual(60);
	});

	for (const golden of cases) {
		if (golden.case.startsWith("sweep ")) {
			it(`${golden.case}`, () => {
				const input = golden.input as {
					annotations: Compact[];
					zIndexes: number[];
					excludeIds: string[];
					times: number[];
				};
				const actual: unknown[] = [];
				for (const regionStart of input.times) {
					for (const regionEnd of input.times) {
						actual.push({
							regionStart,
							regionEnd,
							actions: run({
								annotations: input.annotations,
								regionStart,
								regionEnd,
								zIndexes: input.zIndexes,
								excludeIds: input.excludeIds,
							}),
						});
					}
				}
				expect(actual).toEqual(golden.output);
			});
			continue;
		}

		if (golden.case.startsWith("seeded random scenes")) {
			it(`${golden.case}`, () => {
				const scenes = (golden.input as { scenes: Scene[] }).scenes;
				const actual = scenes.map((scene) => run(scene));
				expect(actual).toEqual(golden.output);

				// Invariant asserted alongside the snapshot: no interval the module
				// chooses to KEEP is shorter than MIN_ANNOTATION_DURATION. Tolerance
				// 1e-9 because these durations are computed by subtraction here.
				for (const [i, scene] of scenes.entries()) {
					for (const kept of survivingDurations(scene, actual[i])) {
						expect(kept.duration).toBeGreaterThanOrEqual(
							MIN_ANNOTATION_DURATION - 1e-9,
						);
					}
				}
			});
			continue;
		}

		it(`${golden.case}`, () => {
			expect(run(golden.input as Scene)).toEqual(golden.output);
		});
	}
});
