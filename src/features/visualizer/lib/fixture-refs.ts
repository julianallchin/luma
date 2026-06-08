/**
 * Map of `fixtureId -> three.js Group`. Mirrors `piece-refs.ts` from the
 * stage feature so the unified transform gizmo can read live world poses
 * for both fixtures and stage pieces from a single API.
 *
 * `FixtureObject` registers / unregisters its group on mount / unmount.
 */

import type { Group } from "three";

const REFS = new Map<string, Group>();

export function registerFixtureGroup(id: string, group: Group): void {
	REFS.set(id, group);
}

export function unregisterFixtureGroup(id: string): void {
	REFS.delete(id);
}

export function getFixtureGroup(id: string): Group | null {
	return REFS.get(id) ?? null;
}
