import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	BeatGrid,
	PatternSummary,
	PlaybackStateSnapshot,
} from "@/bindings/schema";
import { MAX_ZOOM, MIN_ZOOM } from "../utils/timeline-constants";

// Local types until we regenerate bindings
export type TrackAnnotation = {
	id: number;
	trackId: number;
	patternId: number;
	startTime: number;
	endTime: number;
	zIndex: number;
	createdAt: string;
	updatedAt: string;
};

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
	createdAt?: string;
	updatedAt?: string;
};

export type UpdateAnnotationInput = {
	id: number;
	startTime?: number;
	endTime?: number;
	zIndex?: number;
};

export type TimelineAnnotation = TrackAnnotation & {
	patternName?: string;
	patternColor?: string;
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
	zoom: number;
	scrollX: number;
	playheadPosition: number;
	isPlaying: boolean;
	selectedAnnotationId: number | null;
	draggingPatternId: number | null;
	error: string | null;

	loadTrack: (trackId: number, trackName: string) => Promise<void>;
	loadPatterns: () => Promise<void>;
	loadTrackPlayback: (trackId: number) => Promise<void>;
	play: () => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
	syncPlaybackState: (snapshot: PlaybackStateSnapshot) => void;
	setZoom: (zoom: number) => void;
	setScrollX: (scrollX: number) => void;
	setPlayheadPosition: (position: number) => void;
	setIsPlaying: (isPlaying: boolean) => void;
	selectAnnotation: (annotationId: number | null) => void;
	setDraggingPatternId: (patternId: number | null) => void;
	createAnnotation: (
		input: Omit<CreateAnnotationInput, "trackId">,
	) => Promise<TrackAnnotation | null>;
	updateAnnotation: (
		input: UpdateAnnotationInput,
	) => Promise<TrackAnnotation | null>;
	deleteAnnotation: (annotationId: number) => Promise<boolean>;
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
	zoom: 50,
	scrollX: 0,
	playheadPosition: 0,
	isPlaying: false,
	selectedAnnotationId: null,
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
			set({ patterns, patternsLoading: false });
		} catch (err) {
			console.error("Failed to load patterns:", err);
			set({ patternsLoading: false, error: String(err) });
		}
	},

	loadTrackPlayback: async (trackId: number) => {
		try {
			await invoke("load_track_playback", { trackId });
		} catch (err) {
			console.error("Failed to load track playback:", err);
			set({ error: `Failed to load audio playback: ${String(err)}` });
		}
	},

	play: async () => {
		const { trackId, playheadPosition } = get();
		if (trackId !== null) {
			// Play from current position
			await invoke("playback_play_node", {
				nodeId: `track:${trackId}`,
				startTime: playheadPosition,
			});
		}
	},

	pause: async () => {
		await invoke("playback_pause");
	},

	seek: async (seconds: number) => {
		await invoke("playback_seek", { seconds });
	},

	syncPlaybackState: (snapshot: PlaybackStateSnapshot) => {
		const { trackId } = get();
		if (trackId !== null && snapshot.activeNodeId === `track:${trackId}`) {
			set({
				isPlaying: snapshot.isPlaying,
				playheadPosition: snapshot.currentTime,
			});
		} else if (
			snapshot.isPlaying &&
			snapshot.activeNodeId !== `track:${trackId}`
		) {
			set({ isPlaying: false });
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
	selectAnnotation: (annotationId: number | null) =>
		set({ selectedAnnotationId: annotationId }),
	setDraggingPatternId: (patternId: number | null) =>
		set({ draggingPatternId: patternId }),

	createAnnotation: async (input) => {
		const { trackId, patterns, annotations } = get();
		if (trackId === null) return null;

		try {
			const annotation = await invoke<TrackAnnotation>("create_annotation", {
				input: { ...input, trackId },
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

	deleteAnnotation: async (annotationId: number) => {
		const { annotations, selectedAnnotationId } = get();
		try {
			await invoke<void>("delete_annotation", { annotationId });
			set({
				annotations: annotations.filter((a) => a.id !== annotationId),
				selectedAnnotationId:
					selectedAnnotationId === annotationId ? null : selectedAnnotationId,
			});
			return true;
		} catch (err) {
			console.error("Failed to delete annotation:", err);
			set({ error: String(err) });
			return false;
		}
	},

	setError: (error: string | null) => set({ error }),
}));
