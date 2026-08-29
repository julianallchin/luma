import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	BeatGrid,
	BlendMode,
	HostAudioSnapshot,
	PatternArgDef,
	PatternSummary,
	TrackEditResult,
	TrackScore as TrackScoreBinding,
} from "@/bindings/schema";
import { LatestRequestGate } from "@/shared/lib/latest-request-gate";
import { useReviewStatusStore } from "../agent/use-review-status-store";
import {
	applyOverlapActions,
	resolveOverlaps,
} from "../utils/overlap-resolution";
import {
	hydrateTrackReplacement,
	replaceTrackScoreDocument,
} from "../utils/replace-track-score-document";
import {
	MAX_ZOOM,
	MAX_ZOOM_Y,
	MIN_ANNOTATION_DURATION,
	MIN_ZOOM,
	MIN_ZOOM_Y,
} from "../utils/timeline-constants";
import {
	deleteTrackScore,
	updateTrackScore,
} from "../utils/track-score-commands";
import { useAnnotationPreviewStore } from "./use-annotation-preview-store";
import { useUndoStore } from "./use-undo-store";

/** Deep-equal for annotation args, insensitive to JSON key ordering. */
export function argsEqual(a: unknown, b: unknown): boolean {
	if (a === b) return true;
	if (typeof a !== typeof b || a == null || b == null) return false;
	if (typeof a !== "object") return a === b;
	const aObj = a as Record<string, unknown>;
	const bObj = b as Record<string, unknown>;
	const keys = Object.keys(aObj);
	if (keys.length !== Object.keys(bObj).length) return false;
	return keys.every((k) => argsEqual(aObj[k], bObj[k]));
}

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
	trackId: string;
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
	trackId: string;
	patternId: string;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode?: BlendMode | null;
	createdAt?: string;
	updatedAt?: string;
	args?: Record<string, unknown>;
};

