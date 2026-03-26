import { createContext, useContext } from "react";
import type { PrimitiveState } from "@/bindings/universe";
import { universeStore } from "../stores/universe-state-store";

/**
 * Optional override for primitive state lookups.
 * When provided (e.g. in pattern preview), fixtures read from this instead of
 * the global universeStore. The value is a getter that returns a lookup fn.
 */
export const PrimitiveOverrideContext = createContext<
	(() => (id: string) => PrimitiveState | undefined) | null
>(null);

/**
 * Hook to get the latest state for a primitive (fixture or head).
 * Uses a ref to avoid re-rendering on every frame, returns a getter.
 * Use inside a useFrame loop.
 */
export function usePrimitiveState(id: string) {
	const overrideGetter = useContext(PrimitiveOverrideContext);
	if (overrideGetter) {
		return () => overrideGetter()(id);
	}
	const get = () => universeStore.getPrimitive(id);
	return get;
}
