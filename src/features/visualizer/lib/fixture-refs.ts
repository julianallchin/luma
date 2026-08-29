/**
 * Map of `fixtureId -> three.js Group`, so a caller can read a drawn
 * fixture's live world pose without threading refs through props. The stage
 * feature's `node-refs.ts` is the same map for venue nodes.
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
