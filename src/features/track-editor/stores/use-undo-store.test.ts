import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TimelineAnnotation } from "./use-track-editor-store";
import { useTrackEditorStore } from "./use-track-editor-store";
import { useUndoStore } from "./use-undo-store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function annotation(id: string): TimelineAnnotation {
	return {
		id,
		uid: null,
		scoreId: "score-id",
		patternId: "pattern-id",
		startTime: 1,
		endTime: 2,
		zIndex: 0,
		blendMode: "replace",
		args: { color: "#ff0000" },
		createdAt: "created",
		updatedAt: "updated",
		patternName: "Solid",
		patternColor: "#ffffff",
	};
}

describe("revision-safe undo and redo", () => {
	const invokeMock = vi.mocked(invoke);

	beforeEach(() => {
		invokeMock.mockReset();
		useUndoStore.setState({
			undoStack: [],
			redoStack: [],
			_dragBefore: null,
			_busy: false,
		});
		useTrackEditorStore.setState({
			trackId: "track-id",
			scoreId: "score-id",
			annotations: [],
			selectedAnnotationIds: [],
			selectionCursor: null,
			error: null,
		});
	});

	it("does not change optimistic UI or history when the CAS fails", async () => {
		const clip = annotation("client-id");
		const reloadAnnotations = vi.fn(async () => true);
		useTrackEditorStore.setState({
			annotations: [clip],
			selectedAnnotationIds: [clip.id],
			reloadAnnotations,
		});
		useUndoStore.getState().push("Add", [], [clip], [], [clip.id]);
		invokeMock.mockRejectedValue(new Error("revision conflict"));

		await useUndoStore.getState().undo("track-id");

		expect(useTrackEditorStore.getState().annotations).toEqual([clip]);
		expect(useTrackEditorStore.getState().selectedAnnotationIds).toEqual([
			clip.id,
		]);
		expect(useUndoStore.getState().undoStack).toHaveLength(1);
		expect(useUndoStore.getState().redoStack).toHaveLength(0);
		expect(useUndoStore.getState()._busy).toBe(false);
		expect(reloadAnnotations).not.toHaveBeenCalled();
		expect(useTrackEditorStore.getState().error).toContain("revision conflict");
	});

	it("passes exact bases and rebases host-allocated ids through history", async () => {
		const clientClip = annotation("client-id");
		const hostClip = annotation("host-id");
		const reloadDocuments = [[], [hostClip], []] as TimelineAnnotation[][];
		const reloadAnnotations = vi.fn(async () => {
			useTrackEditorStore.setState({
				annotations: reloadDocuments.shift() ?? [],
			});
			return true;
		});
		useTrackEditorStore.setState({
			annotations: [clientClip],
			selectedAnnotationIds: [clientClip.id],
			reloadAnnotations,
		});
		useUndoStore.getState().push("Add", [], [clientClip], [], [clientClip.id]);
		invokeMock
			.mockResolvedValueOnce({
				idMap: {},
				appliedToCurrentProjection: true,
			})
			.mockResolvedValueOnce({
				idMap: { "client-id": "host-id" },
				appliedToCurrentProjection: true,
			})
			.mockResolvedValueOnce({
				idMap: {},
				appliedToCurrentProjection: true,
			});

		await useUndoStore.getState().undo("track-id");
		await useUndoStore.getState().redo("track-id");

		expect(useTrackEditorStore.getState().annotations).toEqual([hostClip]);
		expect(useTrackEditorStore.getState().selectedAnnotationIds).toEqual([
			"host-id",
		]);
		expect(useUndoStore.getState().undoStack[0].afterAnnotations[0].id).toBe(
			"host-id",
		);

		await useUndoStore.getState().undo("track-id");

		const calls = invokeMock.mock.calls.map(
			([, args]) =>
				args as {
					baseScores: Array<{ id: string }>;
					scores: Array<{ id: string }>;
				},
		);
		expect(calls[0]).toMatchObject({
			baseScores: [{ id: "client-id" }],
			scores: [],
		});
		expect(calls[1]).toMatchObject({
			baseScores: [],
			scores: [{ id: "client-id" }],
		});
		expect(calls[2]).toMatchObject({
			baseScores: [{ id: "host-id" }],
			scores: [],
		});
		expect(calls[0]?.baseScores?.[0]).not.toHaveProperty("patternName");
		expect(reloadAnnotations).toHaveBeenCalledTimes(3);
	});
});
