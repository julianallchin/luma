import { create } from "zustand";
import type {
	BeatGrid,
	PatternArgDef,
	PatternSummary,
	ScoreSummary,
	TrackScore,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";
import type {
	TimelineAnnotation,
	TrackWaveform,
} from "../stores/use-track-editor-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import type { BarClassificationsPayload, DrumOnsets } from "./build-context";
import { getPatternColor } from "./score-mutations";

/**
 * Per-track snapshot of everything the track agent / its tools need. Owned by
 * this store rather than the editor store, so the agent keeps working after the
 * user navigates back to the track list.
 *
 * The conversation itself lives in the durable thread (see `track-agent.ts`);
 * this is only the world the tools read and mutate.
 */
export type SessionContext = {
	venueId: string;
	scoreId: string;
	readOnly: boolean;
	trackName: string;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	annotations: TimelineAnnotation[];
	patterns: PatternSummary[];
	patternArgs: Record<string, PatternArgDef[]>;
	venueName: string | null;
	barClassifications: BarClassificationsPayload | null;
	drumOnsets: DrumOnsets | null;
	tagThresholds: Record<string, number>;
};

export type BootstrapArgs = {
	trackId: string;
	venueId: string;
	venueName: string | null;
	userId: string;
	trackName: string;
	/** Existing scoreId. If omitted, the most recent own score is used; if
	 * none exists, a fresh score is created. */
	scoreId?: string;
	/** Pass true to allow loading a score owned by someone else as read-only.
	 * Defaults to false (auto-light needs to write). */
	allowReadOnly?: boolean;
};

export type BootstrapResult =
	| { ok: true; context: SessionContext; createdScore: boolean }
	| { ok: false; error: string };

type SessionsState = {
	contexts: Record<string, SessionContext>;
	getContext: (trackId: string) => SessionContext | null;
	updateContext: (trackId: string, partial: Partial<SessionContext>) => void;
	/** Load everything the agent needs for a track from scratch. Used by the
	 * background auto-light driver, where no editor is mounted. */
	bootstrap: (args: BootstrapArgs) => Promise<BootstrapResult>;
	/** Replace a track's annotations (called from inside tool handlers). */
	setAnnotations: (trackId: string, annotations: TimelineAnnotation[]) => void;
};

function defaultContext(): SessionContext {
	return {
		venueId: "",
		scoreId: "",
		readOnly: false,
		trackName: "",
		durationSeconds: 0,
		beatGrid: null,
		annotations: [],
		patterns: [],
		patternArgs: {},
		venueName: null,
		barClassifications: null,
		drumOnsets: null,
		tagThresholds: {},
	};
}

export const useTrackSessionStore = create<SessionsState>((set, get) => ({
	contexts: {},

	getContext: (trackId) => get().contexts[trackId] ?? null,

	updateContext: (trackId, partial) => {
		set((state) => ({
			contexts: {
				...state.contexts,
				[trackId]: {
					...(state.contexts[trackId] ?? defaultContext()),
					...partial,
				},
			},
		}));
	},

	setAnnotations: (trackId, annotations) => {
		const existing = get().contexts[trackId];
		if (!existing) return;
		set((state) => ({
			contexts: {
				...state.contexts,
				[trackId]: { ...existing, annotations },
			},
		}));
	},

	bootstrap: async (args) => {
		const { trackId, venueId, venueName, userId, trackName, scoreId } = args;
		try {
			// Resolve the score to work against.
			let resolvedScoreId = scoreId ?? null;
			let readOnly = false;
			let createdScore = false;

			if (!resolvedScoreId) {
				const scores = await invoke<ScoreSummary[]>("list_scores_for_track", {
					trackId,
					venueId,
				});
				const ownScore = scores.find((s) => s.uid === userId);
				if (ownScore) {
					resolvedScoreId = ownScore.id;
				} else {
					const created = await invoke<{ id: string }>("create_score", {
						trackId,
						venueId,
						uid: userId,
						name: null,
					});
					resolvedScoreId = created.id;
					createdScore = true;
				}
			} else {
				// Verify the existing score's ownership for the read-only flag.
				const scores = await invoke<ScoreSummary[]>("list_scores_for_track", {
					trackId,
					venueId,
				});
				const found = scores.find((s) => s.id === resolvedScoreId);
				if (found && found.uid !== userId) {
					if (!args.allowReadOnly) {
						throw new Error("Score is owned by another user (read-only).");
					}
					readOnly = true;
				}
			}

			// Make sure patterns are loaded in the editor store (cached if so).
			// We reuse them directly to avoid duplicating expensive arg fetches.
			if (useTrackEditorStore.getState().patterns.length === 0) {
				await useTrackEditorStore.getState().loadPatterns();
			}
			const editorState = useTrackEditorStore.getState();
			const patterns = editorState.patterns;
			const patternArgs = editorState.patternArgs;

			// Fetch the rest in parallel.
			const [
				beatGrid,
				waveform,
				rawAnnotations,
				barClassifications,
				drumOnsets,
				tagThresholds,
			] = await Promise.all([
				invoke<BeatGrid | null>("get_track_beats", { trackId }).catch(
					() => null,
				),
				invoke<TrackWaveform>("get_track_waveform", { trackId }).catch(
					() => null,
				),
				invoke<TrackScore[]>("list_track_scores", {
					scoreId: resolvedScoreId,
				}).catch(() => [] as TrackScore[]),
				invoke<{
					classifications: BarClassificationsPayload["classifications"];
					tagOrder: string[];
				} | null>("get_track_bar_classifications", { trackId }).catch(
					() => null,
				),
				invoke<DrumOnsets | null>("get_track_drum_onsets", { trackId }).catch(
					() => null,
				),
				invoke<Record<string, number>>("get_classifier_thresholds").catch(
					() => ({}) as Record<string, number>,
				),
			]);

			const annotations: TimelineAnnotation[] = rawAnnotations.map((ann) => {
				const pattern = patterns.find((p) => p.id === ann.patternId);
				return {
					...ann,
					patternName: pattern?.name,
					patternColor: getPatternColor(ann.patternId),
				};
			});

			if (!resolvedScoreId) {
				throw new Error("Failed to resolve a score id.");
			}
			const context: SessionContext = {
				venueId,
				scoreId: resolvedScoreId,
				readOnly,
				trackName,
				durationSeconds: waveform?.durationSeconds ?? 0,
				beatGrid,
				annotations,
				patterns,
				patternArgs,
				venueName,
				barClassifications: barClassifications
					? {
							classifications: barClassifications.classifications,
							tagOrder: barClassifications.tagOrder,
						}
					: null,
				drumOnsets,
				tagThresholds,
			};

			set((state) => ({
				contexts: { ...state.contexts, [trackId]: context },
			}));
			return { ok: true as const, context, createdScore };
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			return { ok: false as const, error: message };
		}
	},
}));

// Mirror the editor store's view into the matching session whenever its
// inputs change. This keeps the agent's context fresh when the user makes
// manual edits in the timeline (drag, restack, paste, etc.) without each
// editor action having to know about sessions.
useTrackEditorStore.subscribe((state, prev) => {
	const trackId = state.trackId;
	if (!trackId) return;
	const ctx = useTrackSessionStore.getState().contexts[trackId];
	if (!ctx) return;

	const next: Partial<SessionContext> = {};
	let changed = false;

	if (
		state.annotations !== prev.annotations &&
		state.annotations !== ctx.annotations
	) {
		next.annotations = state.annotations;
		changed = true;
	}
	if (state.beatGrid !== prev.beatGrid && state.beatGrid !== ctx.beatGrid) {
		next.beatGrid = state.beatGrid;
		changed = true;
	}
	if (
		state.durationSeconds !== prev.durationSeconds &&
		state.durationSeconds !== ctx.durationSeconds
	) {
		next.durationSeconds = state.durationSeconds;
		changed = true;
	}
	if (state.patterns !== prev.patterns && state.patterns !== ctx.patterns) {
		next.patterns = state.patterns;
		changed = true;
	}
	if (
		state.patternArgs !== prev.patternArgs &&
		state.patternArgs !== ctx.patternArgs
	) {
		next.patternArgs = state.patternArgs;
		changed = true;
	}
	if (
		state.scoreId &&
		state.scoreId !== prev.scoreId &&
		state.scoreId !== ctx.scoreId
	) {
		next.scoreId = state.scoreId;
		changed = true;
	}
	if (state.readOnly !== prev.readOnly && state.readOnly !== ctx.readOnly) {
		next.readOnly = state.readOnly;
		changed = true;
	}
	if (
		state.trackName &&
		state.trackName !== prev.trackName &&
		state.trackName !== ctx.trackName
	) {
		next.trackName = state.trackName;
		changed = true;
	}
	if (changed) {
		useTrackSessionStore.getState().updateContext(trackId, next);
	}
});
