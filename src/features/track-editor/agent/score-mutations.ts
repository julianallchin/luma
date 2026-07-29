import type {
	BlendMode,
	PatternArgDef,
	PatternSummary,
	TrackScore,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import {
	applyOverlapActions,
	resolveOverlaps,
} from "../utils/overlap-resolution";
import { MIN_ANNOTATION_DURATION } from "../utils/timeline-constants";

const PATTERN_COLORS = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

export function getPatternColor(patternId: string): string {
	let hash = 0;
	for (let i = 0; i < patternId.length; i++) {
		hash = (hash * 31 + patternId.charCodeAt(i)) | 0;
	}
	return PATTERN_COLORS[Math.abs(hash) % PATTERN_COLORS.length];
}

export function enrichAnnotation(
	ann: TrackScore,
	patterns: PatternSummary[],
): TimelineAnnotation {
	const pattern = patterns.find((p) => p.id === ann.patternId);
	return {
		...ann,
		patternName: pattern?.name,
		patternColor: getPatternColor(ann.patternId),
	};
}

/** Common state every mutation needs, regardless of caller. The editor store
 * derives these from its own state; the agent's session derives them from
 * the bootstrapped session context. */
export type MutationContext = {
	scoreId: string;
	trackId: string;
	annotations: TimelineAnnotation[];
	patterns: PatternSummary[];
	patternArgs: Record<string, PatternArgDef[]>;
};

export type CreateInput = {
	patternId: string;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode?: BlendMode | null;
	args?: Record<string, unknown>;
};

export type UpdateInput = {
	id: string;
	startTime?: number;
	endTime?: number;
	zIndex?: number;
	blendMode?: BlendMode | null;
	args?: Record<string, unknown>;
};

export type MutationResult<T> = {
	value: T;
	annotations: TimelineAnnotation[];
};

function defaultArgs(
	patternId: string,
	patternArgs: Record<string, PatternArgDef[]>,
): Record<string, unknown> {
	const argDefs = patternArgs[patternId] ?? [];
	return Object.fromEntries(
		argDefs.map((arg) => [arg.id, arg.defaultValue ?? {}]),
	);
}

async function reloadAnnotationsFromServer(
	scoreId: string,
	patterns: PatternSummary[],
): Promise<TimelineAnnotation[]> {
	const raw = await invoke<TrackScore[]>("list_track_scores", { scoreId });
	return raw.map((a) => enrichAnnotation(a, patterns));
}

/**
 * Place a clip on the timeline. Resolves any overlapping clips on the same
 * z-index, calls create_track_score, and returns the new annotation plus the
 * refreshed annotation list. Used by both the editor store's
 * `createAnnotation` action and the agent's `place_clip` tool — every code
 * path that places a clip flows through here.
 */
export async function createAnnotationMutation(
	ctx: MutationContext,
	input: CreateInput,
): Promise<MutationResult<TrackScore | null>> {
	if (input.endTime - input.startTime < MIN_ANNOTATION_DURATION) {
		return { value: null, annotations: ctx.annotations };
	}

	const overlapActions = resolveOverlaps(
		ctx.annotations,
		input.startTime,
		input.endTime,
		new Set([input.zIndex]),
		new Set(),
	);
	if (overlapActions.length > 0) {
		await applyOverlapActions(overlapActions, ctx.scoreId, ctx.trackId);
	}

	const mergedArgs =
		input.args ?? defaultArgs(input.patternId, ctx.patternArgs);

	const created = await invoke<TrackScore>("create_track_score", {
		payload: {
			scoreId: ctx.scoreId,
			trackId: ctx.trackId,
			patternId: input.patternId,
			startTime: input.startTime,
			endTime: input.endTime,
			zIndex: input.zIndex,
			blendMode: input.blendMode ?? null,
			args: mergedArgs,
		},
	});

	const annotations = await reloadAnnotationsFromServer(
		ctx.scoreId,
		ctx.patterns,
	);
	return { value: created, annotations };
}

/** Update fields of an existing clip. Caller is responsible for collision
 * checks before calling — this just persists the change and returns the
 * refreshed cache. */
export async function updateAnnotationMutation(
	ctx: MutationContext,
	input: UpdateInput,
): Promise<MutationResult<TrackScore | null>> {
	const existing = ctx.annotations.find((a) => a.id === input.id);
	if (!existing) return { value: null, annotations: ctx.annotations };

	await invoke("update_track_score", { payload: input });

	const next: TrackScore = {
		...existing,
		startTime: input.startTime ?? existing.startTime,
		endTime: input.endTime ?? existing.endTime,
		zIndex: input.zIndex ?? existing.zIndex,
		blendMode: input.blendMode == null ? existing.blendMode : input.blendMode,
		args: input.args === undefined ? existing.args : input.args,
	};
	const enriched = enrichAnnotation(next, ctx.patterns);
	const annotations = ctx.annotations.map((a) =>
		a.id === input.id ? enriched : a,
	);
	return { value: next, annotations };
}

/** Delete a clip. */
export async function deleteAnnotationMutation(
	ctx: MutationContext,
	id: string,
): Promise<MutationResult<boolean>> {
	await invoke<void>("delete_track_score", { id });
	const annotations = ctx.annotations.filter((a) => a.id !== id);
	return { value: true, annotations };
}
