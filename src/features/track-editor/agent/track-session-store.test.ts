import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { trackSessionKey, useTrackSessionStore } from "./track-session-store";

describe("track session scope", () => {
	beforeEach(() => {
		useTrackSessionStore.setState({ contexts: {} });
	});

	afterEach(() => resetInvoke());

	it("keeps contexts for the same track isolated by venue and score", () => {
		const a = {
			trackId: "track-1",
			venueId: "venue-a",
			scoreId: "score-a",
		};
		const b = {
			trackId: "track-1",
			venueId: "venue-b",
			scoreId: "score-b",
		};
		const sessions = useTrackSessionStore.getState();

		sessions.updateContext(a, { trackName: "A" });
		sessions.updateContext(b, { trackName: "B" });

		expect(sessions.getContext(a)?.trackName).toBe("A");
		expect(sessions.getContext(b)?.trackName).toBe("B");
		expect(Object.keys(useTrackSessionStore.getState().contexts)).toEqual([
			trackSessionKey(a),
			trackSessionKey(b),
		]);
	});

	it("updates annotations only in the exact scope", () => {
		const a = {
			trackId: "track-1",
			venueId: "venue-a",
			scoreId: "score-a",
		};
		const b = { ...a, scoreId: "score-b" };
		const sessions = useTrackSessionStore.getState();
		sessions.updateContext(a, {});
		sessions.updateContext(b, {});

		sessions.setAnnotations(a, [{ id: "clip-a" } as never]);

		expect(sessions.getContext(a)?.annotations).toHaveLength(1);
		expect(sessions.getContext(b)?.annotations).toEqual([]);
	});

	it("rejects an explicit score outside the requested track and venue", async () => {
		const calls: string[] = [];
		setInvoke(async (command) => {
			calls.push(command);
			if (command === "list_scores_for_track") return [] as never;
			throw new Error(`unexpected command: ${command}`);
		});
		// Avoid the unrelated pattern-catalog load; scope validation must fail
		// before bootstrap reaches any of the content loaders.
		useTrackEditorStore.setState({ patterns: [{ id: "pattern-1" } as never] });

		const result = await useTrackSessionStore.getState().bootstrap({
			trackId: "track-a",
			venueId: "venue-a",
			venueName: "A",
			userId: "user-a",
			trackName: "Track A",
			scoreId: "score-from-somewhere-else",
		});

		expect(result).toEqual({
			ok: false,
			error: "Score does not belong to the requested track and venue.",
		});
		expect(calls).toEqual(["list_scores_for_track"]);
		expect(useTrackSessionStore.getState().contexts).toEqual({});
	});
});
