import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { TrackScore } from "@/bindings/schema";
import type {
	SelectionCursor,
	TimelineAnnotation,
} from "./use-track-editor-store";
import { useTrackEditorStore } from "./use-track-editor-store";

const MAX_UNDO_ENTRIES = 50;

type UndoEntry = {
	label: string;
	beforeAnnotations: TimelineAnnotation[];
	afterAnnotations: TimelineAnnotation[];
	beforeSelection: number[];
	afterSelection: number[];
};

type UndoState = {
	undoStack: UndoEntry[];
	redoStack: UndoEntry[];
	_dragBefore: {
		annotations: TimelineAnnotation[];
		selection: number[];
	} | null;
	_busy: boolean;

	push: (
		label: string,
		before: TimelineAnnotation[],
		after: TimelineAnnotation[],
		beforeSel: number[],
		afterSel: number[],
	) => void;
	captureBeforeDrag: (
		annotations: TimelineAnnotation[],
		selection: number[],
	) => void;
	completeDrag: (
		label: string,
		afterAnnotations: TimelineAnnotation[],
		afterSelection: number[],
	) => void;
	undo: (trackId: number) => Promise<void>;
	redo: (trackId: number) => Promise<void>;
	clear: () => void;
	canUndo: () => boolean;
	canRedo: () => boolean;
};

function annotationsEqual(
	a: TimelineAnnotation[],
	b: TimelineAnnotation[],
): boolean {
	if (a.length !== b.length) return false;
	const mapA = new Map(a.map((ann) => [ann.id, ann]));
	for (const ann of b) {
		const other = mapA.get(ann.id);
		if (!other) return false;
		if (
			other.startTime !== ann.startTime ||
			other.endTime !== ann.endTime ||
			other.zIndex !== ann.zIndex ||
			other.blendMode !== ann.blendMode ||
			other.patternId !== ann.patternId
		)
			return false;
		// Shallow compare args
		const argsA = JSON.stringify(other.args);
		const argsB = JSON.stringify(ann.args);
		if (argsA !== argsB) return false;
	}
	return true;
}

/** Derive a selection cursor as the bounding box of the selected annotations. */
function deriveSelectionCursor(
	annotations: TimelineAnnotation[],
	selectedIds: number[],
): SelectionCursor | null {
	if (selectedIds.length === 0) return null;
	const idSet = new Set(selectedIds);
	const selected = annotations.filter((a) => idSet.has(a.id));
	if (selected.length === 0) return null;

	const startTime = Math.min(...selected.map((a) => a.startTime));
	const endTime = Math.max(...selected.map((a) => a.endTime));

	// Compute row indices: row 0 = highest z (visually top)
	const allZ = Array.from(new Set(annotations.map((a) => a.zIndex))).sort(
		(a, b) => a - b,
	);
	const maxRow = Math.max(0, allZ.length - 1);
	const selectedZ = new Set(selected.map((a) => a.zIndex));
	const rows = [...selectedZ].map((z) => {
		const idx = allZ.indexOf(z);
		return idx >= 0 ? maxRow - idx : 0;
	});
	const minRow = Math.min(...rows);
	const maxSelectedRow = Math.max(...rows);

	return {
		trackRow: minRow,
		trackRowEnd: maxSelectedRow !== minRow ? maxSelectedRow : null,
		startTime,
		endTime,
	};
}

async function syncDbFromAnnotations(
	trackId: number,
	annotations: TimelineAnnotation[],
): Promise<void> {
	const scores: TrackScore[] = annotations.map((ann) => ({
		id: ann.id,
		scoreId: ann.scoreId,
		patternId: ann.patternId,
		startTime: ann.startTime,
		endTime: ann.endTime,
		zIndex: ann.zIndex,
		blendMode: ann.blendMode,
		args: ann.args ?? {},
		remoteId: ann.remoteId ?? null,
		uid: ann.uid ?? null,
		createdAt: ann.createdAt,
		updatedAt: ann.updatedAt,
	}));
	await invoke("replace_track_scores", { trackId, scores });
}

export const useUndoStore = create<UndoState>((set, get) => ({
	undoStack: [],
	redoStack: [],
	_dragBefore: null,
	_busy: false,

	push: (label, before, after, beforeSel, afterSel) => {
		if (annotationsEqual(before, after)) return;
		set((state) => ({
			undoStack: [
				...state.undoStack.slice(-(MAX_UNDO_ENTRIES - 1)),
				{
					label,
					beforeAnnotations: before,
					afterAnnotations: after,
					beforeSelection: beforeSel,
					afterSelection: afterSel,
				},
			],
			redoStack: [],
		}));
	},

	captureBeforeDrag: (annotations, selection) => {
		set({
			_dragBefore: {
				annotations: [...annotations],
				selection: [...selection],
			},
		});
	},

	completeDrag: (label, afterAnnotations, afterSelection) => {
		const { _dragBefore } = get();
		if (!_dragBefore) return;
		get().push(
			label,
			_dragBefore.annotations,
			afterAnnotations,
			_dragBefore.selection,
			afterSelection,
		);
		set({ _dragBefore: null });
	},

	undo: async (trackId) => {
		const { undoStack, _busy } = get();
		if (_busy || undoStack.length === 0) return;
		set({ _busy: true });

		try {
			const entry = undoStack[undoStack.length - 1];
			// Restore Zustand state + derive cursor from selection
			const cursor = deriveSelectionCursor(
				entry.beforeAnnotations,
				entry.beforeSelection,
			);
			useTrackEditorStore.setState({
				annotations: entry.beforeAnnotations,
				selectedAnnotationIds: entry.beforeSelection,
				selectionCursor: cursor,
			});
			// Sync DB
			await syncDbFromAnnotations(trackId, entry.beforeAnnotations);
			// Move entry from undo to redo
			set((state) => ({
				undoStack: state.undoStack.slice(0, -1),
				redoStack: [...state.redoStack, entry],
			}));
		} finally {
			set({ _busy: false });
		}
	},

	redo: async (trackId) => {
		const { redoStack, _busy } = get();
		if (_busy || redoStack.length === 0) return;
		set({ _busy: true });

		try {
			const entry = redoStack[redoStack.length - 1];
			// Restore Zustand state + derive cursor from selection
			const cursor = deriveSelectionCursor(
				entry.afterAnnotations,
				entry.afterSelection,
			);
			useTrackEditorStore.setState({
				annotations: entry.afterAnnotations,
				selectedAnnotationIds: entry.afterSelection,
				selectionCursor: cursor,
			});
			// Sync DB
			await syncDbFromAnnotations(trackId, entry.afterAnnotations);
			// Move entry from redo to undo
			set((state) => ({
				redoStack: state.redoStack.slice(0, -1),
				undoStack: [...state.undoStack, entry],
			}));
		} finally {
			set({ _busy: false });
		}
	},

	clear: () => {
		set({ undoStack: [], redoStack: [], _dragBefore: null });
	},

	canUndo: () => get().undoStack.length > 0,
	canRedo: () => get().redoStack.length > 0,
}));
