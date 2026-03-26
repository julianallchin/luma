import { invoke } from "@tauri-apps/api/core";
import type { TrackScore as TrackScoreBinding } from "@/bindings/schema";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { MIN_ANNOTATION_DURATION } from "./timeline-constants";

export type OverlapAction =
	| { type: "delete"; id: string }
	| { type: "trim-end"; id: string; newEndTime: number }
	| { type: "trim-start"; id: string; newStartTime: number }
	| {
			type: "split";
			id: string;
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
	excludeIds: Set<string>,
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
			// If trimming would make it too short, delete instead
			if (regionStart - ann.startTime < MIN_ANNOTATION_DURATION) {
				actions.push({ type: "delete", id: ann.id });
			} else {
				actions.push({ type: "trim-end", id: ann.id, newEndTime: regionStart });
			}
		} else if (startsInsideEndsAfter) {
			// If trimming would make it too short, delete instead
			if (ann.endTime - regionEnd < MIN_ANNOTATION_DURATION) {
				actions.push({ type: "delete", id: ann.id });
			} else {
				actions.push({
					type: "trim-start",
					id: ann.id,
					newStartTime: regionEnd,
				});
			}
		} else if (spansEntireRegion) {
			const leftDuration = regionStart - ann.startTime;
			const rightDuration = ann.endTime - regionEnd;
			if (
				leftDuration < MIN_ANNOTATION_DURATION &&
				rightDuration < MIN_ANNOTATION_DURATION
			) {
				// Both halves too short — delete entirely
				actions.push({ type: "delete", id: ann.id });
			} else if (leftDuration < MIN_ANNOTATION_DURATION) {
				// Left half too short — trim start instead of split
				actions.push({
					type: "trim-start",
					id: ann.id,
					newStartTime: regionEnd,
				});
			} else if (rightDuration < MIN_ANNOTATION_DURATION) {
				// Right half too short — trim end instead of split
				actions.push({ type: "trim-end", id: ann.id, newEndTime: regionStart });
			} else {
				actions.push({
					type: "split",
					id: ann.id,
					leftEnd: regionStart,
					rightStart: regionEnd,
					annotation: ann,
				});
			}
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
	scoreId: string,
	trackId: string,
): Promise<string[]> {
	const newIds: string[] = [];

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
						scoreId,
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
