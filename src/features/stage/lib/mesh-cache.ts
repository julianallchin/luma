/**
 * Per-mesh geometry cache.
 *
 * The snap solver needs to know each mesh's bounding box and resolved
 * socket positions to compute candidate snaps. Bbox can only be measured
 * once a GLB is loaded; mesh components (`StagePieceObject`, `StageGhost`)
 * register their geometry into this cache on first load.
 *
 * Lookups before a mesh has loaded return `null`; the solver treats
 * un-cached pieces as "no sockets" and they simply contribute no snap
 * candidates until their first instance renders.
 */

import type { Box3 } from "three";
import type { ResolvedSocket } from "./sockets";
import { resolveSocket } from "./sockets";
import { getStageMesh } from "./stage-meshes";

interface MeshGeometry {
	bbox: Box3;
	sockets: ResolvedSocket[];
}

const CACHE = new Map<string, MeshGeometry>();
const listeners = new Set<() => void>();

/** Subscribe to cache updates (so React components can re-render). */
export function subscribeMeshCache(listener: () => void): () => void {
	listeners.add(listener);
	return () => {
		listeners.delete(listener);
	};
}

function notify() {
	for (const l of listeners) l();
}

/**
 * Called by mesh-loading components once they've measured their GLB.
 * Idempotent — repeated calls with the same meshPath are ignored.
 */
export function registerMeshGeometry(meshPath: string, bbox: Box3): void {
	if (CACHE.has(meshPath)) return;
	const def = getStageMesh(meshPath);
	if (!def) return;
	const resolved = def.sockets.map((s) => resolveSocket(s, bbox));
	CACHE.set(meshPath, { bbox: bbox.clone(), sockets: resolved });
	notify();
}

export function getMeshGeometry(meshPath: string): MeshGeometry | null {
	return CACHE.get(meshPath) ?? null;
}

export function getMeshSockets(meshPath: string): ResolvedSocket[] {
	return CACHE.get(meshPath)?.sockets ?? [];
}

/** For tests / debug. */
export function clearMeshCache(): void {
	CACHE.clear();
	notify();
}
