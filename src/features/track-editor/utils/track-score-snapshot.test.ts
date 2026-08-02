import { describe, expect, it } from "vitest";
import type { TrackScore } from "@/bindings/schema";
import { trackScoreSnapshot } from "./track-score-snapshot";

function existing(): TrackScore {
	return {
		id: "existing-id",
		uid: "sync-id",
		scoreId: "score-id",
		patternId: "pattern-id",
		startTime: 0,
		endTime: 2,
		zIndex: 0,
		blendMode: "replace",
		args: { nested: { value: 1 } },
		createdAt: "created",
		updatedAt: "updated",
	};
}

describe("trackScoreSnapshot", () => {
	it("strips timeline-only presentation fields", () => {
		const timelineScore = {
			...existing(),
			patternName: "Solid",
			patternColor: "#ffffff",
		};
		expect(trackScoreSnapshot([timelineScore])).toEqual([existing()]);
	});
});
