import { create } from "zustand";

interface UploadProgressState {
	total: number;
	completed: number;
	addToTotal: (n: number) => void;
	tick: () => void;
}

export const useUploadProgressStore = create<UploadProgressState>((set) => ({
	total: 0,
	completed: 0,
	addToTotal: (n) => set((s) => ({ total: s.total + n })),
	tick: () => set((s) => ({ completed: s.completed + 1 })),
}));
