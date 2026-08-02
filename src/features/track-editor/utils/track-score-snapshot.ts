import type { TrackScore } from "@/bindings/schema";

/**
 * Strip timeline-only presentation fields from a score snapshot before it
 * crosses the Tauri boundary. Full-document replacement uses this exact base
 * document for compare-and-swap, so callers should materialize their candidate
 * from the returned snapshot and send both in the same request.
 */
export function trackScoreSnapshot(
	scores: readonly TrackScore[],
): TrackScore[] {
	return scores.map((score) => ({
		id: score.id,
		uid: score.uid,
		scoreId: score.scoreId,
		patternId: score.patternId,
		startTime: score.startTime,
		endTime: score.endTime,
		zIndex: score.zIndex,
		blendMode: score.blendMode,
		args: score.args,
		createdAt: score.createdAt,
		updatedAt: score.updatedAt,
	}));
}
