import type { TrackEditResult, TrackScore } from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";
import { trackScoreSnapshot } from "./track-score-snapshot";

/** Publish one complete score candidate as one relational compare-and-swap.
 * This is the sole frontend primitive for a gesture that affects multiple
 * clips; callers never fan a gesture out into per-row mutation commands. */
export function replaceTrackScoreDocument(
	scoreId: string,
	trackId: string,
	base: readonly TrackScore[],
	candidate: readonly TrackScore[],
): Promise<TrackEditResult> {
	const request = {
		scoreId,
		trackId,
		baseScores: trackScoreSnapshot(base),
		scores: trackScoreSnapshot(candidate),
		operationId: crypto.randomUUID(),
	};
	return invoke<TrackEditResult>("replace_track_scores", request).catch(() =>
		invoke<TrackEditResult>("replace_track_scores", request),
	);
}

export function rebaseTrackScoreIds<T extends TrackScore>(
	scores: readonly T[],
	idMap: TrackEditResult["idMap"],
): T[] {
	return scores.map((score) => {
		const id = idMap[score.id];
		return id === undefined ? score : { ...score, id };
	});
}

/** Hydrate the host's authoritative semantic document for UI rendering. A
 * replay may return a main tip newer than the submitted candidate, so callers
 * must use these clips rather than assuming their candidate is still current. */
export function hydrateTrackReplacement(
	scoreId: string,
	knownScores: readonly TrackScore[],
	result: TrackEditResult,
): TrackScore[] {
	const draftByStored = new Map(
		Object.entries(result.idMap).map(([draft, stored]) => [stored, draft]),
	);
	const known = new Map(knownScores.map((score) => [score.id, score]));
	const now = new Date().toISOString();
	return result.clips.map((clip) => {
		const source =
			known.get(clip.id) ??
			(draftByStored.has(clip.id)
				? known.get(draftByStored.get(clip.id) as string)
				: undefined);
		return {
			id: clip.id,
			uid: source?.uid ?? null,
			scoreId,
			patternId: clip.patternId,
			startTime: clip.startTime,
			endTime: clip.endTime,
			zIndex: clip.zIndex,
			blendMode: clip.blendMode,
			args: clip.args,
			createdAt: source?.createdAt ?? now,
			updatedAt: source?.updatedAt ?? now,
		};
	});
}
