import { create } from "zustand";

import type { AppView } from "./viewTypes";

export type ProjectInfo = {
	path: string;
	name: string;
};

type AppViewState = {
	view: AppView;
	setView: (view: AppView) => void;
	currentProject: ProjectInfo | null;
	setProject: (project: ProjectInfo | null) => void;
};

export const useAppViewStore = create<AppViewState>((set) => ({
	view: { type: "welcome" },
	setView: (view) => set({ view }),
	currentProject: null,
	setProject: (project) => set({ currentProject: project }),
}));
