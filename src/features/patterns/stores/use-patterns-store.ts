import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { PatternSummary } from "@/bindings/schema";

export type PatternFilter = "all" | "mine" | "community";

type PatternsState = {
	patterns: PatternSummary[];
	filter: PatternFilter;
	currentUserId: string | null;
	loading: boolean;
	error: string | null;
	refresh: () => Promise<void>;
	setFilter: (filter: PatternFilter) => void;
	setCurrentUserId: (uid: string | null) => void;
	pullOwn: () => Promise<void>;
	pullCommunity: () => Promise<void>;
	publishPattern: (id: number, publish: boolean) => Promise<void>;
	forkPattern: (id: number) => Promise<PatternSummary>;
	deletePattern: (id: number) => Promise<void>;
	filteredPatterns: () => PatternSummary[];
};

export const usePatternsStore = create<PatternsState>((set, get) => ({
	patterns: [],
	filter: "all",
	currentUserId: null,
	loading: false,
	error: null,

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

	pullOwn: async () => {
		try {
			await invoke("pull_own_patterns");
			await get().refresh();
		} catch (err) {
			console.error("[patterns] Failed to pull own patterns", err);
		}
	},

	pullCommunity: async () => {
		try {
			await invoke("pull_community_patterns");
			await get().refresh();
		} catch (err) {
			console.error("[patterns] Failed to pull community patterns", err);
		}
	},

	publishPattern: async (id, publish) => {
		await invoke("publish_pattern", { id, publish });
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
		if (filter === "all") return patterns;
		if (filter === "mine")
			return patterns.filter((p) => p.uid === currentUserId);
		// community
		return patterns.filter((p) => p.uid !== currentUserId);
	},
}));
