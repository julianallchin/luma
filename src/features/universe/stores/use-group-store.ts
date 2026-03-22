import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { PatchedFixture } from "@/bindings/fixtures";
import type {
	FixtureGroup,
	FixtureGroupNode,
	MovementConfig,
} from "@/bindings/groups";

interface GroupState {
	// Data
	groups: FixtureGroupNode[];
	selectedGroupId: string | null;
	isLoading: boolean;
	venueId: string | null;

	// Actions
	fetchGroups: (venueId: string) => Promise<void>;
	createGroup: (
		venueId: string,
		name?: string,
		axisLr?: number,
		axisFb?: number,
		axisAb?: number,
	) => Promise<FixtureGroup | null>;
	updateGroup: (
		id: string,
		name: string,
		axisLr?: number | null,
		axisFb?: number | null,
		axisAb?: number | null,
	) => Promise<void>;
	deleteGroup: (id: string) => Promise<boolean>;
	addFixtureToGroup: (
		fixtureId: string,
		groupId: string,
		fixture: { id: string; label: string },
	) => Promise<void>;
	removeFixtureFromGroup: (fixtureId: string, groupId: string) => Promise<void>;
	updateMovementConfig: (
		groupId: string,
		config: MovementConfig | null,
	) => Promise<void>;
	setSelectedGroupId: (id: string | null) => void;
	previewSelectionQuery: (
		venueId: string,
		query: string,
		seed?: number,
	) => Promise<PatchedFixture[]>;
}

export const useGroupStore = create<GroupState>((set, get) => ({
	groups: [],
	selectedGroupId: null,
	isLoading: false,
	venueId: null,

	fetchGroups: async (venueId: string) => {
		const isInitialLoad = get().groups.length === 0;
		if (isInitialLoad) {
			set({ isLoading: true });
		}
		set({ venueId });

		try {
			await invoke<string>("ensure_fixtures_grouped", { venueId });
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
						fixtureType: "unknown",
						movementConfig: null,
						axisLr: group.axisLr,
						axisFb: group.axisFb,
						axisAb: group.axisAb,
						fixtures: [],
					} as FixtureGroupNode,
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
								: [
										...g.fixtures,
										{ ...fixture, fixtureType: "unknown" as const, heads: [] },
									],
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

	updateMovementConfig: async (groupId, config) => {
		// Optimistic update
		set((state) => ({
			groups: state.groups.map((g) =>
				g.groupId === groupId ? { ...g, movementConfig: config } : g,
			),
		}));

		try {
			await invoke("update_movement_config", {
				groupId,
				config,
			});
		} catch (error) {
			console.error("Failed to update movement config:", error);
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
