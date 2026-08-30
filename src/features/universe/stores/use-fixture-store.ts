import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
	FixtureDefinition,
	FixtureEntry,
	PatchedFixture,
} from "@/bindings/fixtures";
import type { PatchAddress } from "@/bindings/patch";
import { useGroupStore } from "./use-group-store";

interface FixtureState {
	// Venue context
	venueId: string | null;

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
	previewFixtureIds: string[];
	definitionsCache: Map<string, FixtureDefinition>;

	// Multi-selection
	selectedPatchedIds: Set<string>;
	lastSelectedPatchedId: string | null;
	/// One-shot identify-blink targets ("fid" or "fid:head") for the next
	/// selection change — set by sources that know head granularity (group
	/// tree); consumed and cleared by the universe designer's blink watcher.
	blinkOverride: string[] | null;

	// Ungrouped fixtures
	ungroupedFixtures: PatchedFixture[];

	// Pointer-based drag (for Linux compatibility)
	pendingDrag: { modeName: string; numChannels: number } | null;

	// Actions
	setVenueId: (venueId: string | null) => void;
	setSearchQuery: (query: string) => void;
	search: (query: string, reset?: boolean) => Promise<void>;
	loadMore: () => Promise<void>;
	selectFixture: (entry: FixtureEntry) => Promise<void>;
	initialize: (venueId?: string) => Promise<void>;
	getDefinition: (path: string) => Promise<FixtureDefinition | null>;

	// Patch Actions
	fetchPatchedFixtures: () => Promise<void>;
	setPreviewFixtureIds: (ids: string[]) => void;
	clearPreviewFixtureIds: () => void;
	/// Ask the backend where the next `count` fixtures of `channels` channels
	/// each would go. The one allocator lives in Rust; there is no first-fit
	/// loop on this side of the wire.
	nextAddresses: (channels: number, count: number) => Promise<PatchAddress[]>;
	/// Returns `null` when the fixture moved, or the backend's refusal — the
	/// same contract as `patchFixture`, because it is the same door: a
	/// collision or a footprint past 512 leaves the row untouched and names
	/// what is in the way.
	setFixtureAddress: (
		id: string,
		universe: number,
		address: number,
	) => Promise<string | null>;
	/// Returns `null` when the fixture was patched, or the backend's refusal —
	/// a collision, a footprint past 512 — for the caller to show. The message
	/// names the fixture in the way, so it is worth reading rather than
	/// summarising.
	patchFixture: (
		universe: number,
		address: number,
		modeName: string,
		numChannels: number,
	) => Promise<string | null>;
	removePatchedFixture: (id: string) => Promise<void>;
	duplicatePatchedFixture: (id: string) => Promise<void>;
	updatePatchedFixtureLabel: (id: string, label: string) => Promise<void>;

	// Multi-selection actions
	selectFixtureById: (id: string, opts?: { shift?: boolean }) => void;
	selectFixturesByIds: (
		ids: string[],
		primaryId?: string | null,
		blinkTargets?: string[],
	) => void;
	clearSelection: () => void;
	consumeBlinkOverride: () => string[] | null;
	isFixtureSelected: (id: string) => boolean;
	duplicateSelectedFixtures: () => Promise<void>;
	removeSelectedFixtures: () => Promise<void>;

	// Backward compat
	selectedPatchedId: string | null;
	setSelectedPatchedId: (id: string | null) => void;

	// Ungrouped fixtures actions
	fetchUngroupedFixtures: () => Promise<void>;

	// Pointer-based drag actions
	startPendingDrag: (modeName: string, numChannels: number) => void;
	clearPendingDrag: () => void;
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
	previewFixtureIds: [],
	definitionsCache: new Map(),
	ungroupedFixtures: [],
	pendingDrag: null,

	// Multi-selection state
	selectedPatchedIds: new Set<string>(),
	lastSelectedPatchedId: null,
	blinkOverride: null,

	// Backward compat (unused, kept for type satisfaction)
	selectedPatchedId: null,

	setVenueId: (venueId) => set({ venueId }),
	setSearchQuery: (query) => set({ searchQuery: query }),

