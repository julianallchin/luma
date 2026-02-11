import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	BeatGrid,
	BlendMode,
	HostAudioSnapshot,
	PatternArgDef,
	PatternSummary,
	TrackScore as TrackScoreBinding,
} from "@/bindings/schema";
import {
	applyOverlapActions,
	resolveOverlaps,
} from "../utils/overlap-resolution";
import {
	MAX_ZOOM,
	MAX_ZOOM_Y,
	MIN_ANNOTATION_DURATION,
	MIN_ZOOM,
	MIN_ZOOM_Y,
} from "../utils/timeline-constants";
import { useUndoStore } from "./use-undo-store";

function readPersistedNumber(key: string, fallback: number): number {
	try {
		const raw = localStorage.getItem(key);
		if (raw !== null) {
			const n = Number(raw);
			if (Number.isFinite(n)) return n;
		}
	} catch {
		// localStorage may be unavailable
	}
	return fallback;
}

const PLAYBACK_RATE_MIN = 0.25;
const PLAYBACK_RATE_MAX = 2;

// Re-export with the correct type from bindings
export type TrackScore = TrackScoreBinding;

export type BandEnvelopes = {
	low: number[];
	mid: number[];
	high: number[];
};

export type TrackWaveform = {
	trackId: number;
	previewSamples: number[];
	fullSamples: number[] | null;
	/** 3-band envelopes for full waveform (rekordbox-style) */
	bands: BandEnvelopes | null;
	/** 3-band envelopes for preview waveform */
	previewBands: BandEnvelopes | null;
	/** Legacy: RGB colors */
	colors: number[] | null;
	previewColors: number[] | null;
	sampleRate: number;
	durationSeconds: number;
};

export type CreateAnnotationInput = {
	trackId: number;
	patternId: number;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode?: BlendMode | null;
	createdAt?: string;
	updatedAt?: string;
	args?: Record<string, unknown>;
};

export type UpdateAnnotationInput = {
	id: number;
	startTime?: number;
	endTime?: number;
	zIndex?: number;
	blendMode?: BlendMode | null;
	args?: Record<string, unknown>;
};

export type TimelineAnnotation = TrackScore & {
	patternName?: string;
	patternColor?: string;
};

export type SelectionCursor = {
	trackRow: number;
	trackRowEnd: number | null; // null = single row, number = multi-row range
	startTime: number;
	endTime: number | null; // null = point selection, number = range selection
};

// Clipboard stores annotations relative to selection start
export type ClipboardItem = {
	patternId: number;
	offsetFromStart: number; // time offset from selection start
	duration: number;
	zIndex: number;
	blendMode: BlendMode;
	args?: Record<string, unknown>;
};

export type Clipboard = {
	items: ClipboardItem[];
	totalDuration: number; // from selection start to end of last annotation
};

