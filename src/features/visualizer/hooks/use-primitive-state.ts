import { universeStore } from "../stores/universe-state-store";

/**
 * Hook to get the latest state for a primitive (fixture or head).
 * Uses a ref to avoid re-rendering on every frame, returns a getter.
 * Use inside a useFrame loop.
 */
export function usePrimitiveState(id: string) {
	const get = () => universeStore.getPrimitive(id);
	return get;
}
