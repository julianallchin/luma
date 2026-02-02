import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { PatchedFixture } from "@/bindings/fixtures";
import type { FixtureGroup, FixtureGroupNode } from "@/bindings/groups";

interface GroupState {
	// Data
	groups: FixtureGroupNode[];
	selectedGroupId: number | null;
	isLoading: boolean;
	venueId: number | null;

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
		name: string,
		axisLr?: number | null,
		axisFb?: number | null,
		axisAb?: number | null,
	) => Promise<void>;
	deleteGroup: (id: number) => Promise<boolean>;
	addFixtureToGroup: (
		fixtureId: string,
		groupId: number,
		fixture: { id: string; label: string },
	) => Promise<void>;
	removeFixtureFromGroup: (fixtureId: string, groupId: number) => Promise<void>;
	addTagToGroup: (groupId: number, tag: string) => Promise<void>;
	removeTagFromGroup: (groupId: number, tag: string) => Promise<void>;
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
	venueId: null,

	fetchGroups: async (venueId: number) => {
		const isInitialLoad = get().groups.length === 0;
		if (isInitialLoad) {
			set({ isLoading: true });
		}
		set({ venueId });

		try {
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

			// Optimistic: add the new group to the list
			set((state) => ({
				groups: [
					...state.groups,
					{
						groupId: group.id,
						groupName: group.name,
						tags: [],
						axisLr: group.axisLr,
						axisFb: group.axisFb,
						axisAb: group.axisAb,
						fixtures: [],
					},
				],
			}));

			return group;
		} catch (error) {
			console.error("Failed to create group:", error);
			return null;
		}
	},

	updateGroup: async (id, name, axisLr, axisFb, axisAb) => {
		// Optimistic update
		set((state) => ({
			groups: state.groups.map((g) =>
				g.groupId === id
					? {
							...g,
							groupName: name,
							axisLr: axisLr ?? g.axisLr,
							axisFb: axisFb ?? g.axisFb,
							axisAb: axisAb ?? g.axisAb,
						}
					: g,
			),
		}));

		try {
			await invoke("update_group", {
				id,
				name: name ?? null,
				axisLr: axisLr ?? null,
				axisFb: axisFb ?? null,
				axisAb: axisAb ?? null,
			});
		} catch (error) {
			console.error("Failed to update group:", error);
			// Revert on error
			const { venueId } = get();
			if (venueId) await get().fetchGroups(venueId);
		}
	},

	deleteGroup: async (id) => {
		// Optimistic update
		const previousGroups = get().groups;
		set((state) => ({
			groups: state.groups.filter((g) => g.groupId !== id),
			selectedGroupId:
				state.selectedGroupId === id ? null : state.selectedGroupId,
		}));

		try {
			await invoke("delete_group", { id });
			return true;
		} catch (error) {
			console.error("Failed to delete group:", error);
			// Revert on error
			set({ groups: previousGroups });
			return false;
		}
	},

	addFixtureToGroup: async (fixtureId, groupId, fixture) => {
		// Optimistic update
		set((state) => ({
			groups: state.groups.map((g) =>
				g.groupId === groupId
					? {
							...g,
							fixtures: g.fixtures.some((f) => f.id === fixtureId)
								? g.fixtures
								: [...g.fixtures, fixture],
						}
					: g,
			),
		}));

		try {
			await invoke("add_fixture_to_group", { fixtureId, groupId });
		} catch (error) {
			console.error("Failed to add fixture to group:", error);
			// Revert on error
			const { venueId } = get();
			if (venueId) await get().fetchGroups(venueId);
		}
	},

	removeFixtureFromGroup: async (fixtureId, groupId) => {
		// Optimistic update
		set((state) => ({
			groups: state.groups.map((g) =>
				g.groupId === groupId
					? {
							...g,
							fixtures: g.fixtures.filter((f) => f.id !== fixtureId),
						}
					: g,
			),
		}));

		try {
			await invoke("remove_fixture_from_group", { fixtureId, groupId });
		} catch (error) {
			console.error("Failed to remove fixture from group:", error);
			// Revert on error
			const { venueId } = get();
			if (venueId) await get().fetchGroups(venueId);
		}
	},

	addTagToGroup: async (groupId, tag) => {
		// Optimistic update
		set((state) => ({
			groups: state.groups.map((g) =>
				g.groupId === groupId
					? {
							...g,
							tags: g.tags.includes(tag) ? g.tags : [...g.tags, tag],
						}
					: g,
			),
		}));

		try {
			await invoke("add_tag_to_group", { groupId, tag });
		} catch (error) {
			console.error("Failed to add tag:", error);
			// Revert on error
			const { venueId } = get();
			if (venueId) await get().fetchGroups(venueId);
		}
	},

	removeTagFromGroup: async (groupId, tag) => {
		// Optimistic update
		set((state) => ({
			groups: state.groups.map((g) =>
				g.groupId === groupId
					? {
							...g,
							tags: g.tags.filter((t) => t !== tag),
						}
					: g,
			),
		}));

		try {
			await invoke("remove_tag_from_group", { groupId, tag });
		} catch (error) {
			console.error("Failed to remove tag:", error);
			// Revert on error
			const { venueId } = get();
			if (venueId) await get().fetchGroups(venueId);
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