type TrackEditorState = {
	trackId: number | null;
	trackName: string;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	beatGridLoading: boolean;
	waveform: TrackWaveform | null;
	waveformLoading: boolean;
	annotations: TimelineAnnotation[];
	annotationsLoading: boolean;
	patterns: PatternSummary[];
	patternsLoading: boolean;
	patternArgs: Record<number, PatternArgDef[]>;
	zoom: number;
	scrollX: number;
	playheadPosition: number;
	isPlaying: boolean;
	isCompositing: boolean;
	selectionCursor: SelectionCursor | null;
	selectedAnnotationIds: number[];
	clipboard: Clipboard | null;
	draggingPatternId: number | null;
	dragOrigin: { x: number; y: number };
	isDraggingAnnotation: boolean;
	autoScroll: boolean;
	zoomY: number;
	panelHeight: number;
	playbackRate: number;
	error: string | null;

	loadTrack: (trackId: number, trackName: string) => Promise<void>;
	loadPatterns: () => Promise<void>;
	loadTrackPlayback: (trackId: number) => Promise<void>;
	play: () => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
	syncPlaybackState: (snapshot: HostAudioSnapshot) => void;
	setZoom: (zoom: number) => void;
	setScrollX: (scrollX: number) => void;
	setPlayheadPosition: (position: number) => void;
	setIsPlaying: (isPlaying: boolean) => void;
	setIsCompositing: (isCompositing: boolean) => void;
	setSelectionCursor: (cursor: SelectionCursor | null) => void;
	setSelectedAnnotationIds: (ids: number[]) => void;
	selectAnnotation: (annotationId: number | null) => void;
	setDraggingPatternId: (
		patternId: number | null,
		origin?: { x: number; y: number },
	) => void;
	setIsDraggingAnnotation: (isDragging: boolean) => void;
	setAutoScroll: (autoScroll: boolean) => void;
	setZoomY: (zoomY: number) => void;
	setPanelHeight: (height: number) => void;
	setPlaybackRate: (rate: number) => Promise<void>;
	createAnnotation: (
		input: Omit<CreateAnnotationInput, "trackId">,
	) => Promise<TrackScore | null>;
	updateAnnotation: (
		input: UpdateAnnotationInput,
	) => Promise<TrackScore | null>;
	updateAnnotationsLocal: (updates: UpdateAnnotationInput[]) => void;
	persistAnnotations: (ids: number[]) => Promise<void>;
	deleteAnnotation: (annotationId: number) => Promise<boolean>;
	deleteAnnotations: (annotationIds: number[]) => Promise<void>;
	splitAtCursor: () => Promise<void>;
	deleteInRegion: () => Promise<void>;
	moveAnnotationsVertical: (direction: "up" | "down") => Promise<void>;
	reloadAnnotations: () => Promise<void>;
	copySelection: () => void;
	cutSelection: () => Promise<void>;
	paste: () => Promise<void>;
	duplicate: () => Promise<void>;
	captureBeforeDrag: () => void;
	setError: (error: string | null) => void;
	resetTrack: () => void;
};

const patternColors = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

function getPatternColor(patternId: number): string {
	return patternColors[patternId % patternColors.length];
}

async function withUndo<T>(
	label: string,
	get: () => TrackEditorState,
	fn: () => Promise<T>,
): Promise<T> {
	const before = [...get().annotations];
	const beforeSel = [...get().selectedAnnotationIds];
	const result = await fn();
	const after = [...get().annotations];
	const afterSel = [...get().selectedAnnotationIds];
	useUndoStore.getState().push(label, before, after, beforeSel, afterSel);
	return result;
}

