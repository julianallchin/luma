import { create } from "zustand";
import type { BeatGrid, Signal } from "@/bindings/schema";

export type MelSpecData = {
	width: number;
	height: number;
	data: number[];
	beatGrid: BeatGrid | null;
};

/**
 * Live view-node results (view_signal / view_uv / view_events / mel_spec).
 *
 * Deliberately separate from ReactFlow node state: results stream at ~20 Hz
 * during param drags, and routing them through setNodes re-rendered the whole
 * editor (nodes array identity, edge effects, connection validation) on every
 * result. Here each view node subscribes to its own slice, so a result commit
 * re-renders only the nodes whose data actually changed.
 */
type ViewDataStore = {
	views: Record<string, Signal>;
	melSpecs: Record<string, MelSpecData>;
	colorViews: Record<string, string>;
	setResults: (
		views: Record<string, Signal>,
		melSpecs: Record<string, MelSpecData>,
		colorViews: Record<string, string>,
	) => void;
	reset: () => void;
};

export const useViewDataStore = create<ViewDataStore>((set) => ({
	views: {},
	melSpecs: {},
	colorViews: {},
	setResults: (views, melSpecs, colorViews) =>
		set((state) => ({
			views,
			// Param-only runs skip mel-spec computation (audio can't change);
			// merge so existing spectrograms survive those results.
			melSpecs: Object.keys(melSpecs).length
				? { ...state.melSpecs, ...melSpecs }
				: state.melSpecs,
			colorViews,
		})),
	reset: () => set({ views: {}, melSpecs: {}, colorViews: {} }),
}));

export function resetViewDataStore(): void {
	useViewDataStore.getState().reset();
}
