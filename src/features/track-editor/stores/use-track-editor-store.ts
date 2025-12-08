import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	BeatGrid,
	BlendMode,
	HostAudioSnapshot,
	PatternArgDef,
	PatternSummary,
	TrackAnnotation as TrackAnnotationBinding,
} from "@/bindings/schema";
import { MAX_ZOOM, MIN_ZOOM } from "../utils/timeline-constants";

// Re-export with the correct type from bindings
export type TrackAnnotation = TrackAnnotationBinding;

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

export type TimelineAnnotation = TrackAnnotation & {
	patternName?: string;
	patternColor?: string;
};

export type SelectionCursor = {
	trackRow: number;
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
	setDraggingPatternId: (patternId: number | null) => void;
	createAnnotation: (
		input: Omit<CreateAnnotationInput, "trackId">,
	) => Promise<TrackAnnotation | null>;
	updateAnnotation: (
		input: UpdateAnnotationInput,
	) => Promise<TrackAnnotation | null>;
	updateAnnotationsLocal: (updates: UpdateAnnotationInput[]) => void;
	persistAnnotations: (ids: number[]) => Promise<void>;
	deleteAnnotation: (annotationId: number) => Promise<boolean>;
	deleteAnnotations: (annotationIds: number[]) => Promise<void>;
	copySelection: () => void;
	paste: () => Promise<void>;
	duplicate: () => Promise<void>;
	setError: (error: string | null) => void;
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
	error: null,

	loadTrack: async (trackId: number, trackName: string) => {
		set({
			trackId,
			trackName,
			beatGridLoading: true,
			waveformLoading: true,
			annotationsLoading: true,
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
			const rawAnnotations = await invoke<TrackAnnotation[]>(
				"list_annotations",
				{ trackId },
			);
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
		const { playheadPosition } = get();
		// Seek to current position then play
		await invoke("host_seek", { seconds: playheadPosition });
		await invoke("host_play");
	},

	pause: async () => {
		await invoke("host_pause");
	},

	seek: async (seconds: number) => {
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
	setDraggingPatternId: (patternId: number | null) =>
		set({ draggingPatternId: patternId }),

	createAnnotation: async (input) => {
		const { trackId, patterns, annotations, patternArgs } = get();
		if (trackId === null) return null;

		const argDefs = patternArgs[input.patternId] ?? [];
		const defaultArgs = Object.fromEntries(
			argDefs.map((arg) => [arg.id, arg.defaultValue ?? {}]),
		);
		const mergedArgs = input.args ?? defaultArgs;

		try {
			const annotation = await invoke<TrackAnnotation>("create_annotation", {
				input: { ...input, trackId, args: mergedArgs },
			});
			const pattern = patterns.find((p) => p.id === annotation.patternId);
			const enriched: TimelineAnnotation = {
				...annotation,
				patternName: pattern?.name,
				patternColor: getPatternColor(annotation.patternId),
			};
			set({ annotations: [...annotations, enriched] });
			return annotation;
		} catch (err) {
			console.error("Failed to create annotation:", err);
			set({ error: String(err) });
			return null;
		}
	},

	updateAnnotation: async (input) => {
		const { annotations, patterns } = get();
		try {
			const updated = await invoke<TrackAnnotation>("update_annotation", {
				input,
			});
			const pattern = patterns.find((p) => p.id === updated.patternId);
			const enriched: TimelineAnnotation = {
				...updated,
				patternName: pattern?.name,
				patternColor: getPatternColor(updated.patternId),
			};
			set({
				annotations: annotations.map((a) =>
					a.id === updated.id ? enriched : a,
				),
			});
			return updated;
		} catch (err) {
			console.error("Failed to update annotation:", err);
			set({ error: String(err) });
			return null;
		}
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
		const { annotations } = get();
		const toPersist = annotations.filter((a) => ids.includes(a.id));
		await Promise.all(
			toPersist.map((a) =>
				invoke("update_annotation", {
					input: {
						id: a.id,
						startTime: a.startTime,
						endTime: a.endTime,
						zIndex: a.zIndex,
					},
				}),
			),
		);
	},

	deleteAnnotation: async (annotationId: number) => {
		const { annotations, selectedAnnotationIds } = get();
		try {
			await invoke<void>("delete_annotation", { annotationId });
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
	},

	deleteAnnotations: async (annotationIds: number[]) => {
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
				invoke<void>("delete_annotation", { annotationId: id }).catch((err) =>
					console.error(`Failed to delete annotation ${id}:`, err),
				),
			),
		);
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

	paste: async () => {
		const {
			clipboard,
			selectionCursor,
			trackId,
			patterns,
			durationSeconds,
			annotations,
		} = get();
		if (!clipboard || !selectionCursor || trackId === null) return;

		// Normalize paste position (handle right-to-left selection)
		const pasteStart =
			selectionCursor.endTime !== null
				? Math.min(selectionCursor.startTime, selectionCursor.endTime)
				: selectionCursor.startTime;
		const pasteEnd = pasteStart + clipboard.totalDuration;

		// Get all unique zIndexes from clipboard items
		const clipboardZIndexes = new Set(
			clipboard.items.map((item) => item.zIndex),
		);

		// Clear the paste region: delete or trim annotations that overlap
		const affectedAnnotations = annotations.filter((ann) => {
			// Only consider annotations on the same z-indexes we're pasting to
			if (!clipboardZIndexes.has(ann.zIndex)) return false;

			// Check if annotation overlaps with paste region
			return ann.startTime < pasteEnd && ann.endTime > pasteStart;
		});

		for (const ann of affectedAnnotations) {
			const fullyContained =
				ann.startTime >= pasteStart && ann.endTime <= pasteEnd;
			const startsBeforeEndsInside =
				ann.startTime < pasteStart &&
				ann.endTime > pasteStart &&
				ann.endTime <= pasteEnd;
			const startsInsideEndsAfter =
				ann.startTime >= pasteStart &&
				ann.startTime < pasteEnd &&
				ann.endTime > pasteEnd;
			const spansEntireRegion =
				ann.startTime < pasteStart && ann.endTime > pasteEnd;

			if (fullyContained) {
				// Delete annotations fully within paste region
				await invoke<void>("delete_annotation", { annotationId: ann.id });
			} else if (startsBeforeEndsInside) {
				// Trim right side: annotation starts before paste region and ends inside it
				await invoke("update_annotation", {
					input: {
						id: ann.id,
						endTime: pasteStart,
					},
				});
			} else if (startsInsideEndsAfter) {
				// Trim left side: annotation starts inside paste region and ends after it
				await invoke("update_annotation", {
					input: {
						id: ann.id,
						startTime: pasteEnd,
					},
				});
			} else if (spansEntireRegion) {
				// Split annotation: it spans the entire paste region
				// Keep the left part by trimming the original
				await invoke("update_annotation", {
					input: {
						id: ann.id,
						endTime: pasteStart,
					},
				});
				// Create the right part as a new annotation
				await invoke<TrackAnnotation>("create_annotation", {
					input: {
						trackId,
						patternId: ann.patternId,
						startTime: pasteEnd,
						endTime: ann.endTime,
						zIndex: ann.zIndex,
					},
				});
			}
		}

		// Reload annotations after clearing
		const rawAnnotations = await invoke<TrackAnnotation[]>("list_annotations", {
			trackId,
		});
		const updatedAnnotations = rawAnnotations.map((ann) => {
			const pattern = patterns.find((p) => p.id === ann.patternId);
			return {
				...ann,
				patternName: pattern?.name,
				patternColor: getPatternColor(ann.patternId),
			};
		});
		set({ annotations: updatedAnnotations });

		// Now paste the new annotations
		const newAnnotationIds: number[] = [];

		for (const item of clipboard.items) {
			const startTime = pasteStart + item.offsetFromStart;
			const endTime = startTime + item.duration;

			// Skip if would go past track end
			if (endTime > durationSeconds) continue;

			try {
				const annotation = await invoke<TrackAnnotation>("create_annotation", {
					input: {
						trackId,
						patternId: item.patternId,
						startTime,
						endTime,
						zIndex: item.zIndex,
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
				startTime: pasteStart,
				endTime: pasteEnd,
			},
			selectedAnnotationIds: newAnnotationIds,
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

	setError: (error: string | null) => set({ error }),
}));
