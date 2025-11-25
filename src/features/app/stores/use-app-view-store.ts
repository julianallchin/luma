import { create } from "zustand";

import type { AppView } from "../types/view-types";

export type ProjectInfo = {
	path: string;
	name: string;
};

type AppViewState = {
	view: AppView;
	setView: (view: AppView) => void;
	goBack: () => void;
	previousView: AppView | null;
	currentProject: ProjectInfo | null;
	setProject: (project: ProjectInfo | null) => void;
};

export const useAppViewStore = create<AppViewState>((set, get) => ({
	view: { type: "welcome" },
	previousView: null,
	setView: (view) => {
		const current = get().view;
		set({ previousView: current, view });
	},
	goBack: () => {
		const prev = get().previousView;
		if (prev) {
			set({ view: prev, previousView: null });
		} else {
			set({ view: { type: "welcome" } });
		}
	},
	currentProject: null,
	setProject: (project) => set({ currentProject: project }),
}));
