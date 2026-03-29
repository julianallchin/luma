import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { PatternSummary, SearchPatternRow } from "@/bindings/schema";

export type PatternFilter = "verified" | "mine" | "all";

type PatternsState = {
	patterns: PatternSummary[];
	filter: PatternFilter;
	currentUserId: string | null;
	loading: boolean;
	error: string | null;
	// Remote search state
	searchQuery: string;
	searchResults: SearchPatternRow[];
	searchLoading: boolean;
	refresh: () => Promise<void>;
	setFilter: (filter: PatternFilter) => void;
	setCurrentUserId: (uid: string | null) => void;
	verifyPattern: (id: string, verify: boolean) => Promise<void>;
	forkPattern: (id: string) => Promise<PatternSummary>;
	deletePattern: (id: string) => Promise<void>;
	filteredPatterns: () => PatternSummary[];
	searchRemote: (query: string) => Promise<void>;
	setSearchQuery: (query: string) => void;
};

export const usePatternsStore = create<PatternsState>((set, get) => ({
	patterns: [],
	filter: "verified",
	currentUserId: null,
	loading: false,
	error: null,
	searchQuery: "",
	searchResults: [],
	searchLoading: false,

	refresh: async () => {
		set({ loading: true, error: null });
		try {
			const fresh = await invoke<PatternSummary[]>("list_patterns");
			set({ patterns: fresh, loading: false });
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	setFilter: (filter) => set({ filter }),

	setCurrentUserId: (uid) => set({ currentUserId: uid }),

	verifyPattern: async (id, verify) => {
		await invoke("verify_pattern", { id, verify });
		await get().refresh();
	},

	deletePattern: async (id) => {
		await invoke("delete_pattern", { id });
		await get().refresh();
	},

	forkPattern: async (id) => {
		const forked = await invoke<PatternSummary>("fork_pattern", {
			sourcePatternId: id,
		});
		await get().refresh();
		return forked;
	},

	filteredPatterns: () => {
		const { patterns, filter, currentUserId } = get();
		if (filter === "mine")
			return patterns.filter((p) => p.uid === currentUserId);
		if (filter === "verified") return patterns.filter((p) => p.isVerified);
		// "all" tab uses searchResults, not local patterns
		return [];
	},

	setSearchQuery: (query) => set({ searchQuery: query }),

	searchRemote: async (query) => {
		set({ searchLoading: true, searchQuery: query });
		try {
			const results = await invoke<SearchPatternRow[]>(
				"search_patterns_remote",
				{ query, limit: 50, offset: 0 },
			);
			set({ searchResults: results, searchLoading: false });
		} catch (err) {
			console.error("[patterns] Remote search failed", err);
			set({ searchResults: [], searchLoading: false });
		}
	},
}));
