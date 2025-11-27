import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	FixtureDefinition,
	FixtureEntry,
	PatchedFixture,
} from "@/bindings/fixtures";

interface FixtureState {
	// Search
	searchQuery: string;
	searchResults: FixtureEntry[];
	isSearching: boolean;
	pageOffset: number;
	hasMore: boolean;

	// Selection
	selectedEntry: FixtureEntry | null;
	selectedDefinition: FixtureDefinition | null;
	isLoadingDefinition: boolean;

	// Patch
	patchedFixtures: PatchedFixture[];
	selectedPatchedId: string | null;

	// Actions
	setSearchQuery: (query: string) => void;
	search: (query: string, reset?: boolean) => Promise<void>;
	loadMore: () => Promise<void>;
	selectFixture: (entry: FixtureEntry) => Promise<void>;
	initialize: () => Promise<void>;

	// Patch Actions
	fetchPatchedFixtures: () => Promise<void>;
	setSelectedPatchedId: (id: string | null) => void;
	movePatchedFixture: (id: string, address: number) => Promise<void>;
	patchFixture: (
		universe: number,
		address: number,
		modeName: string,
		numChannels: number,
	) => Promise<void>;
	removePatchedFixture: (id: string) => Promise<void>;
}

const LIMIT = 50;

export const useFixtureStore = create<FixtureState>((set, get) => ({
	searchQuery: "",
	searchResults: [],
	isSearching: false,
	pageOffset: 0,
	hasMore: true,
	selectedEntry: null,
	selectedDefinition: null,
	isLoadingDefinition: false,
	patchedFixtures: [],
	selectedPatchedId: null,

	setSearchQuery: (query) => set({ searchQuery: query }),

	initialize: async () => {
		try {
			await invoke("initialize_fixtures");
			// Initial empty search to fill list
			get().search("", true);
			get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to initialize fixtures:", error);
		}
	},

	search: async (query, reset = false) => {
		const currentOffset = reset ? 0 : get().pageOffset;

		if (reset) {
			set({
				searchResults: [],
				pageOffset: 0,
				hasMore: true,
				isSearching: true,
			});
		} else {
			set({ isSearching: true });
		}

		try {
			const results = await invoke<FixtureEntry[]>("search_fixtures", {
				query,
				offset: currentOffset,
				limit: LIMIT,
			});

			set((state) => ({
				searchResults: reset ? results : [...state.searchResults, ...results],
				isSearching: false,
				pageOffset: currentOffset + results.length,
				hasMore: results.length === LIMIT,
			}));
		} catch (error) {
			console.error("Search failed:", error);
			set({ isSearching: false });
		}
	},

	loadMore: async () => {
		const { hasMore, isSearching, searchQuery } = get();
		if (!hasMore || isSearching) return;
		await get().search(searchQuery, false);
	},

	selectFixture: async (entry) => {
		set({
			selectedEntry: entry,
			selectedDefinition: null,
			isLoadingDefinition: true,
		});
		try {
			const def = await invoke<FixtureDefinition>("get_fixture_definition", {
				path: entry.path,
			});
			set({ selectedDefinition: def, isLoadingDefinition: false });
		} catch (error) {
			console.error("Failed to load definition:", error);
			set({ isLoadingDefinition: false });
		}
	},

	fetchPatchedFixtures: async () => {
		try {
			const fixtures = await invoke<PatchedFixture[]>("get_patched_fixtures");
			set((state) => ({
				patchedFixtures: fixtures,
				selectedPatchedId: fixtures.some(
					(f) => f.id === state.selectedPatchedId,
				)
					? state.selectedPatchedId
					: null,
			}));
		} catch (error) {
			console.error("Failed to fetch patched fixtures:", error);
		}
	},

	setSelectedPatchedId: (id) => set({ selectedPatchedId: id }),

	movePatchedFixture: async (id, address) => {
		try {
			// Optimistic update
			const current = get().patchedFixtures;
			const idx = current.findIndex((f) => f.id === id);
			if (idx === -1) return;
			const optimistic = [...current];
			optimistic[idx] = { ...optimistic[idx], address };
			set({ patchedFixtures: optimistic, selectedPatchedId: id });

			console.debug("[useFixtureStore] movePatchedFixture invoke", {
				id,
				address,
			});
			await invoke("move_patched_fixture", { id, address });
			console.debug("[useFixtureStore] movePatchedFixture success");
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to move patched fixture:", error);
			// Reload from DB to avoid drift if optimistic update failed
			await get().fetchPatchedFixtures();
		}
	},

	patchFixture: async (universe, address, modeName, numChannels) => {
		const { selectedEntry, selectedDefinition, patchedFixtures } = get();
		if (!selectedEntry || !selectedDefinition) return;

		try {
			const existingCount = patchedFixtures.filter(
				(f) => f.model === selectedEntry.model,
			).length;
			const label = `${selectedEntry.model} (${existingCount + 1})`;
			console.debug("[useFixtureStore] patchFixture invoke", {
				universe,
				address,
				numChannels,
				manufacturer: selectedEntry.manufacturer,
				model: selectedEntry.model,
				modeName,
				fixturePath: selectedEntry.path,
				label,
			});
			await invoke("patch_fixture", {
				universe,
				address,
				numChannels,
				manufacturer: selectedEntry.manufacturer,
				model: selectedEntry.model,
				modeName,
				fixturePath: selectedEntry.path,
				label,
			});
			console.debug("[useFixtureStore] patchFixture success");
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to patch fixture:", error);
		}
	},

	removePatchedFixture: async (id) => {
		try {
			await invoke("remove_patched_fixture", { id });
			set((state) => ({
				selectedPatchedId:
					state.selectedPatchedId === id ? null : state.selectedPatchedId,
			}));
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to remove patched fixture:", error);
		}
	},
}));
