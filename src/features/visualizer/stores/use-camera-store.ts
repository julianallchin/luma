import { create } from "zustand";

interface CameraState {
	position: [number, number, number];
	target: [number, number, number];
	setCamera: (
		position: [number, number, number],
		target: [number, number, number],
	) => void;
}

export const useCameraStore = create<CameraState>((set) => ({
	position: [0, 1, 3],
	target: [0, 0, 0],
	setCamera: (position, target) => set({ position, target }),
}));
