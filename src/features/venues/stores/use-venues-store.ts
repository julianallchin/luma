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
	updateVenue: (
		id: string,
		name: string,
		description?: string,
	) => Promise<Venue>;
	deleteVenue: (id: string) => Promise<void>;
	joinVenue: (code: string) => Promise<Venue>;
	leaveVenue: (id: number) => Promise<void>;
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

	updateVenue: async (id: string, name: string, description?: string) => {
		const venue = await invoke<Venue>("update_venue", {
			id,
			name,
			description: description || null,
		});
		await get().refresh();
		return venue;
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

	deleteVenue: async (id: string) => {
		await invoke("delete_venue", { id });
		await get().refresh();
		if (String(get().selectedVenueId) === id) {
			set({ selectedVenueId: null });
		}
	},

	joinVenue: async (code: string) => {
		const venue = await invoke<Venue>("join_venue", { code });
		await get().refresh();
		return venue;
	},

	leaveVenue: async (id: number) => {
		await invoke("leave_venue", { venueId: id });
		await get().refresh();
		if (get().selectedVenueId === id) {
			set({ selectedVenueId: null });
		}
	},
}));
