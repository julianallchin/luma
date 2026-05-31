import { create } from "zustand";

interface ExportState {
	isExporting: boolean;
	sessionId: string | null;
	currentFrame: number;
	totalFrames: number;
	status: string;
	cancelRequested: boolean;
	start: (sessionId: string, totalFrames: number) => void;
	setProgress: (currentFrame: number) => void;
	setStatus: (status: string) => void;
	requestCancel: () => void;
	finish: () => void;
}

export const useExportStore = create<ExportState>((set) => ({
	isExporting: false,
	sessionId: null,
	currentFrame: 0,
	totalFrames: 0,
	status: "",
	cancelRequested: false,
	start: (sessionId, totalFrames) =>
		set({
			isExporting: true,
			sessionId,
			totalFrames,
			currentFrame: 0,
			cancelRequested: false,
			status: "Exporting…",
		}),
	setProgress: (currentFrame) => set({ currentFrame }),
	setStatus: (status) => set({ status }),
	requestCancel: () => set({ cancelRequested: true, status: "Cancelling…" }),
	finish: () =>
		set({
			isExporting: false,
			sessionId: null,
			currentFrame: 0,
			totalFrames: 0,
			status: "",
			cancelRequested: false,
		}),
}));
