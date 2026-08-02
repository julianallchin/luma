import { describe, expect, it } from "vitest";
import type { TrackScore } from "@/bindings/schema";
import type { DslAnnotation } from "@/lib/dsl";
import {
	materializeTrackScores,
	trackScoreSnapshot,
} from "./materialize-track-scores";

const NOW = "2026-07-30T12:00:00.000Z";

function existing(overrides?: Partial<TrackScore>): TrackScore {
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
		...overrides,
	};
}

function compiled(overrides?: Partial<DslAnnotation>): DslAnnotation {
	return {
		patternId: "pattern-id",
		startTime: 0,
		endTime: 2,
		zIndex: 0,
		blendMode: "replace",
		args: { nested: { value: 1 } },
		...overrides,
	};
}

describe("materializeTrackScores", () => {
	it("produces a persistence-only CAS snapshot", () => {
		const timelineScore = {
			...existing(),
			patternName: "Solid",
			patternColor: "#ffffff",
		};

		expect(trackScoreSnapshot([timelineScore])).toEqual([existing()]);
	});

	it("preserves identity and timestamps for unchanged clips", () => {
		expect(
			materializeTrackScores(
				[compiled({ id: "existing-id" })],
				[existing()],
				"score-id",
				{ now: NOW },
			),
		).toEqual([existing()]);
	});

	it("updates mutable data without replacing row identity", () => {
		const result = materializeTrackScores(
			[compiled({ id: "existing-id", zIndex: 3 })],
			[existing()],
			"score-id",
			{ now: NOW },
		);
		expect(result[0]).toMatchObject({
			id: "existing-id",
			uid: "sync-id",
			createdAt: "created",
			updatedAt: NOW,
			zIndex: 3,
		});
	});

	it("assigns correlation identities to new clips", () => {
		const result = materializeTrackScores([compiled()], [], "score-id", {
			now: NOW,
			newId: () => "generated-id",
		});
		expect(result[0]).toMatchObject({
			id: "generated-id",
			uid: null,
			scoreId: "score-id",
			createdAt: NOW,
			updatedAt: NOW,
		});
	});

	it("rejects foreign and duplicate clip identities", () => {
		expect(() =>
			materializeTrackScores(
				[compiled({ id: "foreign" })],
				[existing()],
				"score-id",
			),
		).toThrow("does not belong");
		expect(() =>
			materializeTrackScores(
				[compiled({ id: "existing-id" }), compiled({ id: "existing-id" })],
				[existing()],
				"score-id",
			),
		).toThrow("appears more than once");
	});
});
