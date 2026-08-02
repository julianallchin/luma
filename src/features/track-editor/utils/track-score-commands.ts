import type { BlendMode, TrackEditResult } from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";

export type TrackScoreUpdate = {
	id: string;
	startTime?: number;
	endTime?: number;
	zIndex?: number;
	blendMode?: BlendMode | null;
	args?: Record<string, unknown>;
};

async function retryOnce<T>(
	command: string,
	args: Record<string, unknown>,
): Promise<T> {
	try {
		return await invoke<T>(command, args);
	} catch {
		return invoke<T>(command, args);
	}
}

/** One stable operation identity spans the ambiguous IPC response boundary. */
export function updateTrackScore(
	scoreId: string,
	trackId: string,
	update: TrackScoreUpdate,
): Promise<TrackEditResult> {
	return retryOnce<TrackEditResult>("update_track_score", {
		payload: {
			...update,
			scoreId,
			trackId,
			operationId: crypto.randomUUID(),
		},
	});
}

/** Delete is replay-safe for the same reason: a lost successful response must
 * never turn the retry into an unknown/missing-clip failure. */
export function deleteTrackScore(
	scoreId: string,
	trackId: string,
	id: string,
): Promise<TrackEditResult> {
	return retryOnce<TrackEditResult>("delete_track_score", {
		payload: {
			id,
			scoreId,
			trackId,
			operationId: crypto.randomUUID(),
		},
	});
}
