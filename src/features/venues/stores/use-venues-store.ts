import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { Venue } from "@/bindings/venues";

type VenuesState = {
	venues: Venue[];
	loading: boolean;
	error: string | null;
	selectedVenueId: number | null;
	refresh: () => Promise<void>;
	selectVenue: (id: number | null) => void;
	createVenue: (name: string, description?: string) => Promise<Venue>;
	deleteVenue: (id: number) => Promise<void>;
};

export const useVenuesStore = create<VenuesState>((set, get) => ({
	venues: [],
	loading: false,
	error: null,
	selectedVenueId: null,

	refresh: async () => {
		set({ loading: true, error: null });
		try {
			const fresh = await invoke<Venue[]>("list_venues");
			set({ venues: fresh, loading: false });
		} catch (err) {
			set({
				loading: false,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	},

	selectVenue: (id: number | null) => {
		set({ selectedVenueId: id });
	},

	createVenue: async (name: string, description?: string) => {
		const venue = await invoke<Venue>("create_venue", {
			name,
			description: description || null,
		});
		// Refresh the list after creation
		await get().refresh();
		return venue;
	},

	deleteVenue: async (id: number) => {
		await invoke("delete_venue", { id });
		// Refresh the list after deletion
		await get().refresh();
		// Clear selection if deleted venue was selected
		if (get().selectedVenueId === id) {
			set({ selectedVenueId: null });
		}
	},
}));
