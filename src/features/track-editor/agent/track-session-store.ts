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
import { getPatternColor } from "../utils/timeline-constants";

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
};

/** Identity of one editable track world. A track can be open against several
 * venue/score pairs at once (interactive editor + background jobs), so trackId
 * alone is never a safe cache key. */
export type TrackSessionScope = Readonly<{
	trackId: string;
	venueId: string;
	scoreId: string;
}>;

export function trackSessionKey(scope: TrackSessionScope): string {
	return JSON.stringify([scope.trackId, scope.venueId, scope.scoreId]);
}

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
	getContext: (scope: TrackSessionScope) => SessionContext | null;
	updateContext: (
		scope: TrackSessionScope,
		partial: Partial<Omit<SessionContext, "venueId" | "scoreId">>,
	) => void;
	/** Load everything the agent needs for a track from scratch. Used by the
	 * background auto-light driver, where no editor is mounted. */
	bootstrap: (args: BootstrapArgs) => Promise<BootstrapResult>;
	/** Replace a track's annotations (called from inside tool handlers). */
	setAnnotations: (
		scope: TrackSessionScope,
		annotations: TimelineAnnotation[],
	) => void;
};

function defaultContext(scope: TrackSessionScope): SessionContext {
	return {
		venueId: scope.venueId,
		scoreId: scope.scoreId,
		readOnly: false,
		trackName: "",
		durationSeconds: 0,
		beatGrid: null,
		annotations: [],
		patterns: [],
		patternArgs: {},
		venueName: null,
	};
}

/** Add editor-only presentation fields to the authoritative persisted rows. */
export function toTimelineAnnotations(
	scores: TrackScore[],
	patterns: PatternSummary[],
): TimelineAnnotation[] {
	return scores.map((score) => ({
		...score,
		patternName: patterns.find((pattern) => pattern.id === score.patternId)
			?.name,
		patternColor: getPatternColor(score.patternId),
	}));
}

export const useTrackSessionStore = create<SessionsState>((set, get) => ({
	contexts: {},

	getContext: (scope) => get().contexts[trackSessionKey(scope)] ?? null,

	updateContext: (scope, partial) => {
		const key = trackSessionKey(scope);
		set((state) => ({
			contexts: {
				...state.contexts,
				[key]: {
					...(state.contexts[key] ?? defaultContext(scope)),
					...partial,
					venueId: scope.venueId,
					scoreId: scope.scoreId,
				},
			},
		}));
	},

	setAnnotations: (scope, annotations) => {
		const key = trackSessionKey(scope);
		const existing = get().contexts[key];
		if (!existing) return;
		set((state) => ({
			contexts: {
				...state.contexts,
				[key]: { ...existing, annotations },
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
				if (!found) {
					throw new Error(
						"Score does not belong to the requested track and venue.",
					);
				}
				if (found.uid !== userId) {
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
			const [beatGrid, waveform, rawAnnotations] = await Promise.all([
				invoke<BeatGrid | null>("get_track_beats", { trackId }).catch(
					() => null,
				),
				invoke<TrackWaveform>("get_track_waveform", { trackId }).catch(
					() => null,
				),
				invoke<TrackScore[]>("list_track_scores", {
					scoreId: resolvedScoreId,
				}).catch(() => [] as TrackScore[]),
			]);

			const annotations = toTimelineAnnotations(rawAnnotations, patterns);

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
			};

			set((state) => ({
				contexts: {
					...state.contexts,
					[trackSessionKey({
						trackId,
						venueId,
						scoreId: resolvedScoreId,
					})]: context,
				},
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
	const venueId = state.venueId;
	const scoreId = state.scoreId;
	if (!trackId || !venueId || !scoreId) return;
	const scope = { trackId, venueId, scoreId };
	const ctx = useTrackSessionStore.getState().getContext(scope);
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
		useTrackSessionStore.getState().updateContext(scope, next);
	}
});