export const useTrackEditorStore = create<TrackEditorState>((set, get) => ({
	trackId: null,
	trackName: "",
	durationSeconds: 0,
	beatGrid: null,
	beatGridLoading: false,
	waveform: null,
	waveformLoading: false,
	annotations: [],
	annotationsLoading: false,
	patterns: [],
	patternsLoading: false,
	patternArgs: {},
	zoom: 50,
	scrollX: 0,
	playheadPosition: 0,
	isPlaying: false,
	isCompositing: false,
	selectionCursor: null,
	selectedAnnotationIds: [],
	clipboard: null,
	draggingPatternId: null,
	dragOrigin: { x: 0, y: 0 },
	isDraggingAnnotation: false,
	autoScroll: readPersistedNumber("luma:timeline-auto-scroll", 0) === 1,
	zoomY: readPersistedNumber("luma:timeline-zoom-y", 1),
	panelHeight: readPersistedNumber("luma:timeline-panel-height", 520),
	playbackRate: 1,
	error: null,

	loadTrack: async (trackId: number, trackName: string) => {
		useUndoStore.getState().clear();
		set({
			trackId,
			trackName,
			durationSeconds: 0,
			beatGrid: null,
			beatGridLoading: true,
			waveform: null,
			waveformLoading: true,
			annotations: [],
			annotationsLoading: true,
			playheadPosition: 0,
			isPlaying: false,
			selectionCursor: null,
			selectedAnnotationIds: [],
			clipboard: null,
			error: null,
		});

		const { patterns } = get();

		try {
			const beatGrid = await invoke<BeatGrid | null>("get_track_beats", {
				trackId,
			});
			set({ beatGrid, beatGridLoading: false });
		} catch (err) {
			console.error("Failed to load beat grid:", err);
			set({ beatGridLoading: false });
		}

		try {
			const waveform = await invoke<TrackWaveform>("get_track_waveform", {
				trackId,
			});
			set({
				waveform,
				waveformLoading: false,
				durationSeconds: waveform.durationSeconds,
			});
		} catch (err) {
			console.error("Failed to load waveform:", err);
			set({ waveformLoading: false });
		}

		try {
			const rawAnnotations = await invoke<TrackScore[]>("list_track_scores", {
				trackId,
			});
			const annotations = rawAnnotations.map((ann) => {
				const pattern = patterns.find((p) => p.id === ann.patternId);
				return {
					...ann,
					patternName: pattern?.name,
					patternColor: getPatternColor(ann.patternId),
				};
			});
			set({ annotations, annotationsLoading: false });
		} catch (err) {
			console.error("Failed to load annotations:", err);
			set({ annotationsLoading: false, error: String(err) });
		}

		// Load audio for playback
		get().loadTrackPlayback(trackId);
	},

	loadPatterns: async () => {
		set({ patternsLoading: true });
		try {
			const patterns = await invoke<PatternSummary[]>("list_patterns");
			const argsEntries = await Promise.all(
				patterns.map(async (p) => {
					try {
						const args = await invoke<PatternArgDef[]>("get_pattern_args", {
							id: p.id,
						});
						return [p.id, args] as const;
					} catch (err) {
						console.error("Failed to load pattern args", err);
						return [p.id, []] as const;
					}
				}),
			);
			const patternArgs = Object.fromEntries(argsEntries);
			set({ patterns, patternArgs, patternsLoading: false });
		} catch (err) {
			console.error("Failed to load patterns:", err);
			set({ patternsLoading: false, error: String(err) });
		}
	},

	loadTrackPlayback: async (trackId: number) => {
		try {
			await invoke("host_load_track", { trackId });
		} catch (err) {
			console.error("Failed to load track playback:", err);
			set({ error: `Failed to load audio playback: ${String(err)}` });
		}
	},

	play: async () => {
		const { playheadPosition, trackId } = get();
		if (trackId === null) return;
		// Seek to current position then play
		await invoke("host_seek", { seconds: playheadPosition });
		await invoke("host_play");
	},

	pause: async () => {
		const { trackId } = get();
		if (trackId === null) return;
		await invoke("host_pause");
	},

	seek: async (seconds: number) => {
		const { trackId } = get();
		if (trackId === null) return;
		await invoke("host_seek", { seconds });
	},

	syncPlaybackState: (snapshot: HostAudioSnapshot) => {
		// Host audio is simpler - no node IDs, just sync if loaded
		if (snapshot.isLoaded) {
			set({
				isPlaying: snapshot.isPlaying,
				playheadPosition: snapshot.currentTime,
			});
		}
	},

	setZoom: (zoom: number) =>
		set({ zoom: Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, zoom)) }),
	setScrollX: (scrollX: number) => set({ scrollX: Math.max(0, scrollX) }),
	setPlayheadPosition: (position: number) => {
		const { durationSeconds } = get();
		set({ playheadPosition: Math.max(0, Math.min(position, durationSeconds)) });
	},
	setIsPlaying: (isPlaying: boolean) => set({ isPlaying }),
	setIsCompositing: (isCompositing: boolean) => set({ isCompositing }),
	setSelectionCursor: (cursor: SelectionCursor | null) =>
		set({ selectionCursor: cursor }),
	setSelectedAnnotationIds: (ids: number[]) =>
		set({ selectedAnnotationIds: ids }),
	selectAnnotation: (annotationId: number | null) =>
		set({ selectedAnnotationIds: annotationId !== null ? [annotationId] : [] }),
	setDraggingPatternId: (
		patternId: number | null,
		origin?: { x: number; y: number },
	) =>
		set({ draggingPatternId: patternId, dragOrigin: origin ?? { x: 0, y: 0 } }),
	setIsDraggingAnnotation: (isDragging: boolean) =>
		set({ isDraggingAnnotation: isDragging }),
	setAutoScroll: (autoScroll: boolean) => {
		set({ autoScroll });
		try {
			localStorage.setItem("luma:timeline-auto-scroll", autoScroll ? "1" : "0");
		} catch {
			// ignore
		}
	},
	setZoomY: (zoomY: number) => {
		const clamped = Math.max(MIN_ZOOM_Y, Math.min(MAX_ZOOM_Y, zoomY));
		set({ zoomY: clamped });
		try {
			localStorage.setItem("luma:timeline-zoom-y", String(clamped));
		} catch {
			// ignore
		}
	},
	setPanelHeight: (height: number) => {
		const clamped = Math.max(200, Math.min(600, height));
		set({ panelHeight: clamped });
		try {
			localStorage.setItem("luma:timeline-panel-height", String(clamped));
		} catch {
			// ignore
		}
	},
	setPlaybackRate: async (rate: number) => {
		const clamped = Math.max(
			PLAYBACK_RATE_MIN,
			Math.min(PLAYBACK_RATE_MAX, rate),
		);
		set({ playbackRate: clamped });
		try {
			await invoke("host_set_playback_rate", { rate: clamped });
		} catch (err) {
			console.error("Failed to set playback rate:", err);
			set({ error: `Failed to set playback rate: ${String(err)}` });
		}
	},

	createAnnotation: async (input) => {
		return withUndo("Create annotation", get, async () => {
			const { trackId, annotations, patternArgs } = get();
			if (trackId === null) return null;

			const argDefs = patternArgs[input.patternId] ?? [];
			const defaultArgs = Object.fromEntries(
				argDefs.map((arg) => [arg.id, arg.defaultValue ?? {}]),
			);
			const mergedArgs = input.args ?? defaultArgs;

			try {
				// Resolve overlaps before creating
				const overlapActions = resolveOverlaps(
					annotations,
					input.startTime,
					input.endTime,
					new Set([input.zIndex]),
					new Set(),
				);
				if (overlapActions.length > 0) {
					await applyOverlapActions(overlapActions, trackId);
				}

				const annotation = await invoke<TrackScore>("create_track_score", {
					payload: { ...input, trackId, args: mergedArgs },
				});

				// Reload all annotations to reflect overlap resolution
				await get().reloadAnnotations();

				return annotation;
			} catch (err) {
				console.error("Failed to create annotation:", err);
				set({ error: String(err) });
				return null;
			}
		});
	},

	updateAnnotation: async (input) => {
		return withUndo("Edit annotation", get, async () => {
			const { annotations, patterns } = get();
			try {
				await invoke("update_track_score", {
					payload: input,
				});
				const existing = annotations.find((a) => a.id === input.id);
				if (!existing) return null;
				const next: TimelineAnnotation = {
					...existing,
					startTime: input.startTime ?? existing.startTime,
					endTime: input.endTime ?? existing.endTime,
					zIndex: input.zIndex ?? existing.zIndex,
					blendMode:
						input.blendMode == null ? existing.blendMode : input.blendMode,
					args: input.args === undefined ? existing.args : input.args,
				};
				const pattern = patterns.find((p) => p.id === next.patternId);
				const enriched: TimelineAnnotation = {
					...next,
					patternName: pattern?.name,
					patternColor: getPatternColor(next.patternId),
				};
				set({
					annotations: annotations.map((a) =>
						a.id === input.id ? enriched : a,
					),
				});
				return enriched;
			} catch (err) {
				console.error("Failed to update annotation:", err);
				set({ error: String(err) });
				return null;
			}
		});
	},

	// Synchronous local-only update for smooth dragging
	updateAnnotationsLocal: (updates) => {
		const { annotations } = get();
		const updateMap = new Map(updates.map((u) => [u.id, u]));
		set({
			annotations: annotations.map((a) => {
				const update = updateMap.get(a.id);
				if (!update) return a;
				return {
					...a,
					startTime: update.startTime ?? a.startTime,
					endTime: update.endTime ?? a.endTime,
					zIndex: update.zIndex ?? a.zIndex,
				};
			}),
		});
	},

	// Persist annotations to backend (call on drag end)
	persistAnnotations: async (ids) => {
		const { trackId, annotations } = get();
		if (trackId === null) return;
		const idsSet = new Set(ids);
		const toPersist = annotations.filter((a) => idsSet.has(a.id));
		await Promise.all(
			toPersist.map((a) =>
				invoke("update_track_score", {
					payload: {
						id: a.id,
						startTime: a.startTime,
						endTime: a.endTime,
						zIndex: a.zIndex,
					},
				}),
			),
		);

		// Resolve overlaps for each persisted annotation on its z-index
		for (const ann of toPersist) {
			const actions = resolveOverlaps(
				get().annotations,
				ann.startTime,
				ann.endTime,
				new Set([ann.zIndex]),
				new Set([ann.id]),
			);
			if (actions.length > 0) {
				await applyOverlapActions(actions, trackId);
			}
		}

		// Reload to reflect any changes from overlap resolution
		if (toPersist.length > 0) {
			await get().reloadAnnotations();
		}

		// Complete drag undo entry if one was started
		useUndoStore
			.getState()
			.completeDrag(
				"Move annotation",
				[...get().annotations],
				[...get().selectedAnnotationIds],
			);
	},

	deleteAnnotation: async (annotationId: number) => {
		return withUndo("Delete annotation", get, async () => {
			const { annotations, selectedAnnotationIds } = get();
			try {
				await invoke<void>("delete_track_score", { id: annotationId });
				set({
					annotations: annotations.filter((a) => a.id !== annotationId),
					selectedAnnotationIds: selectedAnnotationIds.filter(
						(id) => id !== annotationId,
					),
				});
				return true;
			} catch (err) {
				console.error("Failed to delete annotation:", err);
				set({ error: String(err) });
				return false;
			}
		});
	},

	deleteAnnotations: async (annotationIds: number[]) => {
		return withUndo("Delete annotations", get, async () => {
			const { annotations } = get();
			const idsSet = new Set(annotationIds);

			// Optimistically update local state first
			set({
				annotations: annotations.filter((a) => !idsSet.has(a.id)),
				selectedAnnotationIds: [],
				selectionCursor: null,
			});

			// Then delete from backend
			await Promise.all(
				annotationIds.map((id) =>
					invoke<void>("delete_track_score", { id }).catch((err) =>
						console.error(`Failed to delete annotation ${id}:`, err),
					),
				),
			);
		});
	},

	reloadAnnotations: async () => {
		const { trackId, patterns } = get();
		if (trackId === null) return;
		const rawAnnotations = await invoke<TrackScore[]>("list_track_scores", {
			trackId,
		});
		const annotations = rawAnnotations.map((ann) => {
			const pattern = patterns.find((p) => p.id === ann.patternId);
			return {
				...ann,
				patternName: pattern?.name,
				patternColor: getPatternColor(ann.patternId),
			};
		});
		set({ annotations });
	},

	splitAtCursor: async () => {
		return withUndo("Split", get, async () => {
			const { trackId, selectionCursor, annotations } = get();
			if (trackId === null || !selectionCursor) return;

			const splitTime = selectionCursor.startTime;

			// Determine affected rows from cursor
			const sortedZ = Array.from(
				new Set(annotations.map((a) => a.zIndex)),
			).sort((a, b) => a - b);
			const zRowsDesc = [...sortedZ].sort((a, b) => b - a);

			const minRow = Math.min(
				selectionCursor.trackRow,
				selectionCursor.trackRowEnd ?? selectionCursor.trackRow,
			);
			const maxRow = Math.max(
				selectionCursor.trackRow,
				selectionCursor.trackRowEnd ?? selectionCursor.trackRow,
			);

			const affectedZIndexes = new Set<number>();
			for (let r = minRow; r <= maxRow; r++) {
				if (r < zRowsDesc.length) affectedZIndexes.add(zRowsDesc[r]);
			}

			// Find annotations that straddle the split point
			const toSplit = annotations.filter(
				(ann) =>
					affectedZIndexes.has(ann.zIndex) &&
					ann.startTime < splitTime &&
					ann.endTime > splitTime,
			);

			if (toSplit.length === 0) return;

			const newIds: number[] = [];

			for (const ann of toSplit) {
				const leftDuration = splitTime - ann.startTime;
				const rightDuration = ann.endTime - splitTime;

				// Skip if either half would be too short
				if (
					leftDuration < MIN_ANNOTATION_DURATION ||
					rightDuration < MIN_ANNOTATION_DURATION
				)
					continue;

				// Trim original to left half
				await invoke("update_track_score", {
					payload: { id: ann.id, endTime: splitTime },
				});

				// Create right half
				const created = await invoke<TrackScore>("create_track_score", {
					payload: {
						trackId,
						patternId: ann.patternId,
						startTime: splitTime,
						endTime: ann.endTime,
						zIndex: ann.zIndex,
						blendMode: ann.blendMode,
						args: (ann.args as Record<string, unknown>) ?? {},
					},
				});
				newIds.push(created.id);
			}

			await get().reloadAnnotations();
			set({ selectedAnnotationIds: newIds });
		});
	},

	deleteInRegion: async () => {
		return withUndo("Delete region", get, async () => {
			const { trackId, selectionCursor, annotations } = get();
			if (
				trackId === null ||
				!selectionCursor ||
				selectionCursor.endTime === null
			)
				return;

			const rangeStart = Math.min(
				selectionCursor.startTime,
				selectionCursor.endTime,
			);
			const rangeEnd = Math.max(
				selectionCursor.startTime,
				selectionCursor.endTime,
			);

			// Determine affected rows
			const sortedZ = Array.from(
				new Set(annotations.map((a) => a.zIndex)),
			).sort((a, b) => a - b);
			const zRowsDesc = [...sortedZ].sort((a, b) => b - a);

			const minRow = Math.min(
				selectionCursor.trackRow,
				selectionCursor.trackRowEnd ?? selectionCursor.trackRow,
			);
			const maxRow = Math.max(
				selectionCursor.trackRow,
				selectionCursor.trackRowEnd ?? selectionCursor.trackRow,
			);

			const affectedZIndexes = new Set<number>();
			for (let r = minRow; r <= maxRow; r++) {
				if (r < zRowsDesc.length) affectedZIndexes.add(zRowsDesc[r]);
			}

			const actions = resolveOverlaps(
				annotations,
				rangeStart,
				rangeEnd,
				affectedZIndexes,
				new Set(),
			);

			if (actions.length === 0) return;

			await applyOverlapActions(actions, trackId);
			await get().reloadAnnotations();
			set({ selectedAnnotationIds: [], selectionCursor: null });
		});
	},

	moveAnnotationsVertical: async (direction) => {
		return withUndo("Move to lane", get, async () => {
			const { trackId, annotations, selectedAnnotationIds, selectionCursor } =
				get();
			if (trackId === null || selectedAnnotationIds.length === 0) return;

			const selected = annotations.filter((a) =>
				selectedAnnotationIds.includes(a.id),
			);
			if (selected.length === 0) return;

			// Build row mapping: row 0 = highest z (visually top)
			const sortedZ = Array.from(
				new Set(annotations.map((a) => a.zIndex)),
			).sort((a, b) => a - b);
			const zRowsDesc = [...sortedZ].sort((a, b) => b - a);

			// Shift each annotation's row by 1, preserving relative positions
			if (direction === "up") {
				const highestZ = zRowsDesc[0];
				for (const ann of selected) {
					const row = zRowsDesc.indexOf(ann.zIndex);
					const targetZ = row <= 0 ? highestZ + 1 : zRowsDesc[row - 1];
					await invoke("update_track_score", {
						payload: { id: ann.id, zIndex: targetZ },
					});
				}
			} else {
				const lowestZ = zRowsDesc[zRowsDesc.length - 1];
				for (const ann of selected) {
					const row = zRowsDesc.indexOf(ann.zIndex);
					const targetZ =
						row >= zRowsDesc.length - 1 ? lowestZ - 1 : zRowsDesc[row + 1];
					await invoke("update_track_score", {
						payload: { id: ann.id, zIndex: targetZ },
					});
				}
			}

			await get().reloadAnnotations();

			// Resolve overlaps at the new position
			const reloaded = get().annotations;
			const movedAnns = reloaded.filter((a) =>
				selectedAnnotationIds.includes(a.id),
			);
			for (const ann of movedAnns) {
				const actions = resolveOverlaps(
					get().annotations,
					ann.startTime,
					ann.endTime,
					new Set([ann.zIndex]),
					new Set([ann.id]),
				);
				if (actions.length > 0) {
					await applyOverlapActions(actions, trackId);
				}
			}

			await get().reloadAnnotations();

			// Update selection cursor to the actual row of moved annotations
			if (selectionCursor) {
				const finalAnnotations = get().annotations;
				const movedFinal = finalAnnotations.filter((a) =>
					selectedAnnotationIds.includes(a.id),
				);
				if (movedFinal.length > 0) {
					// Compute row map the same way the component does
					const newSortedZ = Array.from(
						new Set(finalAnnotations.map((a) => a.zIndex)),
					).sort((a, b) => a - b);
					const maxRow = Math.max(0, newSortedZ.length - 1);
					const movedZ = movedFinal[0].zIndex;
					const zIdx = newSortedZ.indexOf(movedZ);
					const actualRow = zIdx >= 0 ? maxRow - zIdx : 0;

					set({
						selectionCursor: {
							...selectionCursor,
							trackRow: actualRow,
							trackRowEnd: null,
						},
					});
				}
			}
		});
	},

	copySelection: () => {
		const { annotations, selectedAnnotationIds, selectionCursor } = get();
		if (!selectionCursor || selectedAnnotationIds.length === 0) return;

		const selectedAnns = annotations.filter((a) =>
			selectedAnnotationIds.includes(a.id),
		);
		if (selectedAnns.length === 0) return;

		// Normalize selection bounds (handle right-to-left selection)
		const selectionStart =
			selectionCursor.endTime !== null
				? Math.min(selectionCursor.startTime, selectionCursor.endTime)
				: selectionCursor.startTime;
		const selectionEnd =
			selectionCursor.endTime !== null
				? Math.max(selectionCursor.startTime, selectionCursor.endTime)
				: Math.max(...selectedAnns.map((a) => a.endTime));

		const items: ClipboardItem[] = selectedAnns.map((a) => ({
			patternId: a.patternId,
			offsetFromStart: a.startTime - selectionStart,
			duration: a.endTime - a.startTime,
			zIndex: a.zIndex,
			blendMode: a.blendMode,
			args: (a.args as Record<string, unknown> | undefined) ?? {},
		}));

		set({
			clipboard: {
				items,
				totalDuration: selectionEnd - selectionStart,
			},
		});
	},

	cutSelection: async () => {
		return withUndo("Cut", get, async () => {
			const { selectedAnnotationIds } = get();
			if (selectedAnnotationIds.length === 0) return;

			// First populate the clipboard from the current selection
			get().copySelection();

			// Only delete if clipboard was successfully set
			if (!get().clipboard) return;

			// Delete directly without nesting another withUndo
			const { annotations } = get();
			const idsSet = new Set(selectedAnnotationIds);
			set({
				annotations: annotations.filter((a) => !idsSet.has(a.id)),
				selectedAnnotationIds: [],
				selectionCursor: null,
			});
			await Promise.all(
				selectedAnnotationIds.map((id) =>
					invoke<void>("delete_track_score", { id }).catch((err) =>
						console.error(`Failed to delete annotation ${id}:`, err),
					),
				),
			);
		});
	},

	paste: async () => {
		return withUndo("Paste", get, async () => {
			const {
				clipboard,
				selectionCursor,
				trackId,
				patterns,
				durationSeconds,
				annotations,
			} = get();
			if (!clipboard || !selectionCursor || trackId === null) return;

			// Determine z-index offset so we can paste into a different track row
			const uniqueZ = Array.from(
				new Set(annotations.map((a) => a.zIndex)),
			).sort((a, b) => a - b);
			const zRowsDesc = [...uniqueZ].sort((a, b) => b - a);
			const rowToZ = (row: number): number => {
				if (zRowsDesc.length === 0) return 0;
				if (row < zRowsDesc.length) return zRowsDesc[row];
				// Extend rows below by stepping lower z-indices
				const lowest = zRowsDesc[zRowsDesc.length - 1];
				const extra = row - (zRowsDesc.length - 1);
				return lowest - extra;
			};

			const sourceBaseZ = Math.min(
				...clipboard.items.map((item) => item.zIndex),
			);
			const targetRow = Math.max(0, selectionCursor.trackRow);
			const targetBaseZ = rowToZ(targetRow);
			const zOffset = targetBaseZ - sourceBaseZ;

			// Normalize paste position (handle right-to-left selection)
			const pasteStart =
				selectionCursor.endTime !== null
					? Math.min(selectionCursor.startTime, selectionCursor.endTime)
					: selectionCursor.startTime;
			const pasteEnd = pasteStart + clipboard.totalDuration;

			// Get all unique (shifted) zIndexes the paste will occupy
			const clipboardZIndexes = new Set(
				clipboard.items.map((item) => item.zIndex + zOffset),
			);

			// Clear the paste region using shared overlap resolution
			const overlapActions = resolveOverlaps(
				annotations,
				pasteStart,
				pasteEnd,
				clipboardZIndexes,
				new Set(),
			);

			if (overlapActions.length > 0) {
				await applyOverlapActions(overlapActions, trackId);
			}

			// Reload annotations after clearing
			await get().reloadAnnotations();

			// Now paste the new annotations
			const newAnnotationIds: number[] = [];

			for (const item of clipboard.items) {
				const startTime = pasteStart + item.offsetFromStart;
				const endTime = startTime + item.duration;
				const targetZIndex = item.zIndex + zOffset;

				// Skip if would go past track end
				if (endTime > durationSeconds) continue;

				try {
					const annotation = await invoke<TrackScore>("create_track_score", {
						payload: {
							trackId,
							patternId: item.patternId,
							startTime,
							endTime,
							zIndex: targetZIndex,
							blendMode: item.blendMode,
							args: item.args ?? {},
						},
					});
					const pattern = patterns.find((p) => p.id === annotation.patternId);
					const enriched: TimelineAnnotation = {
						...annotation,
						patternName: pattern?.name,
						patternColor: getPatternColor(annotation.patternId),
					};
					// Add to annotations incrementally
					set({ annotations: [...get().annotations, enriched] });
					newAnnotationIds.push(annotation.id);
				} catch (err) {
					console.error("Failed to create annotation during paste:", err);
				}
			}

			// Update cursor to span the pasted region and select new annotations
			set({
				selectionCursor: {
					trackRow: selectionCursor.trackRow,
					trackRowEnd: selectionCursor.trackRowEnd ?? null,
					startTime: pasteStart,
					endTime: pasteEnd,
				},
				selectedAnnotationIds: newAnnotationIds,
			});
		});
	},

	duplicate: async () => {
		const { selectionCursor } = get();
		if (!selectionCursor) return;

		// First copy the current selection
		get().copySelection();

		const { clipboard } = get();
		if (!clipboard) return;

		// Calculate paste position at end of current cursor
		const cursorEnd =
			selectionCursor.endTime !== null
				? Math.max(selectionCursor.startTime, selectionCursor.endTime)
				: selectionCursor.startTime;

		// Temporarily update cursor to paste position
		set({
			selectionCursor: {
				...selectionCursor,
				startTime: cursorEnd,
				endTime: null,
			},
		});

		// Paste at the new position
		await get().paste();
	},

	captureBeforeDrag: () => {
		useUndoStore
			.getState()
			.captureBeforeDrag(
				[...get().annotations],
				[...get().selectedAnnotationIds],
			);
	},

	setError: (error: string | null) => set({ error }),

	resetTrack: () => {
		useUndoStore.getState().clear();
		set({
			trackId: null,
			trackName: "",
			durationSeconds: 0,
			beatGrid: null,
			beatGridLoading: false,
			waveform: null,
			waveformLoading: false,
			annotations: [],
			annotationsLoading: false,
			playheadPosition: 0,
			isPlaying: false,
			isCompositing: false,
			selectionCursor: null,
			selectedAnnotationIds: [],
			clipboard: null,
			draggingPatternId: null,
			isDraggingAnnotation: false,
			error: null,
		});
	},
}));