	initialize: async (venueId?: string) => {
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
				searchQuery: query,
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
			set((state) => {
				// Prune selection to only include IDs that still exist
				const validIds = new Set(fixtures.map((f) => f.id));
				const nextSelected = new Set<string>();
				for (const id of state.selectedPatchedIds) {
					if (validIds.has(id)) nextSelected.add(id);
				}
				const nextLast =
					state.lastSelectedPatchedId &&
					validIds.has(state.lastSelectedPatchedId)
						? state.lastSelectedPatchedId
						: null;
				return {
					patchedFixtures: fixtures,
					selectedPatchedIds: nextSelected,
					lastSelectedPatchedId: nextLast,
				};
			});
			// Also refresh ungrouped fixtures
			get().fetchUngroupedFixtures();
		} catch (error) {
			console.error("Failed to fetch patched fixtures:", error);
		}
	},

	// Backward compat: setSelectedPatchedId(id) → selectFixtureById
	setSelectedPatchedId: (id) => {
		if (id === null) {
			get().clearSelection();
		} else {
			get().selectFixtureById(id);
		}
	},

	setPreviewFixtureIds: (ids) => set({ previewFixtureIds: ids }),
	clearPreviewFixtureIds: () => set({ previewFixtureIds: [] }),

	// --- Multi-selection actions ---

	selectFixtureById: (id, opts) => {
		set((state) => {
			if (opts?.shift) {
				// Toggle in set
				const next = new Set(state.selectedPatchedIds);
				if (next.has(id)) {
					next.delete(id);
					// If we removed the primary, pick another or null
					const nextLast =
						state.lastSelectedPatchedId === id
							? (next.values().next().value ?? null)
							: state.lastSelectedPatchedId;
					return {
						selectedPatchedIds: next,
						lastSelectedPatchedId: nextLast,
					};
				}
				next.add(id);
				return { selectedPatchedIds: next, lastSelectedPatchedId: id };
			}
			// No shift: clear and select one
			return {
				selectedPatchedIds: new Set([id]),
				lastSelectedPatchedId: id,
			};
		});
	},

	selectFixturesByIds: (ids, primaryId, blinkTargets) => {
		const fallback = ids.length > 0 ? ids[ids.length - 1] : null;
		const primary =
			primaryId !== undefined && primaryId !== null && ids.includes(primaryId)
				? primaryId
				: fallback;
		set({
			selectedPatchedIds: new Set(ids),
			lastSelectedPatchedId: primary,
			blinkOverride: blinkTargets ?? null,
		});
	},

	clearSelection: () => {
		set({
			selectedPatchedIds: new Set<string>(),
			lastSelectedPatchedId: null,
			blinkOverride: null,
		});
	},

	consumeBlinkOverride: () => {
		const targets = get().blinkOverride;
		if (targets !== null) set({ blinkOverride: null });
		return targets;
	},

	isFixtureSelected: (id) => {
		return get().selectedPatchedIds.has(id);
	},

	nextAddresses: async (channels, count) => {
		const { venueId } = get();
		if (venueId === null || count <= 0) return [];
		try {
			return await invoke<PatchAddress[]>("next_addresses", {
				venueId,
				run: null,
				channels,
				count,
			});
		} catch (error) {
			console.error("Failed to allocate addresses:", error);
			return [];
		}
	},

	setFixtureAddress: async (id, universe, address) => {
		const { venueId } = get();
		if (venueId === null) return "No venue is open.";
		let refusal: string | null = null;
		try {
			await invoke("set_fixture_address", { venueId, id, universe, address });
		} catch (error) {
			// A refusal leaves the database untouched, so the reload below is
			// what restores the real value in the table; the message names the
			// fixture in the way and belongs on screen, not in the console.
			refusal = String(error);
		}
		await get().fetchPatchedFixtures();
		return refusal;
	},

	patchFixture: async (universe, address, modeName, numChannels) => {
		const { selectedEntry, selectedDefinition, venueId } = get();
		if (!selectedEntry || !selectedDefinition || venueId === null) {
			return "No fixture selected.";
		}

		try {
			console.debug("[useFixtureStore] patchFixture invoke", {
				venueId,
				universe,
				address,
				numChannels,
				manufacturer: selectedEntry.manufacturer,
				model: selectedEntry.model,
				modeName,
				fixturePath: selectedEntry.path,
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
				// The backend mints `<model> <n>`: one naming rule, and it is
				// not on this side of the wire.
				label: null,
			});
			console.debug("[useFixtureStore] patchFixture success");
			await get().fetchPatchedFixtures();
			return null;
		} catch (error) {
			return String(error);
		}
	},

	removePatchedFixture: async (id) => {
		const { venueId } = get();
		if (venueId === null) return;

		try {
			await invoke("remove_patched_fixture", { venueId, id });
			set((state) => {
				const next = new Set(state.selectedPatchedIds);
				next.delete(id);
				const nextLast =
					state.lastSelectedPatchedId === id
						? (next.values().next().value ?? null)
						: state.lastSelectedPatchedId;
				return {
					selectedPatchedIds: next,
					lastSelectedPatchedId: nextLast,
				};
			});
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to remove patched fixture:", error);
		}
	},

	removeSelectedFixtures: async () => {
		const { venueId, selectedPatchedIds } = get();
		if (venueId === null || selectedPatchedIds.size === 0) return;

		try {
			await Promise.all(
				[...selectedPatchedIds].map((id) =>
					invoke("remove_patched_fixture", { venueId, id }),
				),
			);
			set({
				selectedPatchedIds: new Set<string>(),
				lastSelectedPatchedId: null,
			});
			await get().fetchPatchedFixtures();
		} catch (error) {
			console.error("Failed to remove selected fixtures:", error);
			await get().fetchPatchedFixtures();
		}
	},

	duplicatePatchedFixture: async (id) => {
		const { venueId, patchedFixtures } = get();
		if (venueId === null) return;

		const fixture = patchedFixtures.find((f) => f.id === id);
		if (!fixture) return;
		const numChannels = Number(fixture.numChannels);

		const [slot] = await get().nextAddresses(numChannels, 1);
		if (!slot) {
			console.error("No available address for duplicate fixture");
			return;
		}

		try {
			const newFixture = await invoke<PatchedFixture>("patch_fixture", {
				venueId,
				universe: slot.universe,
				address: slot.address,
				numChannels,
				manufacturer: fixture.manufacturer,
				model: fixture.model,
				modeName: fixture.modeName,
				fixturePath: fixture.fixturePath,
				label: null,
			});

			await get().fetchPatchedFixtures();
			set({
				selectedPatchedIds: new Set([newFixture.id]),
				lastSelectedPatchedId: newFixture.id,
			});
		} catch (error) {
			console.error("Failed to duplicate fixture:", error);
		}
	},

	duplicateSelectedFixtures: async () => {
		const { venueId, selectedPatchedIds, patchedFixtures } = get();
		if (venueId === null || selectedPatchedIds.size === 0) return;

		const toDuplicate = patchedFixtures.filter((f) =>
			selectedPatchedIds.has(f.id),
		);
		if (toDuplicate.length === 0) return;

		// One allocation call per *width*, not per fixture: `next_addresses`
		// answers for `count` fixtures of one channel count at a time, and the
		// slots it returns are already disjoint. Widths are asked for in turn
		// because each group's rows are written before the next group is
		// placed, so the backend always sees the real occupancy.
		const byWidth = new Map<number, PatchedFixture[]>();
		for (const fixture of toDuplicate) {
			const width = Number(fixture.numChannels);
			byWidth.set(width, [...(byWidth.get(width) ?? []), fixture]);
		}

		const newIds: string[] = [];
		try {
			for (const [numChannels, group] of byWidth) {
				const slots = await get().nextAddresses(numChannels, group.length);
				if (slots.length < group.length) {
					console.error("No available address for duplicate fixture");
				}
				for (const [index, fixture] of group.entries()) {
					const slot = slots[index];
					if (!slot) continue;

					const newFixture = await invoke<PatchedFixture>("patch_fixture", {
						venueId,
						universe: slot.universe,
						address: slot.address,
						numChannels,
						manufacturer: fixture.manufacturer,
						model: fixture.model,
						modeName: fixture.modeName,
						fixturePath: fixture.fixturePath,
						label: null,
					});
					newIds.push(newFixture.id);
				}
			}

			await get().fetchPatchedFixtures();
			if (newIds.length > 0) {
				set({
					selectedPatchedIds: new Set(newIds),
					lastSelectedPatchedId: newIds[newIds.length - 1],
				});
			}
		} catch (error) {
			console.error("Failed to duplicate selected fixtures:", error);
			await get().fetchPatchedFixtures();
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
		set({ patchedFixtures: optimistic });
		get().selectFixtureById(id);

		try {
			await invoke("rename_patched_fixture", { venueId, id, label: nextLabel });
			await get().fetchPatchedFixtures();
			// Refresh group tree so fixture labels update there too
			await useGroupStore.getState().fetchGroups(venueId);
		} catch (error) {
			console.error("Failed to rename patched fixture:", error);
			await get().fetchPatchedFixtures();
		}
	},

	fetchUngroupedFixtures: async () => {
		const { venueId } = get();
		if (venueId === null) return;
		try {
			const ungrouped = await invoke<PatchedFixture[]>(
				"get_ungrouped_fixtures",
				{ venueId },
			);
			set({ ungroupedFixtures: ungrouped });
		} catch (error) {
			console.error("Failed to fetch ungrouped fixtures:", error);
		}
	},

	startPendingDrag: (modeName, numChannels) => {
		set({ pendingDrag: { modeName, numChannels } });
	},

	clearPendingDrag: () => {
		set({ pendingDrag: null });
	},
}));
