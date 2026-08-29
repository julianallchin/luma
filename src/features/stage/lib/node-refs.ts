/**
 * Map of `nodeId -> three.js Group` for the drawn venue nodes.
 *
 * A node's group exists only once its GLB has resolved through Suspense, so
 * the map doubles as the "this venue has finished drawing" signal the golden
 * harness waits on before it captures a frame.
 */

import type { Group } from "three";

const REFS = new Map<string, Group>();

export function registerNodeGroup(id: string, group: Group): void {
	REFS.set(id, group);
}

export function unregisterNodeGroup(id: string): void {
	REFS.delete(id);
}

export function getNodeGroup(id: string): Group | null {
	return REFS.get(id) ?? null;
}
