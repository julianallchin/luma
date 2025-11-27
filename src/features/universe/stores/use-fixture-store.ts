import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import type { FixtureEntry, FixtureDefinition } from '@/bindings/fixtures';

interface FixtureState {
    // Search
    searchQuery: string;
    searchResults: FixtureEntry[];
    isSearching: boolean;
    
    // Selection
    selectedEntry: FixtureEntry | null;
    selectedDefinition: FixtureDefinition | null;
    isLoadingDefinition: boolean;
    
    // Actions
    setSearchQuery: (query: string) => void;
    search: (query: string) => Promise<void>;
    selectFixture: (entry: FixtureEntry) => Promise<void>;
    initialize: () => Promise<void>;
}

export const useFixtureStore = create<FixtureState>((set, get) => ({
    searchQuery: "",
    searchResults: [],
    isSearching: false,
    selectedEntry: null,
    selectedDefinition: null,
    isLoadingDefinition: false,

    setSearchQuery: (query) => set({ searchQuery: query }),

    initialize: async () => {
        try {
            await invoke('initialize_fixtures');
            // Initial empty search to fill list
            get().search("");
        } catch (error) {
            console.error("Failed to initialize fixtures:", error);
        }
    },

    search: async (query) => {
        set({ isSearching: true });
        try {
            const results = await invoke<FixtureEntry[]>('search_fixtures', { query });
            set({ searchResults: results, isSearching: false });
        } catch (error) {
            console.error("Search failed:", error);
            set({ isSearching: false });
        }
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
    }
}));
