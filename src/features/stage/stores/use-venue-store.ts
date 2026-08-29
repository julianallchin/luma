import { invoke } from "@tauri-apps/api/core";
import { useMemo } from "react";
import { create } from "zustand";
import type { PatchedFixture } from "@/bindings/fixtures";
import type {
	ResolvedNode,
	ResolvedUnplaced,
	ResolvedVenue,
} from "@/bindings/venue-graph";
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
	/**
	 * Branches with no pose, by their root: the patch tray, and anything a
	 * detach left hanging. Nothing draws them — they are what stops a detached
	 * wing from simply disappearing with no way to ask where it went.
	 */
	unplaced: ResolvedUnplaced[];

	initialize: (venueId: string) => Promise<void>;
	refresh: () => Promise<void>;
}

export const useVenueStore = create<VenueState>((set, get) => ({
	venueId: null,
	nodes: [],
	byId: new Map(),
	warnings: [],
	unplaced: [],

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
				unplaced: venue.unplaced,
			});
		} catch (err) {
			console.error("[venue] get_resolved_venue failed", err);
		}
	},
}));

/**
 * Every node that stands for one physical object — the structure and set
 * pieces, not fixtures.
 *
 * `setPiece` is the resolver's own answer (`NodePose::is_set_piece`), carried
 * on the wire rather than re-derived here: an array's *anchor* is a seat with
 * no geometry that carries its members' `catalogRef`, so a filter written from
 * `kind` and `catalogRef` alone draws N+1 meshes for an array of N, with the
 * extra one inside the middle member.
 *
 * A fixture's node is drawn by the fixture layer instead (see
 * {@link usePlacedFixtures}), which needs the patch row for its definition.
 */
export function useStructureNodes(): ResolvedNode[] {
	const nodes = useVenueStore((s) => s.nodes);
	return useMemo(() => structureNodes(nodes), [nodes]);
}

/** {@link useStructureNodes} without the hook, so it can be tested. */
export function structureNodes(nodes: ResolvedNode[]): ResolvedNode[] {
	return nodes.filter((n) => n.setPiece);
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
