import { createContext, useContext } from "react";

import type { BeatGrid, TrackSummary } from "@/bindings/schema";
import type {
	TrackScore,
	TrackWaveform,
} from "@/features/track-editor/stores/use-track-editor-store";

export type PatternAnnotationInstance = TrackScore & {
	track: TrackSummary;
	beatGrid: BeatGrid | null;
	waveform: TrackWaveform | null;
};

export type PatternAnnotationContextValue = {
	instances: PatternAnnotationInstance[];
	selectedId: number | null;
	selectInstance: (annotationId: number | null) => void;
	loading: boolean;
};

const PatternAnnotationContext = createContext<PatternAnnotationContextValue>({
	instances: [],
	selectedId: null,
	selectInstance: () => {},
	loading: false,
});

export function usePatternAnnotationContext() {
	return useContext(PatternAnnotationContext);
}

export const PatternAnnotationProvider = PatternAnnotationContext.Provider;
