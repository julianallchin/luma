import { create } from "zustand";

export type ProjectInfo = {
	path: string;
	name: string;
};

type AppViewState = {
	currentProject: ProjectInfo | null;
	setProject: (project: ProjectInfo | null) => void;
};

export const useAppViewStore = create<AppViewState>((set) => ({
	currentProject: null,
	setProject: (project) => set({ currentProject: project }),
}));