export type UpdateAnnotationInput = {
	id: string;
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
	patternId: string;
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

export type ScoreState = "loading" | "loaded" | "no_score";

type TrackEditorState = {
	trackId: string | null;
	venueId: string | null;
	scoreId: string | null;
	readOnly: boolean;
	trackName: string;
	scoreState: ScoreState;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	beatGridLoading: boolean;
	waveform: TrackWaveform | null;
	waveformLoading: boolean;
	annotations: TimelineAnnotation[];
	annotationsLoading: boolean;
	patterns: PatternSummary[];
	patternsLoading: boolean;
	patternArgs: Record<string, PatternArgDef[]>;
	zoom: number;
	scrollX: number;
	playheadPosition: number;
	isPlaying: boolean;
	isCompositing: boolean;
	selectionCursor: SelectionCursor | null;
	selectedAnnotationIds: string[];
	clipboard: Clipboard | null;
	isDraggingAnnotation: boolean;
	autoScroll: boolean;
	zoomY: number;
	panelHeight: number;
	playbackRate: number;
	loopRegion: { start: number; end: number } | null;
	error: string | null;

	loadTrack: (
		trackId: string,
		trackName: string,
		venueId: string,
		scoreId: string,
		readOnly: boolean,
	) => Promise<void>;
	startFreshScore: () => void;
	loadPatterns: () => Promise<void>;
	loadTrackPlayback: (trackId: string) => Promise<void>;
	play: () => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
	syncPlaybackState: (snapshot: HostAudioSnapshot) => void;
	setZoom: (zoom: number) => void;
	setScrollX: (scrollX: number) => void;
	setPlayheadPosition: (position: number) => void;
	setIsPlaying: (isPlaying: boolean) => void;
	setIsCompositing: (isCompositing: boolean) => void;
	setLoopRegion: (start: number, end: number) => Promise<void>;
	clearLoopRegion: () => Promise<void>;
	setSelectionCursor: (cursor: SelectionCursor | null) => void;
	setSelectedAnnotationIds: (ids: string[]) => void;
	selectAnnotation: (annotationId: string | null) => void;
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
	updateAnnotationsBatch: (inputs: UpdateAnnotationInput[]) => Promise<void>;
	updateArgs: (argId: string, value: Record<string, unknown> | number) => void;
	updateAnnotationsLocal: (updates: UpdateAnnotationInput[]) => void;
	persistAnnotations: (ids: string[]) => Promise<void>;
	deleteAnnotation: (annotationId: string) => Promise<boolean>;
	deleteAnnotations: (annotationIds: string[]) => Promise<void>;
	splitAtCursor: () => Promise<void>;
	deleteInRegion: () => Promise<void>;
	moveAnnotationsVertical: (direction: "up" | "down") => Promise<void>;
	reloadAnnotations: () => Promise<boolean>;
	copySelection: () => void;
	cutSelection: () => Promise<void>;
	paste: () => Promise<void>;
	duplicate: () => Promise<void>;
	captureBeforeDrag: () => void;
	cloneAnnotationsInPlace: (ids: string[]) => Promise<TimelineAnnotation[]>;
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

function getPatternColor(patternId: string): string {
	// Simple hash of the string ID to pick a stable color
	let hash = 0;
	for (let i = 0; i < patternId.length; i++) {
		hash = (hash * 31 + patternId.charCodeAt(i)) | 0;
	}
	return patternColors[Math.abs(hash) % patternColors.length];
}

// Monotonic ownership token for the asynchronous track loader. Every new load
// (and reset) retires all older work, so a late response can never write into a
// newer track/venue/score identity.
let trackLoadGeneration = 0;
const annotationAuthorityGate = new LatestRequestGate();

function ownsTrackLoad(
	get: () => TrackEditorState,
	generation: number,
	trackId: string,
	venueId: string,
	scoreId: string,
): boolean {
	const state = get();
	return (
		generation === trackLoadGeneration &&
		state.trackId === trackId &&
		state.venueId === venueId &&
		state.scoreId === scoreId
	);
}

async function withUndo<T>(
	label: string,
	get: () => TrackEditorState,
	fn: (authorityTicket: number) => Promise<T>,
): Promise<T> {
	const authorityTicket = annotationAuthorityGate.issue();
	const undoEpoch = useUndoStore.getState()._epoch;
	const before = [...get().annotations];
	const beforeSel = [...get().selectedAnnotationIds];
	const result = await fn(authorityTicket);
	const after = [...get().annotations];
	const afterSel = [...get().selectedAnnotationIds];
	if (
		annotationAuthorityGate.owns(authorityTicket) &&
		useUndoStore.getState()._epoch === undoEpoch
	) {
		useUndoStore.getState().push(label, before, after, beforeSel, afterSel);
	}
	return result;
}

function ownsAnnotationAuthority(
	authorityTicket: number,
	get: () => TrackEditorState,
	trackId: string,
	venueId: string,
	scoreId: string,
): boolean {
	const state = get();
	return (
		annotationAuthorityGate.owns(authorityTicket) &&
		state.trackId === trackId &&
		state.venueId === venueId &&
		state.scoreId === scoreId
	);
}

function enrichAnnotations(
	annotations: readonly TimelineAnnotation[],
	patterns: readonly PatternSummary[],
): TimelineAnnotation[] {
	return annotations.map((annotation) => {
		const pattern = patterns.find(
			(candidate) => candidate.id === annotation.patternId,
		);
		return {
			...annotation,
			patternName: pattern?.name,
			patternColor: getPatternColor(annotation.patternId),
		};
	});
}

function draftAnnotation(
	scoreId: string,
	input: Omit<CreateAnnotationInput, "trackId">,
	args: Record<string, unknown>,
): TimelineAnnotation {
	const now = new Date().toISOString();
	return {
		id: crypto.randomUUID(),
		uid: null,
		scoreId,
		patternId: input.patternId,
		startTime: input.startTime,
		endTime: input.endTime,
		zIndex: input.zIndex,
		blendMode: input.blendMode ?? "replace",
		args,
		createdAt: input.createdAt ?? now,
		updatedAt: input.updatedAt ?? now,
	};
}

async function replaceScoreCandidate(
	scoreId: string,
	trackId: string,
	base: readonly TimelineAnnotation[],
	candidate: readonly TimelineAnnotation[],
	patterns: readonly PatternSummary[],
): Promise<{
	annotations: TimelineAnnotation[];
	idMap: TrackEditResult["idMap"];
	appliedToCurrentProjection: boolean;
}> {
	const result = await replaceTrackScoreDocument(
		scoreId,
		trackId,
		base,
		candidate,
	);
	return projectTrackEditResult(
		scoreId,
		[...base, ...candidate],
		result,
		patterns,
	);
}

function projectTrackEditResult(
	scoreId: string,
	known: readonly TimelineAnnotation[],
	result: TrackEditResult,
	patterns: readonly PatternSummary[],
): {
	annotations: TimelineAnnotation[];
	idMap: TrackEditResult["idMap"];
	appliedToCurrentProjection: boolean;
} {
	return {
		annotations: enrichAnnotations(
			hydrateTrackReplacement(scoreId, known, result),
			patterns,
		),
		idMap: result.idMap,
		appliedToCurrentProjection: result.appliedToCurrentProjection,
	};
}

function acceptTrackProjection(appliedToCurrentProjection: boolean): void {
	if (appliedToCurrentProjection) return;
	// Main advanced after this operation committed but before its replay was
	// observed. The returned document is authoritative, but the old local undo
	// base no longer describes it.
	useUndoStore.getState().clear();
}

/** Return trackId & venueId when both are set, or null to bail out. */
function requireContext(get: () => TrackEditorState) {
	const { trackId, venueId, scoreId } = get();
	if (trackId === null || venueId === null || scoreId === null) return null;
	return { trackId, venueId, scoreId };
}

/**
 * If the cursor describes a time range AND every explicitly selected annotation
 * lies inside that range × row band, return the region info so copy/cut can
 * clip partial overlaps. Returns null for object-mode selections (e.g.
 * shift-clicked annotations across different rows or times) so those fall back
 * to whole-annotation handling.
 */
function getRegionInfo(
	selectionCursor: SelectionCursor | null,
	annotations: TimelineAnnotation[],
	selectedAnnotationIds: string[],
): {
	regionStart: number;
	regionEnd: number;
	affectedZIndexes: Set<number>;
} | null {
	if (!selectionCursor || selectionCursor.endTime === null) return null;

	const regionStart = Math.min(
		selectionCursor.startTime,
		selectionCursor.endTime,
	);
	const regionEnd = Math.max(
		selectionCursor.startTime,
		selectionCursor.endTime,
	);

	const sortedZ = Array.from(new Set(annotations.map((a) => a.zIndex))).sort(
		(a, b) => a - b,
	);
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
		// Row 0 is the empty top lane; occupied rows start at 1
		const zIdx = r - 1;
		if (zIdx >= 0 && zIdx < zRowsDesc.length)
			affectedZIndexes.add(zRowsDesc[zIdx]);
	}

	const annById = new Map(annotations.map((a) => [a.id, a]));
	for (const id of selectedAnnotationIds) {
		const a = annById.get(id);
		if (!a) continue;
		if (
			!affectedZIndexes.has(a.zIndex) ||
			a.endTime <= regionStart ||
			a.startTime >= regionEnd
		) {
			return null;
		}
	}

	return { regionStart, regionEnd, affectedZIndexes };
}

// --- Live arg edits ---------------------------------------------------------
// During a drag the picker emits 60-120×/sec. Writing the store on each emit
// re-renders the whole timeline (73 clips) and starves the visualizer. Instead
// we keep the live value off React entirely: accumulate it in `_liveArgs`, push
// it straight to the backend compositor (one rAF coalesces a frame's emits → the
// rig updates via the visualizer with ZERO React re-renders), and commit to the
// store + persist only on the trailing edge. The picker shows its own internal
// state during the drag, so the stale store value is invisible.
let _argTimer: ReturnType<typeof setTimeout> | null = null;
let _argSnapshot: {
	selection: string[];
} | null = null;
/** Serializes arg-edit persists. Each flush applies against the track revision
 * its predecessor produced, so back-to-back tweaks (a color picker fires one
 * flush per click) can't conflict with their own in-flight predecessor. */
let _argFlushChain: Promise<void> = Promise.resolve();
const _liveArgs = new Map<string, Record<string, unknown>>();
let _liveRaf: number | null = null;
let _lastLivePreviewTs = 0;

/** rAF-coalesced live composite straight to the backend — no store write. The
 * edited annotations' heatmap previews are regenerated too (throttled), which is
 * safe now that the timeline reads bitmaps imperatively (no re-render). */
function scheduleLiveComposite() {
	if (_liveRaf !== null) return;
	_liveRaf = requestAnimationFrame(() => {
		_liveRaf = null;
		const ctx = requireContext(() => useTrackEditorStore.getState());
		if (!ctx) return;
		const { annotations, selectedAnnotationIds } =
			useTrackEditorStore.getState();
		const merged = annotations.map((a) =>
			_liveArgs.has(a.id) ? { ...a, args: _liveArgs.get(a.id) } : a,
		);
		invoke("composite_track", {
			scoreId: ctx.scoreId,
			skipCache: false,
			annotations: merged.map((a) => ({
				id: a.id,
				patternId: a.patternId,
				startTime: a.startTime,
				endTime: a.endTime,
				zIndex: a.zIndex,
				blendMode: a.blendMode,
				args: a.args ?? {},
			})),
		}).catch((err) => console.error("[live] composite failed:", err));

		// Live heatmap preview for the edited annotations (throttled ~12fps; the
		// store coalesces per-id so it never backs up).
		const nowTs = performance.now();
		if (nowTs - _lastLivePreviewTs > 80) {
			_lastLivePreviewTs = nowTs;
			const sel = new Set(selectedAnnotationIds);
			const updatePreview = useAnnotationPreviewStore.getState().updatePreview;
			for (const a of merged) {
				if (!sel.has(a.id)) continue;
				updatePreview(ctx.trackId, ctx.venueId, {
					id: a.id,
					patternId: a.patternId,
					startTime: a.startTime,
					endTime: a.endTime,
					args: (a.args ?? {}) as Record<string, unknown>,
				});
			}
		}
	});
}

async function flushPendingArgs(): Promise<void> {
	if (_argTimer) {
		clearTimeout(_argTimer);
		_argTimer = null;
	}
	if (!_argSnapshot) return;
	const beforeSelection = _argSnapshot.selection;
	const liveArgs = new Map(_liveArgs);
	_argSnapshot = null;
	_liveArgs.clear();
	const run = _argFlushChain.then(() =>
		persistArgEdit(beforeSelection, liveArgs),
	);
	_argFlushChain = run;
	return run;
}

/** Runs strictly after any earlier flush, so the store's annotations — read
 * here, not at interaction time — already reflect the predecessor's apply and
 * hash to the backend's current track revision. */
async function persistArgEdit(
	beforeSelection: string[],
	liveArgs: Map<string, Record<string, unknown>>,
): Promise<void> {
	const editor = useTrackEditorStore.getState();
	const { trackId, venueId, scoreId, patterns, selectedAnnotationIds } = editor;
	const base = editor.annotations;
	const candidate = base.map((annotation) => {
		const args = liveArgs.get(annotation.id);
		return args ? { ...annotation, args } : annotation;
	});
	if (!trackId || !venueId || !scoreId) return;
	const authorityTicket = annotationAuthorityGate.issue();

	try {
		const applied = await replaceScoreCandidate(
			scoreId,
			trackId,
			base,
			candidate,
			patterns,
		);
		if (
			!ownsAnnotationAuthority(
				authorityTicket,
				() => useTrackEditorStore.getState(),
				trackId,
				venueId,
				scoreId,
			)
		) {
			return;
		}
		acceptTrackProjection(applied.appliedToCurrentProjection);
		useTrackEditorStore.setState({
			annotations: applied.annotations,
			error: null,
		});
		if (applied.appliedToCurrentProjection) {
			useUndoStore
				.getState()
				.push(
					"Edit arg",
					base,
					applied.annotations,
					beforeSelection,
					selectedAnnotationIds,
				);
		}
	} catch (error) {
		const current = useTrackEditorStore.getState();
		if (
			ownsAnnotationAuthority(
				authorityTicket,
				() => current,
				trackId,
				venueId,
				scoreId,
			)
		) {
			useTrackEditorStore.setState({
				error: `Failed to persist argument edit: ${String(error)}`,
			});
			void current.reloadAnnotations().catch(() => undefined);
		}
	}
}

export const useTrackEditorStore = create<TrackEditorState>((set, get) => ({
	trackId: null,
	venueId: null,
	scoreId: null,
	readOnly: false,
	trackName: "",
	scoreState: "loading" as ScoreState,
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
	isDraggingAnnotation: false,
	autoScroll: readPersistedNumber("luma:timeline-auto-scroll", 0) === 1,
	zoomY: readPersistedNumber("luma:timeline-zoom-y", 1),
	panelHeight: readPersistedNumber("luma:timeline-panel-height", 520),
	playbackRate: 1,
	loopRegion: null,
	error: null,

	loadTrack: async (
		trackId: string,
		trackName: string,
		venueId: string,
		scoreId: string,
		readOnly: boolean,
	) => {
		const generation = ++trackLoadGeneration;
		annotationAuthorityGate.supersede();
		const stillCurrent = () =>
			ownsTrackLoad(get, generation, trackId, venueId, scoreId);

		// Clean up the previous track's backend resources if switching tracks
		const previous = get();
		const prevTrackId = previous.trackId;
		if (
			prevTrackId !== null &&
			previous.scoreId !== null &&
			prevTrackId !== trackId
		) {
			invoke("leave_track", {
				scoreId: previous.scoreId,
			}).catch((err) => console.error("leave_track failed:", err));
			useAnnotationPreviewStore.getState().clear();
		}

		useUndoStore.getState().clear();
		// Opening a track counts as "the user has reviewed it" — clear the
		// blue dot if auto-light flagged this score for review.
		useReviewStatusStore.getState().clearNeedsReview(trackId, venueId);
		set({
			trackId,
			venueId,
			scoreId,
			readOnly,
			trackName,
			scoreState: "loading",
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
			loopRegion: null,
			error: null,
		});

		const { patterns } = get();

		try {
			const beatGrid = await invoke<BeatGrid | null>("get_track_beats", {
				trackId,
			});
			if (!stillCurrent()) return;
			set({ beatGrid, beatGridLoading: false });
		} catch (err) {
			if (!stillCurrent()) return;
			console.error("Failed to load beat grid:", err);
			set({ beatGridLoading: false });
		}

		try {
			const waveform = await invoke<TrackWaveform>("get_track_waveform", {
				trackId,
			});
			if (!stillCurrent()) return;
			set({
				waveform,
				waveformLoading: false,
				durationSeconds: waveform.durationSeconds,
			});
		} catch (err) {
			if (!stillCurrent()) return;
			console.error("Failed to load waveform:", err);
			set({ waveformLoading: false });
		}

		try {
			const annotationTicket = annotationAuthorityGate.issue();
			const rawAnnotations = await invoke<TrackScore[]>("list_track_scores", {
				scoreId,
			});
			if (!stillCurrent() || !annotationAuthorityGate.owns(annotationTicket))
				return;
			const annotations = rawAnnotations.map((ann) => {
				const pattern = patterns.find((p) => p.id === ann.patternId);
				return {
					...ann,
					patternName: pattern?.name,
					patternColor: getPatternColor(ann.patternId),
				};
			});
			set({
				annotations,
				annotationsLoading: false,
				scoreState: "loaded",
			});
		} catch (err) {
			if (!stillCurrent()) return;
			console.error("Failed to load annotations:", err);
			set({
				annotationsLoading: false,
				scoreState: "no_score",
				error: String(err),
			});
		}

		// Load audio for playback
		if (stillCurrent()) void get().loadTrackPlayback(trackId);
	},

	startFreshScore: () => {
		annotationAuthorityGate.supersede();
		set({ annotations: [], scoreState: "loaded" });
	},

	loadPatterns: async () => {
		set({ patternsLoading: true });
		try {
			const venueId = get().venueId;
			const patterns = await invoke<PatternSummary[]>("list_patterns");
			const argsEntries = await Promise.all(
				patterns.map(async (p) => {
					try {
						const args = await invoke<PatternArgDef[]>("get_pattern_args", {
							id: p.id,
							venueId,
							implementationId: null,
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

	loadTrackPlayback: async (trackId: string) => {
		try {
			await invoke("host_load_track", { trackId });
		} catch (err) {
			if (get().trackId !== trackId) return;
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
	setLoopRegion: async (start: number, end: number) => {
		set({ loopRegion: { start, end } });
		await invoke("host_set_loop_region", {
			startSeconds: start,
			endSeconds: end,
		});
	},
	clearLoopRegion: async () => {
		set({ loopRegion: null });
		await invoke("host_set_loop_region", {
			startSeconds: null,
			endSeconds: null,
		});
	},
	setSelectionCursor: (cursor: SelectionCursor | null) =>
		set({ selectionCursor: cursor }),
	setSelectedAnnotationIds: (ids: string[]) =>
		set({ selectedAnnotationIds: ids }),
	selectAnnotation: (annotationId: string | null) =>
		set({ selectedAnnotationIds: annotationId !== null ? [annotationId] : [] }),
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
		if (get().readOnly) return null;
		const authorityTicket = annotationAuthorityGate.issue();
		const ctx = requireContext(get);
		if (!ctx) return null;
		const { trackId, venueId } = ctx;
		const {
			scoreId,
			annotations,
			patternArgs,
			patterns,
			selectedAnnotationIds,
		} = get();
		if (!scoreId) return null;
		if (input.endTime - input.startTime < MIN_ANNOTATION_DURATION) {
			console.warn("Annotation too short, skipping", input);
			return null;
		}

		const dragBefore = useUndoStore.getState().getDragBefore();
		const base = dragBefore?.annotations ?? annotations;
		const argDefs = patternArgs[input.patternId] ?? [];
		const defaultArgs = Object.fromEntries(
			argDefs.map((arg) => [arg.id, arg.defaultValue ?? {}]),
		);
		const overlapActions = resolveOverlaps(
			annotations,
			input.startTime,
			input.endTime,
			new Set([input.zIndex]),
			new Set(),
		);
		const cleared = applyOverlapActions(
			annotations,
			overlapActions,
		).annotations;
		const draft = draftAnnotation(scoreId, input, input.args ?? defaultArgs);
		const candidate = [...cleared, draft];

		try {
			const applied = await replaceScoreCandidate(
				scoreId,
				trackId,
				base,
				candidate,
				patterns,
			);
			if (
				!ownsAnnotationAuthority(
					authorityTicket,
					get,
					trackId,
					venueId,
					scoreId,
				)
			) {
				return null;
			}
			acceptTrackProjection(applied.appliedToCurrentProjection);
			set({ annotations: applied.annotations, error: null });
			if (dragBefore) {
				useUndoStore
					.getState()
					.completeDrag(
						"Create annotation",
						applied.annotations,
						selectedAnnotationIds,
					);
			} else if (applied.appliedToCurrentProjection) {
				useUndoStore
					.getState()
					.push(
						"Create annotation",
						base,
						applied.annotations,
						selectedAnnotationIds,
						selectedAnnotationIds,
					);
			}
			const storedId = applied.idMap[draft.id] ?? draft.id;
			return (
				applied.annotations.find((annotation) => annotation.id === storedId) ??
				null
			);
		} catch (err) {
			console.error("Failed to create annotation:", err);
			if (
				ownsAnnotationAuthority(authorityTicket, get, trackId, venueId, scoreId)
			) {
				useUndoStore.getState().cancelDrag();
				set({
					annotations: enrichAnnotations(base, patterns),
					error: String(err),
				});
				void get()
					.reloadAnnotations()
					.catch(() => undefined);
			}
			return null;
		}
	},

	updateAnnotation: async (input) => {
		if (get().readOnly) return null;
		return withUndo("Edit annotation", get, async (authorityTicket) => {
			const { trackId, venueId, scoreId, annotations, patterns } = get();
			if (!trackId || !venueId || !scoreId) return null;
			try {
				const result = await updateTrackScore(scoreId, trackId, input);
				const applied = projectTrackEditResult(
					scoreId,
					annotations,
					result,
					patterns,
				);
				if (
					!ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					return null;
				}
				acceptTrackProjection(applied.appliedToCurrentProjection);
				set({ annotations: applied.annotations, error: null });
				return (
					applied.annotations.find(
						(annotation) => annotation.id === input.id,
					) ?? null
				);
			} catch (err) {
				console.error("Failed to update annotation:", err);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: String(err) });
				}
				return null;
			}
		});
	},

	updateAnnotationsBatch: async (inputs) => {
		if (get().readOnly) return;
		return withUndo("Edit annotations", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const { scoreId, annotations, patterns } = get();
			if (!scoreId) return;
			const inputMap = new Map(inputs.map((u) => [u.id, u]));
			const nextAnnotations = annotations.map((a) => {
				const input = inputMap.get(a.id);
				if (!input) return a;
				const next = {
					...a,
					startTime: input.startTime ?? a.startTime,
					endTime: input.endTime ?? a.endTime,
					zIndex: input.zIndex ?? a.zIndex,
					blendMode: input.blendMode == null ? a.blendMode : input.blendMode,
					args: input.args === undefined ? a.args : input.args,
				};
				const pattern = patterns.find((p) => p.id === next.patternId);
				return {
					...next,
					patternName: pattern?.name,
					patternColor: getPatternColor(next.patternId),
				};
			});
			try {
				const applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					nextAnnotations,
					patterns,
				);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					acceptTrackProjection(applied.appliedToCurrentProjection);
					set({ annotations: applied.annotations, error: null });
				}
			} catch (err) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to edit annotations: ${String(err)}` });
				}
			}
		});
	},

	updateArgs: (argId, value) => {
		if (get().readOnly) return;
		annotationAuthorityGate.supersede();
		const { annotations, selectedAnnotationIds } = get();
		const selected = annotations.filter((a) =>
			selectedAnnotationIds.includes(a.id),
		);
		if (selected.length === 0) return;

		// Mark the batch open on first change; the persist base is read at flush
		// time (after any in-flight flush), not captured here.
		if (!_argSnapshot) {
			_argSnapshot = { selection: [...selectedAnnotationIds] };
		}

		// Accumulate the live value off React (base = prior live edit, else the
		// committed args). NO store write here — that would re-render the timeline.
		for (const a of selected) {
			const base =
				_liveArgs.get(a.id) ?? (a.args as Record<string, unknown>) ?? {};
			_liveArgs.set(a.id, { ...base, [argId]: value });
		}

		// Push the live value straight to the rig (coalesced, no re-render).
		scheduleLiveComposite();

		// Commit to the store + persist on the trailing edge (single re-render).
		if (_argTimer) clearTimeout(_argTimer);
		_argTimer = setTimeout(() => void flushPendingArgs(), 250);
	},

	// Synchronous local-only update for smooth dragging
	updateAnnotationsLocal: (updates) => {
		if (get().readOnly) return;
		annotationAuthorityGate.supersede();
		const { annotations, selectedAnnotationIds } = get();
		useUndoStore
			.getState()
			.captureBeforeDrag([...annotations], [...selectedAnnotationIds]);
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
		// Live-composite while dragging an annotation (moving its span changes the
		// rig if it crosses the playhead, and the heatmap for audio-reactive
		// patterns). The composite effect is guarded out during the drag, so this
		// is what drives the live update. rAF-coalesced + incremental.
		scheduleLiveComposite();
	},

	// Persist annotations to backend (call on drag end)
	persistAnnotations: async (ids) => {
		const ctx = requireContext(get);
		if (!ctx) return;
		const { trackId, venueId } = ctx;
		const { scoreId, annotations, patterns, selectedAnnotationIds } = get();
		if (!scoreId) return;
		const authorityTicket = annotationAuthorityGate.issue();
		const dragBefore = useUndoStore.getState().getDragBefore();
		const base = dragBefore?.annotations ?? annotations;
		const idsSet = new Set(ids);
		const valid = annotations.filter(
			(annotation) =>
				idsSet.has(annotation.id) &&
				annotation.endTime - annotation.startTime >= MIN_ANNOTATION_DURATION,
		);
		let candidate = annotations.filter(
			(annotation) =>
				!idsSet.has(annotation.id) ||
				annotation.endTime - annotation.startTime >= MIN_ANNOTATION_DURATION,
		);
		for (const ann of valid) {
			const actions = resolveOverlaps(
				candidate,
				ann.startTime,
				ann.endTime,
				new Set([ann.zIndex]),
				idsSet,
			);
			candidate = applyOverlapActions(candidate, actions).annotations;
		}

		try {
			const applied = await replaceScoreCandidate(
				scoreId,
				trackId,
				base,
				candidate,
				patterns,
			);
			if (
				!ownsAnnotationAuthority(
					authorityTicket,
					get,
					trackId,
					venueId,
					scoreId,
				)
			) {
				return;
			}
			acceptTrackProjection(applied.appliedToCurrentProjection);
			const selected = selectedAnnotationIds
				.map((id) => applied.idMap[id] ?? id)
				.filter((id) =>
					applied.annotations.some((annotation) => annotation.id === id),
				);
			set({
				annotations: applied.annotations,
				selectedAnnotationIds: selected,
				error: null,
			});
			if (dragBefore) {
				useUndoStore
					.getState()
					.completeDrag("Move annotation", applied.annotations, selected);
			}
		} catch (error) {
			if (
				ownsAnnotationAuthority(authorityTicket, get, trackId, venueId, scoreId)
			) {
				useUndoStore.getState().cancelDrag();
				set({
					annotations: enrichAnnotations(base, patterns),
					error: `Failed to persist timeline edit: ${String(error)}`,
				});
				void get()
					.reloadAnnotations()
					.catch(() => undefined);
			}
		}
	},

	deleteAnnotation: async (annotationId: string) => {
		if (get().readOnly) return false;
		return withUndo("Delete annotation", get, async (authorityTicket) => {
			const {
				trackId,
				venueId,
				scoreId,
				annotations,
				selectedAnnotationIds,
				patterns,
			} = get();
			if (!trackId || !venueId || !scoreId) return false;
			try {
				const result = await deleteTrackScore(scoreId, trackId, annotationId);
				const applied = projectTrackEditResult(
					scoreId,
					annotations,
					result,
					patterns,
				);
				if (
					!ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					return false;
				}
				acceptTrackProjection(applied.appliedToCurrentProjection);
				set({
					annotations: applied.annotations,
					selectedAnnotationIds: selectedAnnotationIds.filter(
						(id) =>
							id !== annotationId &&
							applied.annotations.some((annotation) => annotation.id === id),
					),
					error: null,
				});
				return true;
			} catch (err) {
				console.error("Failed to delete annotation:", err);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: String(err) });
				}
				return false;
			}
		});
	},

	deleteAnnotations: async (annotationIds: string[]) => {
		if (get().readOnly) return;
		return withUndo("Delete annotations", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const { scoreId, annotations, patterns } = get();
			if (!scoreId) return;
			const idsSet = new Set(annotationIds);
			const candidate = annotations.filter(
				(annotation) => !idsSet.has(annotation.id),
			);
			try {
				const applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					candidate,
					patterns,
				);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					acceptTrackProjection(applied.appliedToCurrentProjection);
					set({
						annotations: applied.annotations,
						selectedAnnotationIds: [],
						selectionCursor: null,
						error: null,
					});
				}
			} catch (error) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to delete annotations: ${String(error)}` });
				}
			}
		});
	},

	reloadAnnotations: async (): Promise<boolean> => {
		const requestTicket = annotationAuthorityGate.issue();
		const { trackId, venueId, scoreId, patterns, annotations: prev } = get();
		if (!trackId || !venueId || !scoreId) return false;
		const rawAnnotations = await invoke<TrackScore[]>("list_track_scores", {
			scoreId,
		});
		const current = get();
		if (
			!annotationAuthorityGate.owns(requestTicket) ||
			current.trackId !== trackId ||
			current.venueId !== venueId ||
			current.scoreId !== scoreId
		) {
			return false;
		}
		const annotations = rawAnnotations.map((ann) => {
			const pattern = patterns.find((p) => p.id === ann.patternId);
			return {
				...ann,
				patternName: pattern?.name,
				patternColor: getPatternColor(ann.patternId),
			};
		});

		const prevById = new Map(prev.map((a) => [a.id, a]));
		const changed =
			annotations.length !== prev.length ||
			annotations.some((a) => {
				const p = prevById.get(a.id);
				if (!p) return true;
				return (
					a.startTime !== p.startTime ||
					a.endTime !== p.endTime ||
					a.zIndex !== p.zIndex ||
					a.blendMode !== p.blendMode ||
					!argsEqual(a.args, p.args)
				);
			});

		set({ annotations });
		return changed;
	},

	splitAtCursor: async () => {
		if (get().readOnly) return;
		return withUndo("Split", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const { scoreId, selectionCursor, annotations, patterns } = get();
			if (!scoreId) return;
			if (!selectionCursor) return;

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
				// Row 0 is the empty top lane; occupied rows start at 1
				const zIdx = r - 1;
				if (zIdx >= 0 && zIdx < zRowsDesc.length)
					affectedZIndexes.add(zRowsDesc[zIdx]);
			}

			// Find annotations that straddle the split point
			const toSplit = annotations.filter(
				(ann) =>
					affectedZIndexes.has(ann.zIndex) &&
					ann.startTime < splitTime &&
					ann.endTime > splitTime,
			);

			if (toSplit.length === 0) return;

			let candidate = [...annotations];
			const draftIds: string[] = [];
			for (const ann of toSplit) {
				const leftDuration = splitTime - ann.startTime;
				const rightDuration = ann.endTime - splitTime;

				// Skip if either half would be too short
				if (
					leftDuration < MIN_ANNOTATION_DURATION ||
					rightDuration < MIN_ANNOTATION_DURATION
				)
					continue;

				const draftId = crypto.randomUUID();
				const now = new Date().toISOString();
				candidate = candidate.map((annotation) =>
					annotation.id === ann.id
						? { ...annotation, endTime: splitTime }
						: annotation,
				);
				candidate.push({
					...ann,
					id: draftId,
					uid: null,
					startTime: splitTime,
					createdAt: now,
					updatedAt: now,
				});
				draftIds.push(draftId);
			}
			if (draftIds.length === 0) return;
			try {
				const applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					candidate,
					patterns,
				);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					acceptTrackProjection(applied.appliedToCurrentProjection);
					set({
						annotations: applied.annotations,
						selectedAnnotationIds: draftIds.map(
							(id) => applied.idMap[id] ?? id,
						),
						error: null,
					});
				}
			} catch (error) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to split annotations: ${String(error)}` });
				}
			}
		});
	},

	deleteInRegion: async () => {
		if (get().readOnly) return;
		return withUndo("Delete region", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const { scoreId, selectionCursor, annotations, patterns } = get();
			if (!scoreId) return;
			if (!selectionCursor || selectionCursor.endTime === null) return;

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
				// Row 0 is the empty top lane; occupied rows start at 1
				const zIdx = r - 1;
				if (zIdx >= 0 && zIdx < zRowsDesc.length)
					affectedZIndexes.add(zRowsDesc[zIdx]);
			}

			const actions = resolveOverlaps(
				annotations,
				rangeStart,
				rangeEnd,
				affectedZIndexes,
				new Set(),
			);

			if (actions.length === 0) return;
			const candidate = applyOverlapActions(annotations, actions).annotations;
			try {
				const applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					candidate,
					patterns,
				);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					acceptTrackProjection(applied.appliedToCurrentProjection);
					set({
						annotations: applied.annotations,
						selectedAnnotationIds: [],
						selectionCursor: null,
						error: null,
					});
				}
			} catch (error) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to delete region: ${String(error)}` });
				}
			}
		});
	},

	moveAnnotationsVertical: async (direction) => {
		if (get().readOnly) return;
		return withUndo("Move to lane", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const {
				scoreId,
				annotations,
				selectedAnnotationIds,
				selectionCursor,
				patterns,
			} = get();
			if (!scoreId) return;
			if (selectedAnnotationIds.length === 0) return;

			const selected = annotations.filter((a) =>
				selectedAnnotationIds.includes(a.id),
			);
			if (selected.length === 0) return;

			// Build row mapping: row 0 = highest z (visually top)
			const sortedZ = Array.from(
				new Set(annotations.map((a) => a.zIndex)),
			).sort((a, b) => a - b);
			const zRowsDesc = [...sortedZ].sort((a, b) => b - a);

			const targetById = new Map<string, number>();
			if (direction === "up") {
				const highestZ = zRowsDesc[0];
				for (const ann of selected) {
					const row = zRowsDesc.indexOf(ann.zIndex);
					const targetZ = row <= 0 ? highestZ + 1 : zRowsDesc[row - 1];
					targetById.set(ann.id, targetZ);
				}
			} else {
				const selectedRows = selected.map((ann) =>
					zRowsDesc.indexOf(ann.zIndex),
				);
				// z=0 is the floor. Moving a multi-lane selection down is all-or-
				// nothing so its relative spacing cannot collapse at the boundary.
				if (Math.max(...selectedRows) >= zRowsDesc.length - 1) return;
				for (const ann of selected) {
					const row = zRowsDesc.indexOf(ann.zIndex);
					const targetZ = zRowsDesc[row + 1];
					targetById.set(ann.id, targetZ);
				}
			}
			const selectedIds = new Set(selectedAnnotationIds);
			let candidate = annotations.map((annotation) => {
				const zIndex = targetById.get(annotation.id);
				return zIndex === undefined ? annotation : { ...annotation, zIndex };
			});
			const movedAnns = candidate.filter((annotation) =>
				selectedIds.has(annotation.id),
			);
			for (const ann of movedAnns) {
				const actions = resolveOverlaps(
					candidate,
					ann.startTime,
					ann.endTime,
					new Set([ann.zIndex]),
					selectedIds,
				);
				candidate = applyOverlapActions(candidate, actions).annotations;
			}
			let applied: Awaited<ReturnType<typeof replaceScoreCandidate>>;
			try {
				applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					candidate,
					patterns,
				);
			} catch (error) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to move annotations: ${String(error)}` });
				}
				return;
			}
			if (
				!ownsAnnotationAuthority(
					authorityTicket,
					get,
					trackId,
					venueId,
					scoreId,
				)
			) {
				return;
			}
			acceptTrackProjection(applied.appliedToCurrentProjection);
			set({ annotations: applied.annotations, error: null });

			// Update selection cursor to the actual row of moved annotations
			if (selectionCursor) {
				const finalAnnotations = applied.annotations;
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
					const actualRow = zIdx >= 0 ? maxRow - zIdx + 1 : maxRow + 1;

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
		if (!selectionCursor) return;

		const region = getRegionInfo(
			selectionCursor,
			annotations,
			selectedAnnotationIds,
		);

		// Region mode: clip any annotation overlapping the cursor's range × row
		// band (mirrors deleteInRegion / resolveOverlaps semantics).
		if (region) {
			const { regionStart, regionEnd, affectedZIndexes } = region;
			const items: ClipboardItem[] = [];
			for (const a of annotations) {
				if (!affectedZIndexes.has(a.zIndex)) continue;
				if (a.endTime <= regionStart || a.startTime >= regionEnd) continue;
				const clippedStart = Math.max(a.startTime, regionStart);
				const clippedEnd = Math.min(a.endTime, regionEnd);
				const duration = clippedEnd - clippedStart;
				if (duration < MIN_ANNOTATION_DURATION) continue;
				items.push({
					patternId: a.patternId,
					offsetFromStart: clippedStart - regionStart,
					duration,
					zIndex: a.zIndex,
					blendMode: a.blendMode,
					args: (a.args as Record<string, unknown> | undefined) ?? {},
				});
			}
			if (items.length === 0) return;
			set({
				clipboard: { items, totalDuration: regionEnd - regionStart },
			});
			return;
		}

		// Object mode: copy explicitly selected annotations in full (e.g.
		// shift-clicks across different rows / times).
		if (selectedAnnotationIds.length === 0) return;
		const selectedAnns = annotations.filter((a) =>
			selectedAnnotationIds.includes(a.id),
		);
		if (selectedAnns.length === 0) return;

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
		if (get().readOnly) return;
		return withUndo("Cut", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const {
				scoreId,
				selectionCursor,
				annotations,
				selectedAnnotationIds,
				patterns,
			} = get();
			if (!scoreId) return;

			// Snapshot region info before we mutate clipboard / selection state so
			// the delete path matches what copySelection actually captured.
			const region = getRegionInfo(
				selectionCursor,
				annotations,
				selectedAnnotationIds,
			);

			get().copySelection();
			if (!get().clipboard) return;

			let candidate: TimelineAnnotation[];
			if (region) {
				const actions = resolveOverlaps(
					annotations,
					region.regionStart,
					region.regionEnd,
					region.affectedZIndexes,
					new Set(),
				);
				candidate = applyOverlapActions(annotations, actions).annotations;
			} else {
				if (selectedAnnotationIds.length === 0) return;
				const idsSet = new Set(selectedAnnotationIds);
				candidate = annotations.filter(
					(annotation) => !idsSet.has(annotation.id),
				);
			}
			try {
				const applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					candidate,
					patterns,
				);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					acceptTrackProjection(applied.appliedToCurrentProjection);
					set({
						annotations: applied.annotations,
						selectedAnnotationIds: [],
						selectionCursor: null,
						error: null,
					});
				}
			} catch (error) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to cut selection: ${String(error)}` });
				}
			}
		});
	},

	paste: async () => {
		if (get().readOnly) return;
		return withUndo("Paste", get, async (authorityTicket) => {
			const ctx = requireContext(get);
			if (!ctx) return;
			const { trackId, venueId } = ctx;
			const {
				scoreId,
				clipboard,
				selectionCursor,
				patterns,
				durationSeconds,
				annotations,
			} = get();
			if (!scoreId) return;
			if (!clipboard || !selectionCursor) return;

			// Top-left anchor: the topmost clipboard row aligns to the cursor's trackRow.
			// Build the z-index ↔ row mapping from current annotations + clipboard z-values
			const allZIndexes = new Set([
				...annotations.map((a) => a.zIndex),
				...clipboard.items.map((item) => item.zIndex),
			]);
			const zRowsDesc = Array.from(allZIndexes).sort((a, b) => b - a);
			const rowToZ = (row: number): number => {
				if (zRowsDesc.length === 0) return 0;
				if (row < zRowsDesc.length) return zRowsDesc[row];
				const lowest = zRowsDesc[zRowsDesc.length - 1];
				return lowest - (row - (zRowsDesc.length - 1));
			};

			// The topmost (highest z) item in the clipboard is the anchor
			const sourceTopZ = Math.max(
				...clipboard.items.map((item) => item.zIndex),
			);
			const sourceTopRow = zRowsDesc.indexOf(sourceTopZ);

			// trackRow is 1-based (row 0 = empty top lane), convert to 0-based z-index
			const targetRow = Math.max(0, selectionCursor.trackRow - 1);
			const rowShift = targetRow - sourceTopRow;

			// Compute per-item z-index: convert each item's row, shift it, map back to z
			const itemZIndex = (itemZ: number): number => {
				const srcRow = zRowsDesc.indexOf(itemZ);
				const srcRowNorm = srcRow >= 0 ? srcRow : zRowsDesc.length; // fallback for unknown z
				const destRow = srcRowNorm + rowShift;
				return rowToZ(Math.max(0, destRow));
			};

			// Normalize paste position (handle right-to-left selection)
			const pasteStart =
				selectionCursor.endTime !== null
					? Math.min(selectionCursor.startTime, selectionCursor.endTime)
					: selectionCursor.startTime;
			const pasteEnd = pasteStart + clipboard.totalDuration;

			// Get all unique destination zIndexes the paste will occupy
			const clipboardZIndexes = new Set(
				clipboard.items.map((item) => itemZIndex(item.zIndex)),
			);

			// Clear the paste region using shared overlap resolution
			const overlapActions = resolveOverlaps(
				annotations,
				pasteStart,
				pasteEnd,
				clipboardZIndexes,
				new Set(),
			);

			const cleared = applyOverlapActions(
				annotations,
				overlapActions,
			).annotations;
			const itemsToPaste = clipboard.items.filter((item) => {
				const endTime = pasteStart + item.offsetFromStart + item.duration;
				return endTime <= durationSeconds;
			});

			const drafts = itemsToPaste.map((item) =>
				draftAnnotation(
					scoreId,
					{
						patternId: item.patternId,
						startTime: pasteStart + item.offsetFromStart,
						endTime: pasteStart + item.offsetFromStart + item.duration,
						zIndex: itemZIndex(item.zIndex),
						blendMode: item.blendMode,
					},
					item.args ?? {},
				),
			);
			if (drafts.length === 0 && overlapActions.length === 0) return;
			try {
				const applied = await replaceScoreCandidate(
					scoreId,
					trackId,
					annotations,
					[...cleared, ...drafts],
					patterns,
				);
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					acceptTrackProjection(applied.appliedToCurrentProjection);
					set({
						annotations: applied.annotations,
						selectionCursor: {
							trackRow: selectionCursor.trackRow,
							trackRowEnd: selectionCursor.trackRowEnd ?? null,
							startTime: pasteStart,
							endTime: pasteEnd,
						},
						selectedAnnotationIds: drafts.map(
							(draft) => applied.idMap[draft.id] ?? draft.id,
						),
						error: null,
					});
				}
			} catch (error) {
				if (
					ownsAnnotationAuthority(
						authorityTicket,
						get,
						trackId,
						venueId,
						scoreId,
					)
				) {
					set({ error: `Failed to paste: ${String(error)}` });
				}
			}
		});
	},

	duplicate: async () => {
		const { selectionCursor } = get();
		if (!selectionCursor) return;

		// First copy the current selection
		get().copySelection();

		const { clipboard, selectedAnnotationIds, annotations } = get();
		if (!clipboard) return;

		// Calculate paste position at end of current cursor
		const cursorEnd =
			selectionCursor.endTime !== null
				? Math.max(selectionCursor.startTime, selectionCursor.endTime)
				: selectionCursor.startTime;

		// Derive trackRow from the actual z-indexes of selected annotations.
		// selectionCursor.trackRow reflects where the cursor drag *started*, which
		// may be a lower row than the topmost selected annotation (e.g. user dragged
		// upward). Using the stale value shifts the paste down by the difference.
		const selectedAnns = annotations.filter((a) =>
			selectedAnnotationIds.includes(a.id),
		);
		const allZ = Array.from(new Set(annotations.map((a) => a.zIndex))).sort(
			(a, b) => a - b,
		);
		const zMax = Math.max(0, allZ.length - 1);
		const selectedZ = new Set(selectedAnns.map((a) => a.zIndex));
		const rows = [...selectedZ].map((z) => {
			const idx = allZ.indexOf(z);
			return idx >= 0 ? zMax - idx + 1 : zMax + 1;
		});
		const trackRow =
			rows.length > 0 ? Math.min(...rows) : selectionCursor.trackRow;

		// Temporarily update cursor to paste position
		set({
			selectionCursor: {
				...selectionCursor,
				startTime: cursorEnd,
				endTime: null,
				trackRow,
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

	cloneAnnotationsInPlace: async (ids) => {
		annotationAuthorityGate.supersede();
		const { scoreId, annotations, patterns } = get();
		if (!scoreId) return [];
		const now = new Date().toISOString();
		const selected = new Set(ids);
		const newAnnotations = enrichAnnotations(
			annotations
				.filter((annotation) => selected.has(annotation.id))
				.map((annotation) => ({
					...annotation,
					id: crypto.randomUUID(),
					uid: null,
					createdAt: now,
					updatedAt: now,
				})),
			patterns,
		);

		if (newAnnotations.length > 0) {
			set({ annotations: [...get().annotations, ...newAnnotations] });
		}
		return newAnnotations;
	},

	setError: (error: string | null) => set({ error }),

	resetTrack: () => {
		trackLoadGeneration += 1;
		annotationAuthorityGate.supersede();
		const { trackId, scoreId } = get();
		void flushPendingArgs();
		useUndoStore.getState().clear();
		useAnnotationPreviewStore.getState().clear();

		// Tell the backend to cancel compositing, unload audio, and free caches
		if (trackId !== null && scoreId !== null) {
			invoke("leave_track", { scoreId }).catch((err) =>
				console.error("leave_track failed:", err),
			);
		}

		set({
			trackId: null,
			venueId: null,
			scoreId: null,
			readOnly: false,
			trackName: "",
			scoreState: "loading",
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
			isDraggingAnnotation: false,
			error: null,
		});
	},
}));
