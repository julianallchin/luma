import { describe, expect, it } from "vitest";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { applyOverlapActions, resolveOverlaps } from "./overlap-resolution";

function annotation(
	id: string,
	startTime: number,
	endTime: number,
): TimelineAnnotation {
	return {
		id,
		uid: null,
		scoreId: "score",
		patternId: "pattern",
		startTime,
		endTime,
		zIndex: 0,
		blendMode: "replace",
		args: {},
		createdAt: "created",
		updatedAt: "updated",
	};
}

describe("overlap candidate transformation", () => {
	it("builds a complete split candidate without persistence side effects", () => {
		const base = [annotation("clip", 0, 10)];
		const actions = resolveOverlaps(base, 4, 6, new Set([0]), new Set());
		const result = applyOverlapActions(base, actions, () => "draft");

		expect(base).toEqual([annotation("clip", 0, 10)]);
		expect(result.newIds).toEqual(["draft"]);
		expect(result.annotations).toMatchObject([
			{ id: "clip", startTime: 0, endTime: 4 },
			{ id: "draft", startTime: 6, endTime: 10 },
		]);
	});

	it("applies a mixed gesture as one deterministic candidate", () => {
		const base = [annotation("left", 0, 5), annotation("inside", 6, 7)];
		const actions = resolveOverlaps(base, 3, 8, new Set([0]), new Set());
		const result = applyOverlapActions(base, actions);

		expect(result.annotations).toMatchObject([
			{ id: "left", startTime: 0, endTime: 3 },
		]);
	});
});
