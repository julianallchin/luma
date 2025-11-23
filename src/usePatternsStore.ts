import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { PatternSummary } from "@/bindings/schema";

type PatternsState = {
	patterns: PatternSummary[];
	loading: boolean;
	error: string | null;
	refresh: () => Promise<void>;
};

export const usePatternsStore = create<PatternsState>((set) => ({
	patterns: [],
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
}));

