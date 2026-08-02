import type { TrackScore } from "@/bindings/schema";
import type { DslAnnotation } from "@/lib/dsl";

export type MaterializeTrackScoresOptions = {
	now?: string;
	newId?: () => string;
};

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

/**
 * Adapt a compiled DSL document to the UI's TrackScore transport shape.
 *
 * Existing IDs preserve row identity. A new clip gets a client-only correlation
 * ID so the canonical host transaction can return its allocated database ID in
 * `idMap`; the host ignores the placeholder UID and timestamps. Accepting an
 * unknown authored ID would make an accidental foreign reference look like a
 * new clip, so authored IDs must belong to the exact base snapshot.
 */
export function materializeTrackScores(
	compiled: DslAnnotation[],
	existing: TrackScore[],
	scoreId: string,
	options?: MaterializeTrackScoresOptions,
): TrackScore[] {
	const now = options?.now ?? new Date().toISOString();
	const newId = options?.newId ?? (() => crypto.randomUUID());
	const existingById = new Map(existing.map((score) => [score.id, score]));
	const usedIds = new Set<string>();

	return compiled.map((annotation) => {
		if (annotation.id !== undefined && !existingById.has(annotation.id)) {
			throw new Error(
				`Clip id "${annotation.id}" does not belong to this score. Omit the id to create a new clip.`,
			);
		}

		const id = annotation.id ?? newId();
		if (usedIds.has(id)) {
			throw new Error(`Clip id "${id}" appears more than once`);
		}
		usedIds.add(id);

		const previous = existingById.get(id);
		const mutable = {
			patternId: annotation.patternId,
			startTime: annotation.startTime,
			endTime: annotation.endTime,
			zIndex: annotation.zIndex,
			blendMode: annotation.blendMode,
			args: annotation.args,
		};
		const unchanged =
			previous !== undefined &&
			previous.patternId === mutable.patternId &&
			previous.startTime === mutable.startTime &&
			previous.endTime === mutable.endTime &&
			previous.zIndex === mutable.zIndex &&
			previous.blendMode === mutable.blendMode &&
			jsonEqual(previous.args, mutable.args);

		return {
			id,
			uid: previous?.uid ?? null,
			scoreId,
			...mutable,
			createdAt: previous?.createdAt ?? now,
			updatedAt: unchanged ? previous.updatedAt : now,
		};
	});
}

function jsonEqual(left: unknown, right: unknown): boolean {
	if (Object.is(left, right)) return true;
	if (Array.isArray(left) && Array.isArray(right)) {
		return (
			left.length === right.length &&
			left.every((value, index) => jsonEqual(value, right[index]))
		);
	}
	if (
		typeof left === "object" &&
		left !== null &&
		!Array.isArray(left) &&
		typeof right === "object" &&
		right !== null &&
		!Array.isArray(right)
	) {
		const leftRecord = left as Record<string, unknown>;
		const rightRecord = right as Record<string, unknown>;
		const leftKeys = Object.keys(leftRecord);
		return (
			leftKeys.length === Object.keys(rightRecord).length &&
			leftKeys.every(
				(key) =>
					Object.hasOwn(rightRecord, key) &&
					jsonEqual(leftRecord[key], rightRecord[key]),
			)
		);
	}
	return false;
}
