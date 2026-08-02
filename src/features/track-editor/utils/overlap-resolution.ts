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

/** Apply overlap resolution to an in-memory document candidate. Persistence is
 * deliberately outside this helper: one user gesture must publish the entire
 * resulting score through a single compare-and-swap transaction. */
export function applyOverlapActions(
	annotations: readonly TimelineAnnotation[],
	actions: OverlapAction[],
	createId: () => string = () => crypto.randomUUID(),
): { annotations: TimelineAnnotation[]; newIds: string[] } {
	let candidate = [...annotations];
	const newIds: string[] = [];

	for (const action of actions) {
		switch (action.type) {
			case "delete":
				candidate = candidate.filter(
					(annotation) => annotation.id !== action.id,
				);
				break;
			case "trim-end":
				candidate = candidate.map((annotation) =>
					annotation.id === action.id
						? { ...annotation, endTime: action.newEndTime }
						: annotation,
				);
				break;
			case "trim-start":
				candidate = candidate.map((annotation) =>
					annotation.id === action.id
						? { ...annotation, startTime: action.newStartTime }
						: annotation,
				);
				break;
			case "split": {
				const draftId = createId();
				const now = new Date().toISOString();
				candidate = candidate.map((annotation) =>
					annotation.id === action.id
						? { ...annotation, endTime: action.leftEnd }
						: annotation,
				);
				candidate.push({
					...action.annotation,
					id: draftId,
					uid: null,
					startTime: action.rightStart,
					createdAt: now,
					updatedAt: now,
				});
				newIds.push(draftId);
				break;
			}
		}
	}

	return { annotations: candidate, newIds };
}
