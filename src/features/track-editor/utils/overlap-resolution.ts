import { invoke } from "@tauri-apps/api/core";
import type { TrackScore as TrackScoreBinding } from "@/bindings/schema";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";

export type OverlapAction =
	| { type: "delete"; id: number }
	| { type: "trim-end"; id: number; newEndTime: number }
	| { type: "trim-start"; id: number; newStartTime: number }
	| {
			type: "split";
			id: number;
			leftEnd: number;
			rightStart: number;
			annotation: TimelineAnnotation;
	  };

/**
 * Compute the actions needed to clear a time region on given z-indexes.
 * Pure function — does not mutate or invoke backend.
 */
export function resolveOverlaps(
	annotations: TimelineAnnotation[],
	regionStart: number,
	regionEnd: number,
	zIndexes: Set<number>,
	excludeIds: Set<number>,
): OverlapAction[] {
	const actions: OverlapAction[] = [];

	for (const ann of annotations) {
		if (excludeIds.has(ann.id)) continue;
		if (!zIndexes.has(ann.zIndex)) continue;
		if (ann.startTime >= regionEnd || ann.endTime <= regionStart) continue;

		const fullyContained =
			ann.startTime >= regionStart && ann.endTime <= regionEnd;
		const startsBeforeEndsInside =
			ann.startTime < regionStart &&
			ann.endTime > regionStart &&
			ann.endTime <= regionEnd;
		const startsInsideEndsAfter =
			ann.startTime >= regionStart &&
			ann.startTime < regionEnd &&
			ann.endTime > regionEnd;
		const spansEntireRegion =
			ann.startTime < regionStart && ann.endTime > regionEnd;

		if (fullyContained) {
			actions.push({ type: "delete", id: ann.id });
		} else if (startsBeforeEndsInside) {
			actions.push({ type: "trim-end", id: ann.id, newEndTime: regionStart });
		} else if (startsInsideEndsAfter) {
			actions.push({
				type: "trim-start",
				id: ann.id,
				newStartTime: regionEnd,
			});
		} else if (spansEntireRegion) {
			actions.push({
				type: "split",
				id: ann.id,
				leftEnd: regionStart,
				rightStart: regionEnd,
				annotation: ann,
			});
		}
	}

	return actions;
}

/**
 * Execute overlap actions against the backend.
 * Returns IDs of any newly-created annotations (from splits).
 */
export async function applyOverlapActions(
	actions: OverlapAction[],
	trackId: number,
): Promise<number[]> {
	const newIds: number[] = [];

	for (const action of actions) {
		switch (action.type) {
			case "delete":
				await invoke<void>("delete_track_score", { id: action.id });
				break;
			case "trim-end":
				await invoke("update_track_score", {
					payload: { id: action.id, endTime: action.newEndTime },
				});
				break;
			case "trim-start":
				await invoke("update_track_score", {
					payload: { id: action.id, startTime: action.newStartTime },
				});
				break;
			case "split": {
				// Trim the original to the left part
				await invoke("update_track_score", {
					payload: { id: action.id, endTime: action.leftEnd },
				});
				// Create the right part
				const ann = action.annotation;
				const created = await invoke<TrackScoreBinding>("create_track_score", {
					payload: {
						trackId,
						patternId: ann.patternId,
						startTime: action.rightStart,
						endTime: ann.endTime,
						zIndex: ann.zIndex,
						blendMode: ann.blendMode,
						args: ann.args ?? {},
					},
				});
				newIds.push(created.id);
				break;
			}
		}
	}

	return newIds;
}
