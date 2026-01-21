import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	FixtureDefinition,
	FixtureEntry,
	PatchedFixture,
} from "@/bindings/fixtures";

interface FixtureState {
	// Venue context
	venueId: number | null;

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
	previewFixtureIds: string[];
	definitionsCache: Map<string, FixtureDefinition>;

	// Actions
	setVenueId: (venueId: number | null) => void;
	setSearchQuery: (query: string) => void;
	search: (query: string, reset?: boolean) => Promise<void>;
	loadMore: () => Promise<void>;
	selectFixture: (entry: FixtureEntry) => Promise<void>;
	initialize: (venueId?: number) => Promise<void>;
	getDefinition: (path: string) => Promise<FixtureDefinition | null>;

	// Patch Actions
	fetchPatchedFixtures: () => Promise<void>;
	setSelectedPatchedId: (id: string | null) => void;
	setPreviewFixtureIds: (ids: string[]) => void;
	clearPreviewFixtureIds: () => void;
	movePatchedFixture: (id: string, address: number) => Promise<void>;
	moveFixtureSpatial: (
		id: string,
		pos: { x: number; y: number; z: number },
		rot: { x: number; y: number; z: number },
	) => Promise<void>;
	patchFixture: (
		universe: number,
		address: number,
		modeName: string,
		numChannels: number,
	) => Promise<void>;
	removePatchedFixture: (id: string) => Promise<void>;
	updatePatchedFixtureLabel: (id: string, label: string) => Promise<void>;
}

const LIMIT = 50;

export const useFixtureStore = create<FixtureState>((set, get) => ({
	venueId: null,
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
	previewFixtureIds: [],
	definitionsCache: new Map(),

	setVenueId: (venueId) => set({ venueId }),
	setSearchQuery: (query) => set({ searchQuery: query }),

	initialize: async (venueId?: number) => {
		try {
			if (venueId !== undefined) {
				set({ venueId });
			}
			await invoke("initialize_fixtures");
			// Initial empty search to fill list
			get().search("", true);
			if (get().venueId !== null) {
				get().fetchPatchedFixtures();
			}
		} catch (error) {
			console.error("Failed to initialize fixtures:", error);
		}
	},

	getDefinition: async (path) => {
		const { definitionsCache } = get();
		if (definitionsCache.has(path)) {
			return definitionsCache.get(path) || null;
		}

		try {
			const def = await invoke<FixtureDefinition>("get_fixture_definition", {
				path,
			});
			const newCache = new Map(definitionsCache);
			newCache.set(path, def);
			set({ definitionsCache: newCache });
			return def;
		} catch (error) {
			console.error(`Failed to load definition for ${path}:`, error);
			return null;
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
		const { venueId } = get();
		if (venueId === null) {
			console.warn("Cannot fetch patched fixtures without venueId");
			return;
		}
		try {
			const fixtures = await invoke<PatchedFixture[]>("get_patched_fixtures", {
				venueId,
			});
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
	setPreviewFixtureIds: (ids) => set({ previewFixtureIds: ids }),
	clearPreviewFixtureIds: () => set({ previewFixtureIds: [] }),

	moveFixtureSpatial: async (id, pos, rot) => {
		const { venueId } = get();
		if (venueId === null) return;

		try {
			// Optimistic update
			const current = get().patchedFixtures;
			const idx = current.findIndex((f) => f.id === id);
			if (idx === -1) return;
			const optimistic = [...current];
			optimistic[idx] = {
				...optimistic[idx],
				posX: pos.x,
				posY: pos.y,
				posZ: pos.z,
				rotX: rot.x,
				rotY: rot.y,
				rotZ: rot.z,
			};
			set({ patchedFixtures: optimistic });

			await invoke("move_patched_fixture_spatial", {
				venueId,
				id,
				posX: pos.x,
				posY: pos.y,
				posZ: pos.z,
				rotX: rot.x,
				rotY: rot.y,
				rotZ: rot.z,
			});
		} catch (error) {
			console.error("Failed to move fixture spatially:", error);
			await get().fetchPatchedFixtures();
		}
	},

	movePatchedFixture: async (id, address) => {
		const { venueId } = get();
		if (venueId === null) return;

		try {
			// Optimistic update
			const current = get().patchedFixtures;
			const idx = current.findIndex((f) => f.id === id);
			if (idx === -1) return;
			const optimistic = [...current];
			optimistic[idx] = { ...optimistic[idx], address: BigInt(address) };
			set({ patchedFixtures: optimistic, selectedPatchedId: id });

			console.debug("[useFixtureStore] movePatchedFixture invoke", {
				venueId,
				id,
				address,
			});
			await invoke("move_patched_fixture", { venueId, id, address });
			console.debug("[useFixtureStore] movePatchedFixture success");
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to move patched fixture:", error);
			// Reload from DB to avoid drift if optimistic update failed
			await get().fetchPatchedFixtures();
		}
	},

	patchFixture: async (universe, address, modeName, numChannels) => {
		const { selectedEntry, selectedDefinition, patchedFixtures, venueId } =
			get();
		if (!selectedEntry || !selectedDefinition || venueId === null) return;

		try {
			const existingCount = patchedFixtures.filter(
				(f) => f.model === selectedEntry.model,
			).length;
			const label = `${selectedEntry.model} (${existingCount + 1})`;
			console.debug("[useFixtureStore] patchFixture invoke", {
				venueId,
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
				venueId,
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
		const { venueId } = get();
		if (venueId === null) return;

		try {
			await invoke("remove_patched_fixture", { venueId, id });
			set((state) => ({
				selectedPatchedId:
					state.selectedPatchedId === id ? null : state.selectedPatchedId,
			}));
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to remove patched fixture:", error);
		}
	},

	updatePatchedFixtureLabel: async (id, label) => {
		const { venueId } = get();
		if (venueId === null) return;

		const nextLabel = label.trim();
		if (!nextLabel) return;
		const current = get().patchedFixtures;
		const idx = current.findIndex((f) => f.id === id);
		if (idx === -1) return;

		const optimistic = [...current];
		optimistic[idx] = { ...optimistic[idx], label: nextLabel };
		set({ patchedFixtures: optimistic, selectedPatchedId: id });

		try {
			await invoke("rename_patched_fixture", { venueId, id, label: nextLabel });
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to rename patched fixture:", error);
			await get().fetchPatchedFixtures();
		}
	},
}));
