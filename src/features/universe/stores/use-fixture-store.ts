import { invoke } from "@tauri-apps/api/core";
import { Euler, Quaternion, Vector3 } from "three";
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
	previewFixtureIds: string[];
	definitionsCache: Map<string, FixtureDefinition>;

	// Multi-selection
	selectedPatchedIds: Set<string>;
	lastSelectedPatchedId: string | null;

	// Pointer-based drag (for Linux compatibility)
	pendingDrag: { modeName: string; numChannels: number } | null;

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
	duplicatePatchedFixture: (id: string) => Promise<void>;
	updatePatchedFixtureLabel: (id: string, label: string) => Promise<void>;

	// Multi-selection actions
	selectFixtureById: (id: string, opts?: { shift?: boolean }) => void;
	selectFixturesByIds: (ids: string[]) => void;
	clearSelection: () => void;
	isFixtureSelected: (id: string) => boolean;
	duplicateSelectedFixtures: () => Promise<void>;
	removeSelectedFixtures: () => Promise<void>;
	moveSelectedFixturesSpatialDelta: (
		deltaPos: { x: number; y: number; z: number },
		deltaRot: { x: number; y: number; z: number },
	) => Promise<void>;
	rotateSelectedAroundCenter: (deltaRot: {
		x: number;
		y: number;
		z: number;
	}) => Promise<void>;

	// Backward compat
	selectedPatchedId: string | null;
	setSelectedPatchedId: (id: string | null) => void;

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
	pendingDrag: null,

	// Multi-selection state
	selectedPatchedIds: new Set<string>(),
	lastSelectedPatchedId: null,

	// Backward compat (unused, kept for type satisfaction)
	selectedPatchedId: null,

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

	selectFixturesByIds: (ids) => {
		set({
			selectedPatchedIds: new Set(ids),
			lastSelectedPatchedId: ids.length > 0 ? ids[ids.length - 1] : null,
		});
	},

	clearSelection: () => {
		set({
			selectedPatchedIds: new Set<string>(),
			lastSelectedPatchedId: null,
		});
	},

	isFixtureSelected: (id) => {
		return get().selectedPatchedIds.has(id);
	},

	// --- Spatial ---

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

	moveSelectedFixturesSpatialDelta: async (deltaPos, deltaRot) => {
		const {
			venueId,
			selectedPatchedIds,
			lastSelectedPatchedId,
			patchedFixtures,
		} = get();
		if (venueId === null) return;

		// Skip the primary fixture — it's already moved by TransformControls + moveFixtureSpatial
		const toMove = patchedFixtures.filter(
			(f) => selectedPatchedIds.has(f.id) && f.id !== lastSelectedPatchedId,
		);
		if (toMove.length === 0) return;

		// Optimistic update all at once
		const optimistic = [...patchedFixtures];
		for (const fixture of toMove) {
			const idx = optimistic.findIndex((f) => f.id === fixture.id);
			if (idx === -1) continue;
			optimistic[idx] = {
				...optimistic[idx],
				posX: fixture.posX + deltaPos.x,
				posY: fixture.posY + deltaPos.y,
				posZ: fixture.posZ + deltaPos.z,
				rotX: fixture.rotX + deltaRot.x,
				rotY: fixture.rotY + deltaRot.y,
				rotZ: fixture.rotZ + deltaRot.z,
			};
		}
		set({ patchedFixtures: optimistic });

		// Persist each
		try {
			await Promise.all(
				toMove.map((f) =>
					invoke("move_patched_fixture_spatial", {
						venueId,
						id: f.id,
						posX: f.posX + deltaPos.x,
						posY: f.posY + deltaPos.y,
						posZ: f.posZ + deltaPos.z,
						rotX: f.rotX + deltaRot.x,
						rotY: f.rotY + deltaRot.y,
						rotZ: f.rotZ + deltaRot.z,
					}),
				),
			);
		} catch (error) {
			console.error("Failed to move fixtures spatially:", error);
			await get().fetchPatchedFixtures();
		}
	},

	rotateSelectedAroundCenter: async (deltaRot) => {
		const {
			venueId,
			selectedPatchedIds,
			lastSelectedPatchedId,
			patchedFixtures,
		} = get();
		if (venueId === null || selectedPatchedIds.size < 2) return;

		const selected = patchedFixtures.filter((f) =>
			selectedPatchedIds.has(f.id),
		);
		if (selected.length < 2) return;

		// Compute centroid of all selected fixtures (data coords, Z-up)
		const centroid = { x: 0, y: 0, z: 0 };
		for (const f of selected) {
			centroid.x += f.posX;
			centroid.y += f.posY;
			centroid.z += f.posZ;
		}
		centroid.x /= selected.length;
		centroid.y /= selected.length;
		centroid.z /= selected.length;

		// Build quaternion from delta rotation (data coords)
		const q = new Quaternion().setFromEuler(
			new Euler(deltaRot.x, deltaRot.y, deltaRot.z),
		);

		// Skip the primary — it's already moved by TransformControls + moveFixtureSpatial
		const toRotate = selected.filter((f) => f.id !== lastSelectedPatchedId);

		// Compute new positions/rotations
		const updates: Array<{
			id: string;
			posX: number;
			posY: number;
			posZ: number;
			rotX: number;
			rotY: number;
			rotZ: number;
		}> = [];
		for (const f of toRotate) {
			const offset = new Vector3(
				f.posX - centroid.x,
				f.posY - centroid.y,
				f.posZ - centroid.z,
			);
			offset.applyQuaternion(q);
			updates.push({
				id: f.id,
				posX: centroid.x + offset.x,
				posY: centroid.y + offset.y,
				posZ: centroid.z + offset.z,
				rotX: f.rotX - deltaRot.x,
				rotY: f.rotY - deltaRot.y,
				rotZ: f.rotZ - deltaRot.z,
			});
		}

		// Optimistic update
		const optimistic = [...patchedFixtures];
		for (const u of updates) {
			const idx = optimistic.findIndex((f) => f.id === u.id);
			if (idx === -1) continue;
			optimistic[idx] = { ...optimistic[idx], ...u };
		}
		set({ patchedFixtures: optimistic });

		// Persist
		try {
			await Promise.all(
				updates.map((u) =>
					invoke("move_patched_fixture_spatial", {
						venueId,
						id: u.id,
						posX: u.posX,
						posY: u.posY,
						posZ: u.posZ,
						rotX: u.rotX,
						rotY: u.rotY,
						rotZ: u.rotZ,
					}),
				),
			);
		} catch (error) {
			console.error("Failed to rotate fixtures around center:", error);
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
			set({ patchedFixtures: optimistic });
			get().selectFixtureById(id);

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

		// Find the first available address that can fit the fixture
		const findNextAvailableAddress = (): number | null => {
			// Build a sorted list of occupied ranges
			const occupiedRanges = patchedFixtures
				.map((f) => ({
					start: Number(f.address),
					end: Number(f.address) + Number(f.numChannels) - 1,
				}))
				.sort((a, b) => a.start - b.start);

			// Try to find a gap starting from address 1
			let candidate = 1;
			for (const range of occupiedRanges) {
				if (candidate + numChannels - 1 < range.start) {
					// Found a gap before this range
					return candidate;
				}
				// Move candidate past this range
				candidate = Math.max(candidate, range.end + 1);
			}

			// Check if there's space after all fixtures
			if (candidate + numChannels - 1 <= 512) {
				return candidate;
			}

			return null;
		};

		const address = findNextAvailableAddress();
		if (address === null) {
			console.error("No available address for duplicate fixture");
			return;
		}

		// Generate label for the duplicate
		const existingCount = patchedFixtures.filter(
			(f) => f.model === fixture.model,
		).length;
		const label = `${fixture.model} (${existingCount + 1})`;

		try {
			const newFixture = await invoke<PatchedFixture>("patch_fixture", {
				venueId,
				universe: Number(fixture.universe),
				address,
				numChannels,
				manufacturer: fixture.manufacturer,
				model: fixture.model,
				modeName: fixture.modeName,
				fixturePath: fixture.fixturePath,
				label,
			});

			// Copy spatial position from original fixture
			await invoke("move_patched_fixture_spatial", {
				venueId,
				id: newFixture.id,
				posX: fixture.posX,
				posY: fixture.posY,
				posZ: fixture.posZ,
				rotX: fixture.rotX,
				rotY: fixture.rotY,
				rotZ: fixture.rotZ,
			});

			await get().fetchPatchedFixtures();
			// Select the new fixture
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

		// Track cumulative occupancy as each new fixture is allocated
		const allOccupied = patchedFixtures
			.map((f) => ({
				start: Number(f.address),
				end: Number(f.address) + Number(f.numChannels) - 1,
			}))
			.sort((a, b) => a.start - b.start);

		const newIds: string[] = [];

		try {
			for (const fixture of toDuplicate) {
				const numChannels = Number(fixture.numChannels);

				// Find next available address considering cumulative occupancy
				const sorted = [...allOccupied].sort((a, b) => a.start - b.start);
				let address: number | null = null;
				let candidate = 1;
				for (const range of sorted) {
					if (candidate + numChannels - 1 < range.start) {
						address = candidate;
						break;
					}
					candidate = Math.max(candidate, range.end + 1);
				}
				if (address === null && candidate + numChannels - 1 <= 512) {
					address = candidate;
				}
				if (address === null) {
					console.error("No available address for duplicate fixture");
					continue;
				}

				// Add to cumulative occupancy
				allOccupied.push({
					start: address,
					end: address + numChannels - 1,
				});

				const existingCount =
					patchedFixtures.filter((f) => f.model === fixture.model).length +
					newIds.length;
				const label = `${fixture.model} (${existingCount + 1})`;

				const newFixture = await invoke<PatchedFixture>("patch_fixture", {
					venueId,
					universe: Number(fixture.universe),
					address,
					numChannels,
					manufacturer: fixture.manufacturer,
					model: fixture.model,
					modeName: fixture.modeName,
					fixturePath: fixture.fixturePath,
					label,
				});

				// Copy spatial position with small X offset so duplicates are visible
				await invoke("move_patched_fixture_spatial", {
					venueId,
					id: newFixture.id,
					posX: fixture.posX + 0.3,
					posY: fixture.posY,
					posZ: fixture.posZ,
					rotX: fixture.rotX,
					rotY: fixture.rotY,
					rotZ: fixture.rotZ,
				});

				newIds.push(newFixture.id);
			}

			await get().fetchPatchedFixtures();
			// Select only the new fixtures
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
		} catch (error) {
			console.error("Failed to rename patched fixture:", error);
			await get().fetchPatchedFixtures();
		}
	},

	startPendingDrag: (modeName, numChannels) => {
		set({ pendingDrag: { modeName, numChannels } });
	},

	clearPendingDrag: () => {
		set({ pendingDrag: null });
	},
}));
