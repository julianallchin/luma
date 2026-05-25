import { create } from "zustand";

type SettingsDialogState = {
	open: boolean;
	setOpen: (open: boolean) => void;
	toggle: () => void;
};

export const useSettingsDialogStore = create<SettingsDialogState>((set) => ({
	open: false,
	setOpen: (open) => set({ open }),
	toggle: () => set((s) => ({ open: !s.open })),
}));
