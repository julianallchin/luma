import { invoke } from "@tauri-apps/api/core";
import { useMemo } from "react";
import { create } from "zustand";
import type { PatchedFixture } from "@/bindings/fixtures";
import type { ResolvedNode, ResolvedVenue } from "@/bindings/venue-graph";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";

/**
 * The solved venue, as this app draws it.
 *
 * Poses are not stored anywhere — `get_resolved_venue` walks the venue graph
 * and returns a world transform per node, in the stored convention (metres and
 * radians, data space, Z-up). Drawing is the whole contract: the graph is
 * edited in the gpui builder, and this app has no write verbs.
 *
 * `position`/`rotation` are **world**, not parent-local, so a consumer places
 * each node flat rather than nesting groups by `parentId`.
 */
interface VenueState {
	venueId: string | null;
	/** Depth-first from the root, children in id order. */
	nodes: ResolvedNode[];
	byId: Map<string, ResolvedNode>;
	/** One line per thing the solve had to decide for us. */
	warnings: string[];

	initialize: (venueId: string) => Promise<void>;
	refresh: () => Promise<void>;
}

export const useVenueStore = create<VenueState>((set, get) => ({
	venueId: null,
	nodes: [],
	byId: new Map(),
	warnings: [],

	initialize: async (venueId) => {
		set({ venueId });
		await get().refresh();
	},

	refresh: async () => {
		const { venueId } = get();
		if (!venueId) return;
		try {
			const venue = await invoke<ResolvedVenue>("get_resolved_venue", {
				venueId,
			});
			set({
				nodes: venue.nodes,
				byId: new Map(venue.nodes.map((n) => [n.id, n])),
				warnings: venue.warnings,
			});
		} catch (err) {
			console.error("[venue] get_resolved_venue failed", err);
		}
	},
}));

/**
 * Every node that carries a mesh — the structure and set pieces, not fixtures.
 *
 * A fixture's node is drawn by the fixture layer instead (see
 * {@link usePlacedFixtures}), which needs the patch row for its definition.
 */
export function useStructureNodes(): ResolvedNode[] {
	const nodes = useVenueStore((s) => s.nodes);
	return useMemo(
		() => nodes.filter((n) => n.kind !== "fixture" && n.catalogRef !== null),
		[nodes],
	);
}

/**
 * Patched fixtures that are *placed*, carrying their solved pose.
 *
 * A fixture node's id is its `fixtures` row id, and the node's pose replaces
 * the row's vestigial `posX`/`rotX` columns — so the pose fields here are
 * overwritten from the solve and every consumer keeps reading `.posX`. A
 * fixture with no node is patched but unplaced and is simply absent: there is
 * nowhere to draw it.
 */
export function usePlacedFixtures(): PatchedFixture[] {
	const patched = useFixtureStore((s) => s.patchedFixtures);
	const byId = useVenueStore((s) => s.byId);
	return useMemo(
		() =>
			patched.flatMap((f) => {
				const node = byId.get(f.id);
				if (!node) return [];
				return [
					{
						...f,
						posX: node.position[0],
						posY: node.position[1],
						posZ: node.position[2],
						rotX: node.rotation[0],
						rotY: node.rotation[1],
						rotZ: node.rotation[2],
					},
				];
			}),
		[patched, byId],
	);
}
