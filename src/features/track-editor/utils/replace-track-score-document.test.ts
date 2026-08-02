import { afterEach, describe, expect, it } from "vitest";
import type { TrackScore } from "@/bindings/schema";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import {
	hydrateTrackReplacement,
	rebaseTrackScoreIds,
	replaceTrackScoreDocument,
} from "./replace-track-score-document";

afterEach(resetInvoke);

function score(id: string): TrackScore {
	return {
		id,
		uid: null,
		scoreId: "score",
		patternId: "pattern",
		startTime: 0,
		endTime: 1,
		zIndex: 0,
		blendMode: "replace",
		args: {},
		createdAt: "created",
		updatedAt: "updated",
	};
}

describe("replaceTrackScoreDocument", () => {
	it("publishes a compound gesture with one full-document CAS", async () => {
		const calls: Array<{ command: string; args?: Record<string, unknown> }> =
			[];
		setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
			calls.push({ command, args });
			return {
				revision: "next",
				appliedToCurrentProjection: true,
				createdClipId: null,
				clips: [],
				idMap: { draft: "stored" },
				added: 1,
				updated: 1,
				removed: 0,
			} as T;
		});

		const base = [{ ...score("existing"), patternName: "presentation" }];
		const candidate = [
			{ ...base[0], endTime: 2 },
			{ ...score("draft"), startTime: 2, endTime: 3 },
		];
		await replaceTrackScoreDocument("score", "track", base, candidate);

		expect(calls).toHaveLength(1);
		expect(calls[0]).toMatchObject({
			command: "replace_track_scores",
			args: {
				scoreId: "score",
				trackId: "track",
				operationId: expect.any(String),
				baseScores: [{ id: "existing", endTime: 1 }],
				scores: [{ id: "existing", endTime: 2 }, { id: "draft" }],
			},
		});
	});

	it("retries a lost response with the exact same operation id", async () => {
		const requests: Record<string, unknown>[] = [];
		setInvoke(async <T>(_command: string, args?: Record<string, unknown>) => {
			requests.push(structuredClone(args ?? {}));
			if (requests.length === 1) throw new Error("response lost");
			return {
				revision: "next",
				appliedToCurrentProjection: true,
				createdClipId: null,
				clips: [],
				idMap: {},
				added: 0,
				updated: 1,
				removed: 0,
			} as T;
		});

		await replaceTrackScoreDocument(
			"score",
			"track",
			[score("clip")],
			[{ ...score("clip"), endTime: 2 }],
		);

		expect(requests).toHaveLength(2);
		expect(requests[0]).toEqual(requests[1]);
	});

	it("hydrates the authoritative current clips returned by a replay", () => {
		const draft = score("draft");
		const current = hydrateTrackReplacement("score", [draft], {
			revision: "newer-tip",
			appliedToCurrentProjection: false,
			createdClipId: null,
			idMap: { draft: "stored" },
			clips: [
				{
					id: "stored",
					patternId: "pattern",
					startTime: 4,
					endTime: 5,
					zIndex: 2,
					blendMode: "add",
					args: { current: true },
				},
			],
			added: 1,
			updated: 0,
			removed: 0,
		});
		expect(current).toEqual([
			{
				...draft,
				id: "stored",
				startTime: 4,
				endTime: 5,
				zIndex: 2,
				blendMode: "add",
				args: { current: true },
			},
		]);
	});

	it("rebases host-allocated ids without changing the candidate", () => {
		const draft = score("draft");
		expect(rebaseTrackScoreIds([draft], { draft: "stored" })).toEqual([
			{ ...draft, id: "stored" },
		]);
		expect(draft.id).toBe("draft");
	});
});
