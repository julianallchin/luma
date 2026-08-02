import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TimelineAnnotation } from "./use-track-editor-store";
import { useTrackEditorStore } from "./use-track-editor-store";
import { useUndoStore } from "./use-undo-store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => vi.unstubAllGlobals());

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}

function annotation(id: string, startTime: number): TimelineAnnotation {
	return {
		id,
		uid: null,
		scoreId: "score",
		patternId: "pattern",
		startTime,
		endTime: startTime + 1,
		zIndex: 0,
		blendMode: "replace",
		args: {},
		createdAt: "created",
		updatedAt: "updated",
	};
}

function result(clips: TimelineAnnotation[]) {
	return {
		revision: "revision",
		createdClipId: null,
		clips: clips.map(
			({ id, patternId, startTime, endTime, zIndex, blendMode, args }) => ({
				id,
				patternId,
				startTime,
				endTime,
				zIndex,
				blendMode,
				args,
			}),
		),
		idMap: {},
		added: 0,
		updated: clips.length,
		removed: 0,
		appliedToCurrentProjection: true,
	};
}

describe("atomic score gestures", () => {
	const invokeMock = vi.mocked(invoke);

	beforeEach(() => {
		invokeMock.mockReset();
		useUndoStore.setState({
			undoStack: [],
			redoStack: [],
			_dragBefore: null,
			_busy: false,
			_epoch: 0,
		});
		useTrackEditorStore.setState({
			trackId: "track",
			venueId: "venue",
			scoreId: "score",
			readOnly: false,
			patterns: [],
			annotations: [annotation("a", 0), annotation("b", 2)],
			selectedAnnotationIds: ["a", "b"],
			selectionCursor: null,
			error: null,
		});
	});

	it("publishes a multi-clip inspector edit as one document CAS", async () => {
		const expected = [annotation("a", 0), annotation("b", 2)].map((clip) => ({
			...clip,
			zIndex: 3,
		}));
		invokeMock.mockResolvedValueOnce(result(expected));

		await useTrackEditorStore.getState().updateAnnotationsBatch([
			{ id: "a", zIndex: 3 },
			{ id: "b", zIndex: 3 },
		]);

		expect(invokeMock).toHaveBeenCalledTimes(1);
		expect(invokeMock).toHaveBeenCalledWith(
			"replace_track_scores",
			expect.objectContaining({
				scoreId: "score",
				trackId: "track",
				operationId: expect.any(String),
			}),
		);
		expect(useTrackEditorStore.getState().annotations).toMatchObject([
			{ id: "a", zIndex: 3 },
			{ id: "b", zIndex: 3 },
		]);
		expect(useUndoStore.getState().undoStack).toHaveLength(1);
	});

	it("keeps UI and undo unchanged when a compound delete fails", async () => {
		const before = useTrackEditorStore.getState().annotations;
		invokeMock.mockRejectedValue(new Error("revision conflict"));

		await useTrackEditorStore.getState().deleteAnnotations(["a", "b"]);

		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(invokeMock.mock.calls[0]).toEqual(invokeMock.mock.calls[1]);
		expect(useTrackEditorStore.getState().annotations).toEqual(before);
		expect(useUndoStore.getState().undoStack).toHaveLength(0);
		expect(useTrackEditorStore.getState().error).toContain("revision conflict");
	});

	it("commits a multi-clip drag once from its exact pre-drag base", async () => {
		vi.stubGlobal("requestAnimationFrame", () => 1);
		const store = useTrackEditorStore.getState();
		store.captureBeforeDrag();
		store.updateAnnotationsLocal([
			{ id: "a", startTime: 4, endTime: 5 },
			{ id: "b", startTime: 6, endTime: 7 },
		]);
		const moved = useTrackEditorStore.getState().annotations;
		invokeMock.mockResolvedValueOnce(result(moved));

		await useTrackEditorStore.getState().persistAnnotations(["a", "b"]);

		expect(invokeMock).toHaveBeenCalledTimes(1);
		const args = invokeMock.mock.calls[0]?.[1] as {
			baseScores: TimelineAnnotation[];
			scores: TimelineAnnotation[];
		};
		expect(args.baseScores).toMatchObject([
			{ id: "a", startTime: 0 },
			{ id: "b", startTime: 2 },
		]);
		expect(args.scores).toMatchObject([
			{ id: "a", startTime: 4 },
			{ id: "b", startTime: 6 },
		]);
		expect(useUndoStore.getState().undoStack).toHaveLength(1);
	});

	it("drops an older mutation response after a newer same-scope local intent", async () => {
		vi.stubGlobal("requestAnimationFrame", () => 1);
		const response = deferred<ReturnType<typeof result>>();
		invokeMock.mockReturnValueOnce(response.promise);

		const olderMutation = useTrackEditorStore
			.getState()
			.updateAnnotationsBatch([{ id: "a", zIndex: 3 }]);
		await Promise.resolve();

		useTrackEditorStore
			.getState()
			.updateAnnotationsLocal([{ id: "a", startTime: 8, endTime: 9 }]);
		response.resolve(
			result([{ ...annotation("a", 0), zIndex: 3 }, annotation("b", 2)]),
		);
		await olderMutation;

		expect(useTrackEditorStore.getState().annotations).toMatchObject([
			{ id: "a", startTime: 8, endTime: 9, zIndex: 0 },
			{ id: "b", startTime: 2 },
		]);
		expect(useUndoStore.getState().undoStack).toHaveLength(0);
	});
});
