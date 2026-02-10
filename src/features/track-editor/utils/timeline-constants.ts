// Timeline rendering constants
export const MIN_ZOOM = 25;
export const MAX_ZOOM = 500;
export const ZOOM_SENSITIVITY = 0.002;
export const HEADER_HEIGHT = 32;
export const WAVEFORM_HEIGHT = 80;
export const TRACK_HEIGHT = 80;
export const ANNOTATION_LANE_HEIGHT = 80; // Taller lane for patterns
export const MINIMAP_HEIGHT = 48;
export const ALWAYS_DRAW = false; // only draw when needed; rAF loop keeps cadence
export const MIN_ANNOTATION_DURATION = 0.05; // seconds — minimum duration for splits

// Vertical zoom constants
export const MIN_ZOOM_Y = 0.5;
export const MAX_ZOOM_Y = 1.5;
export const ZOOM_Y_SENSITIVITY = 0.003;

export type TimelineLayout = {
	headerHeight: number;
	waveformHeight: number;
	trackHeight: number;
	annotationLaneHeight: number;
	minimapHeight: number;
	trackStartY: number;
};

export function computeLayout(zoomY: number): TimelineLayout {
	const headerHeight = HEADER_HEIGHT;
	const waveformHeight = Math.round(WAVEFORM_HEIGHT * zoomY);
	const trackHeight = Math.round(TRACK_HEIGHT * zoomY);
	const annotationLaneHeight = Math.round(ANNOTATION_LANE_HEIGHT * zoomY);
	const minimapHeight = MINIMAP_HEIGHT;
	return {
		headerHeight,
		waveformHeight,
		trackHeight,
		annotationLaneHeight,
		minimapHeight,
		trackStartY: headerHeight + waveformHeight,
	};
}

export const patternColors = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

export function getPatternColor(patternId: number): string {
	return patternColors[patternId % patternColors.length];
}
