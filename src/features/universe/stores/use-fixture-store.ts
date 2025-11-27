import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import type { FixtureEntry, FixtureDefinition, PatchedFixture } from '@/bindings/fixtures';

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
    
    // Actions
    setSearchQuery: (query: string) => void;
    search: (query: string, reset?: boolean) => Promise<void>;
    loadMore: () => Promise<void>;
    selectFixture: (entry: FixtureEntry) => Promise<void>;
    initialize: () => Promise<void>;
    
    // Patch Actions
    fetchPatchedFixtures: () => Promise<void>;
    patchFixture: (
        universe: number, 
        address: number, 
        modeName: string, 
        numChannels: number
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

    setSearchQuery: (query) => set({ searchQuery: query }),

    initialize: async () => {
        try {
            await invoke('initialize_fixtures');
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
             set({ searchResults: [], pageOffset: 0, hasMore: true, isSearching: true });
        } else {
             set({ isSearching: true });
        }

        try {
            const results = await invoke<FixtureEntry[]>('search_fixtures', { 
                query, 
                offset: currentOffset, 
                limit: LIMIT 
            });
            
            set((state) => ({ 
                searchResults: reset ? results : [...state.searchResults, ...results], 
                isSearching: false,
                pageOffset: currentOffset + results.length,
                hasMore: results.length === LIMIT
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
        set({ selectedEntry: entry, selectedDefinition: null, isLoadingDefinition: true });
        try {
            const def = await invoke<FixtureDefinition>('get_fixture_definition', { path: entry.path });
            set({ selectedDefinition: def, isLoadingDefinition: false });
        } catch (error) {
            console.error("Failed to load definition:", error);
            set({ isLoadingDefinition: false });
        }
    },

    fetchPatchedFixtures: async () => {
        try {
            const fixtures = await invoke<PatchedFixture[]>('get_patched_fixtures');
            set({ patchedFixtures: fixtures });
        } catch (error) {
            console.error("Failed to fetch patched fixtures:", error);
        }
    },

    patchFixture: async (universe, address, modeName, numChannels) => {
        const { selectedEntry, selectedDefinition } = get();
        if (!selectedEntry || !selectedDefinition) return;

        try {
            await invoke('patch_fixture', {
                universe,
                address,
                numChannels,
                manufacturer: selectedEntry.manufacturer,
                model: selectedEntry.model,
                modeName,
                fixturePath: selectedEntry.path,
                label: null
            });
            await get().fetchPatchedFixtures();
        } catch (error) {
            console.error("Failed to patch fixture:", error);
        }
    },

    removePatchedFixture: async (id) => {
        try {
            await invoke('remove_patched_fixture', { id });
            await get().fetchPatchedFixtures();
        } catch (error) {
             console.error("Failed to remove patched fixture:", error);
        }
    }
}));
