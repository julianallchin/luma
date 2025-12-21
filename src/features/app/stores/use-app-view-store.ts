import { create } from "zustand";
import type { Venue } from "@/bindings/venues";

type AppViewState = {
	currentVenue: Venue | null;
	setVenue: (venue: Venue | null) => void;
};

export const useAppViewStore = create<AppViewState>((set) => ({
	currentVenue: null,
	setVenue: (venue) => set({ currentVenue: venue }),
}));
