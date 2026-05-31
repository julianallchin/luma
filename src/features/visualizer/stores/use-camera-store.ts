import { create } from "zustand";
import { persist } from "zustand/middleware";

interface CameraState {
	position: [number, number, number];
	target: [number, number, number];
	setCamera: (
		position: [number, number, number],
		target: [number, number, number],
	) => void;
}

export const useCameraStore = create<CameraState>()(
	persist(
		(set) => ({
			position: [0, 1, 3],
			target: [0, 0, 0],
			setCamera: (position, target) => set({ position, target }),
		}),
		{ name: "luma-camera" },
	),
);
