import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BeatGrid, TrackScore } from "@/bindings/schema";
import type { TrackWaveform } from "../use-track-editor-store";
import { useTrackEditorStore } from "../use-track-editor-store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}

function waveform(trackId: string): TrackWaveform {
	return {
		trackId,
		previewSamples: [],
		fullSamples: null,
		bands: null,
		previewBands: null,
		colors: null,
		previewColors: null,
		sampleRate: 44_100,
		durationSeconds: trackId === "track-b" ? 22 : 11,
	};
}

describe("track load ownership", () => {
	const invokeMock = vi.mocked(invoke);

	beforeEach(() => {
		invokeMock.mockReset();
		useTrackEditorStore.setState({
			trackId: null,
			venueId: null,
			scoreId: null,
			trackName: "",
			beatGrid: null,
			waveform: null,
			durationSeconds: 0,
			annotations: [],
		});
	});

	afterEach(() => {
		vi.clearAllMocks();
	});

	it("drops late results and playback from a superseded exact scope", async () => {
		const beatsA = deferred<BeatGrid | null>();
		const beatB = { bpm: 222 } as BeatGrid;
		const playback: string[] = [];

		invokeMock.mockImplementation(async (command, args) => {
			const input = args as Record<string, unknown> | undefined;
			const trackId = input?.trackId as string | undefined;
			switch (command) {
				case "get_track_beats":
					return (trackId === "track-a" ? beatsA.promise : beatB) as never;
				case "get_track_waveform":
					return waveform(trackId ?? "") as never;
				case "list_track_scores":
					return [] as never;
				case "host_load_track":
					playback.push(trackId ?? "");
					return undefined as never;
				case "leave_track":
					return undefined as never;
				default:
					throw new Error(`unexpected command: ${command}`);
			}
		});

		const loadA = useTrackEditorStore
			.getState()
			.loadTrack("track-a", "A", "venue-a", "score-a", false);
		const loadB = useTrackEditorStore
			.getState()
			.loadTrack("track-b", "B", "venue-b", "score-b", false);

		await loadB;
		beatsA.resolve({ bpm: 111 } as BeatGrid);
		await loadA;
		await Promise.resolve();

		const state = useTrackEditorStore.getState();
		expect([state.trackId, state.venueId, state.scoreId]).toEqual([
			"track-b",
			"venue-b",
			"score-b",
		]);
		expect(state.beatGrid).toBe(beatB);
		expect(state.durationSeconds).toBe(22);
		expect(playback).toEqual(["track-b"]);
		expect(
			invokeMock.mock.calls.some(
				([command, args]) =>
					command === "get_track_waveform" &&
					(args as Record<string, unknown> | undefined)?.trackId === "track-a",
			),
		).toBe(false);
	});

	it("drops a late annotation reload after the exact scope changes", async () => {
		const scoresA = deferred<TrackScore[]>();
		const scoreB = {
			id: "clip-b",
			uid: null,
			scoreId: "score-b",
			patternId: "pattern-b",
			startTime: 0,
			endTime: 1,
			zIndex: 0,
			blendMode: "replace",
			args: {},
			createdAt: "created",
			updatedAt: "updated",
		} satisfies TrackScore;
		invokeMock.mockImplementation(async (command) => {
			if (command === "list_track_scores") return scoresA.promise as never;
			throw new Error(`unexpected command: ${command}`);
		});
		useTrackEditorStore.setState({
			trackId: "track-a",
			venueId: "venue-a",
			scoreId: "score-a",
			annotations: [],
		});

		const reload = useTrackEditorStore.getState().reloadAnnotations();
		useTrackEditorStore.setState({
			trackId: "track-b",
			venueId: "venue-b",
			scoreId: "score-b",
			annotations: [scoreB],
		});
		scoresA.resolve([]);

		await expect(reload).resolves.toBe(false);
		expect(useTrackEditorStore.getState().annotations).toEqual([scoreB]);
	});

	it("drops an older annotation response within the same exact scope", async () => {
		const older = deferred<TrackScore[]>();
		const oldClip = {
			id: "old",
			uid: null,
			scoreId: "score-a",
			patternId: "pattern",
			startTime: 0,
			endTime: 1,
			zIndex: 0,
			blendMode: "replace",
			args: {},
			createdAt: "created",
			updatedAt: "updated",
		} satisfies TrackScore;
		const newestClip = { ...oldClip, id: "newest", startTime: 2, endTime: 3 };
		let request = 0;
		invokeMock.mockImplementation(async (command) => {
			if (command !== "list_track_scores") {
				throw new Error(`unexpected command: ${command}`);
			}
			request += 1;
			return (request === 1 ? older.promise : [newestClip]) as never;
		});
		useTrackEditorStore.setState({
			trackId: "track-a",
			venueId: "venue-a",
			scoreId: "score-a",
			annotations: [],
		});

		const first = useTrackEditorStore.getState().reloadAnnotations();
		await expect(
			useTrackEditorStore.getState().reloadAnnotations(),
		).resolves.toBe(true);
		older.resolve([oldClip]);
		await expect(first).resolves.toBe(false);

		expect(useTrackEditorStore.getState().annotations).toMatchObject([
			{ id: "newest", startTime: 2 },
		]);
	});
});
