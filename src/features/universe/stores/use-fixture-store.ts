import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import type { FixtureEntry, FixtureDefinition } from '@/bindings/fixtures';

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
    
    // Actions
    setSearchQuery: (query: string) => void;
    search: (query: string, reset?: boolean) => Promise<void>;
    loadMore: () => Promise<void>;
    selectFixture: (entry: FixtureEntry) => Promise<void>;
    initialize: () => Promise<void>;
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

    setSearchQuery: (query) => set({ searchQuery: query }),

    initialize: async () => {
        try {
            await invoke('initialize_fixtures');
            // Initial empty search to fill list
            get().search("", true);
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
    }
}));
