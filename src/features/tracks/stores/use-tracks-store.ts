import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { TrackBrowserRow, TrackSummary } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";

type TracksState = {
	tracks: TrackSummary[];
	loading: boolean;
	error: string | null;
	refresh: () => Promise<void>;
	browserTracks: TrackBrowserRow[];
	browserLoading: boolean;
	searchQuery: string;
	refreshBrowser: () => Promise<void>;
	setSearchQuery: (q: string) => void;
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
	browserTracks: [],
	browserLoading: false,
	searchQuery: "",
	refreshBrowser: async () => {
		set({ browserLoading: true });
		try {
			const venueId = useAppViewStore.getState().currentVenue?.id ?? null;
			const fresh = await invoke<TrackBrowserRow[]>("list_tracks_enriched", {
				venueId,
			});
			set({ browserTracks: fresh, browserLoading: false });
		} catch (err) {
			console.error("Failed to load enriched tracks:", err);
			set({ browserLoading: false });
		}
	},
	setSearchQuery: (q: string) => set({ searchQuery: q }),
}));
