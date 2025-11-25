import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { TrackSummary } from "@/bindings/schema";

type TracksState = {
	tracks: TrackSummary[];
	loading: boolean;
	error: string | null;
	refresh: () => Promise<void>;
};

export const useTracksStore = create<TracksState>((set) => ({
	tracks: [],
	loading: false,
	error: null,
	refresh: async () => {
		set({ loading: true, error: null });
		try {
			const fresh = await invoke<TrackSummary[]>("list_tracks");
			set({ tracks: fresh, loading: false });
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},
}));
