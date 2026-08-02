import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import { trackScoreSnapshot } from "../utils/materialize-track-scores";
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
	beforeSelection: string[];
	afterSelection: string[];
};

type TrackReplacementResult = {
	idMap: Record<string, string>;
};

type UndoState = {
	undoStack: UndoEntry[];
	redoStack: UndoEntry[];
	_dragBefore: {
		annotations: TimelineAnnotation[];
		selection: string[];
	} | null;
	_busy: boolean;

	push: (
		label: string,
		before: TimelineAnnotation[],
		after: TimelineAnnotation[],
		beforeSel: string[],
		afterSel: string[],
	) => void;
	captureBeforeDrag: (
		annotations: TimelineAnnotation[],
		selection: string[],
	) => void;
	completeDrag: (
		label: string,
		afterAnnotations: TimelineAnnotation[],
		afterSelection: string[],
	) => void;
	undo: (trackId: string) => Promise<void>;
	redo: (trackId: string) => Promise<void>;
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
	selectedIds: string[],
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
		// +1 offset: row 0 is always the empty drop target above the top layer
		return idx >= 0 ? maxRow - idx + 1 : maxRow + 1;
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
	scoreId: string,
	trackId: string,
	baseAnnotations: TimelineAnnotation[],
	candidateAnnotations: TimelineAnnotation[],
): Promise<TrackReplacementResult> {
	return invoke<TrackReplacementResult>("replace_track_scores", {
		scoreId,
		trackId,
		baseScores: trackScoreSnapshot(baseAnnotations),
		scores: trackScoreSnapshot(candidateAnnotations),
	});
}

function rebaseIds(entry: UndoEntry, idMap: Record<string, string>): UndoEntry {
	if (Object.keys(idMap).length === 0) return entry;
	const annotations = (values: TimelineAnnotation[]) =>
		values.map((annotation) => {
			const id = idMap[annotation.id];
			return id === undefined ? annotation : { ...annotation, id };
		});
	const selection = (values: string[]) => values.map((id) => idMap[id] ?? id);
	return {
		...entry,
		beforeAnnotations: annotations(entry.beforeAnnotations),
		afterAnnotations: annotations(entry.afterAnnotations),
		beforeSelection: selection(entry.beforeSelection),
		afterSelection: selection(entry.afterSelection),
	};
}

function ownsEditorScope(trackId: string, scoreId: string): boolean {
	const editor = useTrackEditorStore.getState();
	return editor.trackId === trackId && editor.scoreId === scoreId;
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
		const editor = useTrackEditorStore.getState();
		const { scoreId } = editor;
		if (!scoreId || editor.trackId !== trackId) return;
		set({ _busy: true });
		let replacementApplied = false;

		try {
			const entry = undoStack[undoStack.length - 1];
			const result = await syncDbFromAnnotations(
				scoreId,
				trackId,
				entry.afterAnnotations,
				entry.beforeAnnotations,
			);
			replacementApplied = true;
			if (!ownsEditorScope(trackId, scoreId)) return;

			const rebasedUndo = undoStack.map((item) =>
				rebaseIds(item, result.idMap),
			);
			const rebasedRedo = get().redoStack.map((item) =>
				rebaseIds(item, result.idMap),
			);
			const rebasedEntry = rebasedUndo[rebasedUndo.length - 1];
			await useTrackEditorStore.getState().reloadAnnotations();
			if (!ownsEditorScope(trackId, scoreId)) return;
			const annotations = useTrackEditorStore.getState().annotations;
			const annotationIds = new Set(
				annotations.map((annotation) => annotation.id),
			);
			const selectedIds = rebasedEntry.beforeSelection.filter((id) =>
				annotationIds.has(id),
			);
			const cursor = deriveSelectionCursor(annotations, selectedIds);
			useTrackEditorStore.setState({
				selectedAnnotationIds: selectedIds,
				selectionCursor: cursor,
			});
			set({
				undoStack: rebasedUndo.slice(0, -1),
				redoStack: [...rebasedRedo, rebasedEntry],
			});
		} catch (error) {
			// A successful replacement followed by a failed reload leaves no safe
			// history base to replay. A CAS rejection, however, leaves both the UI
			// and history exactly as they were so the user can reload and retry.
			if (replacementApplied) set({ undoStack: [], redoStack: [] });
			if (ownsEditorScope(trackId, scoreId)) {
				useTrackEditorStore
					.getState()
					.setError(`Failed to undo: ${String(error)}`);
			}
		} finally {
			set({ _busy: false });
		}
	},

	redo: async (trackId) => {
		const { redoStack, _busy } = get();
		if (_busy || redoStack.length === 0) return;
		const editor = useTrackEditorStore.getState();
		const { scoreId } = editor;
		if (!scoreId || editor.trackId !== trackId) return;
		set({ _busy: true });
		let replacementApplied = false;

		try {
			const entry = redoStack[redoStack.length - 1];
			const result = await syncDbFromAnnotations(
				scoreId,
				trackId,
				entry.beforeAnnotations,
				entry.afterAnnotations,
			);
			replacementApplied = true;
			if (!ownsEditorScope(trackId, scoreId)) return;

			const rebasedUndo = get().undoStack.map((item) =>
				rebaseIds(item, result.idMap),
			);
			const rebasedRedo = redoStack.map((item) =>
				rebaseIds(item, result.idMap),
			);
			const rebasedEntry = rebasedRedo[rebasedRedo.length - 1];
			await useTrackEditorStore.getState().reloadAnnotations();
			if (!ownsEditorScope(trackId, scoreId)) return;
			const annotations = useTrackEditorStore.getState().annotations;
			const annotationIds = new Set(
				annotations.map((annotation) => annotation.id),
			);
			const selectedIds = rebasedEntry.afterSelection.filter((id) =>
				annotationIds.has(id),
			);
			const cursor = deriveSelectionCursor(annotations, selectedIds);
			useTrackEditorStore.setState({
				selectedAnnotationIds: selectedIds,
				selectionCursor: cursor,
			});
			set({
				redoStack: rebasedRedo.slice(0, -1),
				undoStack: [...rebasedUndo, rebasedEntry],
			});
		} catch (error) {
			if (replacementApplied) set({ undoStack: [], redoStack: [] });
			if (ownsEditorScope(trackId, scoreId)) {
				useTrackEditorStore
					.getState()
					.setError(`Failed to redo: ${String(error)}`);
			}
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
