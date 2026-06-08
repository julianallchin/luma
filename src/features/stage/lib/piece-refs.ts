/**
 * Map of `pieceId -> three.js Group`. Lets the snap solver and other
 * helpers read the live `matrixWorld` of any rendered piece without
 * threading refs through React props.
 *
 * `StagePieceNode` registers / unregisters its group on mount / unmount.
 */

import type { Group } from "three";

const REFS = new Map<string, Group>();

export function registerPieceGroup(id: string, group: Group): void {
	REFS.set(id, group);
}

export function unregisterPieceGroup(id: string): void {
	REFS.delete(id);
}

export function getPieceGroup(id: string): Group | null {
	return REFS.get(id) ?? null;
}
