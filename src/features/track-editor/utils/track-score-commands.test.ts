import { afterEach, describe, expect, it } from "vitest";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import { deleteTrackScore, updateTrackScore } from "./track-score-commands";

afterEach(resetInvoke);

describe("track score commands", () => {
	it.each([
		[
			"update",
			() => updateTrackScore("score", "track", { id: "clip", zIndex: 2 }),
		],
		["delete", () => deleteTrackScore("score", "track", "clip")],
	] as const)(
		"retries %s with one stable operation id",
		async (_label, run) => {
			const calls: Array<{ command: string; args?: Record<string, unknown> }> =
				[];
			setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
				calls.push({ command, args: structuredClone(args) });
				if (calls.length === 1) throw new Error("response lost");
				return {
					revision: "revision",
					createdClipId: null,
					clips: [],
					idMap: {},
					added: 0,
					updated: 0,
					removed: 0,
					appliedToCurrentProjection: true,
				} as T;
			});

			await run();
			expect(calls).toHaveLength(2);
			expect(calls[0]).toEqual(calls[1]);
			expect(calls[0]).toMatchObject({
				args: {
					payload: {
						id: "clip",
						scoreId: "score",
						trackId: "track",
						operationId: expect.any(String),
					},
				},
			});
		},
	);
});
