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
	refreshVenueCounts: () => Promise<void>;
	setSearchQuery: (q: string) => void;
};

export const useTracksStore = create<TracksState>((set, get) => ({
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
	refreshVenueCounts: async () => {
		const venueId = useAppViewStore.getState().currentVenue?.id;
		const tracks = get().browserTracks;
		if (!tracks.length) return;

		if (!venueId) {
			// No venue — zero out all venue counts
			set({
				browserTracks: tracks.map((t) => ({
					...t,
					venueAnnotationCount: 0,
				})),
			});
			return;
		}

		try {
			const counts = await invoke<Record<string, number>>(
				"get_venue_annotation_counts",
				{ venueId },
			);
			set({
				browserTracks: tracks.map((t) => ({
					...t,
					venueAnnotationCount: counts[t.id] ?? 0,
				})),
			});
		} catch (err) {
			console.error("Failed to refresh venue counts:", err);
		}
	},
	setSearchQuery: (q: string) => set({ searchQuery: q }),
}));
