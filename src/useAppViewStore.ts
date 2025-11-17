import { create } from "zustand";

import type { AppView } from "./viewTypes";

type AppViewState = {
	view: AppView;
	setView: (view: AppView) => void;
};

export const useAppViewStore = create<AppViewState>((set) => ({
	view: { type: "welcome" },
	setView: (view) => set({ view }),
}));
