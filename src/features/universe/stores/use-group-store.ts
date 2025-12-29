import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { PatchedFixture } from "@/bindings/fixtures";
import type { FixtureGroup, FixtureGroupNode } from "@/bindings/groups";

interface GroupState {
	// Data
	groups: FixtureGroupNode[];
	selectedGroupId: number | null;
	isLoading: boolean;

	// Actions
	fetchGroups: (venueId: number) => Promise<void>;
	createGroup: (
		venueId: number,
		name?: string,
		axisLr?: number,
		axisFb?: number,
		axisAb?: number,
	) => Promise<FixtureGroup | null>;
	updateGroup: (
		id: number,
		name?: string,
		axisLr?: number,
		axisFb?: number,
		axisAb?: number,
	) => Promise<void>;
	deleteGroup: (id: number) => Promise<boolean>;
	addFixtureToGroup: (fixtureId: string, groupId: number) => Promise<void>;
	removeFixtureFromGroup: (fixtureId: string, groupId: number) => Promise<void>;
	setSelectedGroupId: (id: number | null) => void;
	previewSelectionQuery: (
		venueId: number,
		query: string,
		seed?: number,
	) => Promise<PatchedFixture[]>;
}

export const useGroupStore = create<GroupState>((set, get) => ({
	groups: [],
	selectedGroupId: null,
	isLoading: false,

	fetchGroups: async (venueId: number) => {
		set({ isLoading: true });
		try {
			// Ensure all fixtures are in at least one group
			await invoke<number>("ensure_fixtures_grouped", { venueId });

			const groups = await invoke<FixtureGroupNode[]>("get_grouped_hierarchy", {
				venueId,
			});
			set({ groups, isLoading: false });
		} catch (error) {
			console.error("Failed to fetch groups:", error);
			set({ isLoading: false });
		}
	},

	createGroup: async (venueId, name, axisLr, axisFb, axisAb) => {
		try {
			const group = await invoke<FixtureGroup>("create_group", {
				venueId,
				name: name ?? null,
				axisLr: axisLr ?? null,
				axisFb: axisFb ?? null,
				axisAb: axisAb ?? null,
			});
			// Refresh groups
			await get().fetchGroups(venueId);
			return group;
		} catch (error) {
			console.error("Failed to create group:", error);
			return null;
		}
	},

	updateGroup: async (id, name, axisLr, axisFb, axisAb) => {
		try {
			await invoke("update_group", {
				id,
				name: name ?? null,
				axisLr: axisLr ?? null,
				axisFb: axisFb ?? null,
				axisAb: axisAb ?? null,
			});
			// Find venueId from current groups to refresh
			const { groups } = get();
			const group = groups.find((g) => g.groupId === id);
			if (group) {
				// We need venue ID but don't have it directly - would need to store it
				// For now, caller should refresh manually
			}
		} catch (error) {
			console.error("Failed to update group:", error);
		}
	},

	deleteGroup: async (id) => {
		try {
			await invoke("delete_group", { id });
			return true;
		} catch (error) {
			console.error("Failed to delete group:", error);
			return false;
		}
	},

	addFixtureToGroup: async (fixtureId, groupId) => {
		try {
			await invoke("add_fixture_to_group", { fixtureId, groupId });
		} catch (error) {
			console.error("Failed to add fixture to group:", error);
		}
	},

	removeFixtureFromGroup: async (fixtureId, groupId) => {
		try {
			await invoke("remove_fixture_from_group", { fixtureId, groupId });
		} catch (error) {
			console.error("Failed to remove fixture from group:", error);
		}
	},

	setSelectedGroupId: (id) => set({ selectedGroupId: id }),

	previewSelectionQuery: async (venueId, query, seed) => {
		try {
			const fixtures = await invoke<PatchedFixture[]>(
				"preview_selection_query",
				{
					venueId,
					query,
					seed,
				},
			);
			return fixtures;
		} catch (error) {
			console.error("Failed to preview selection query:", error);
			return [];
		}
	},
}));
