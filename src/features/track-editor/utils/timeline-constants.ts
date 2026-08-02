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
	/** Start of the scrollable track area (includes empty top lane) */
	trackAreaY: number;
	/** Start of actual data rows (after empty top lane) */
	trackStartY: number;
};

export function computeLayout(zoomY: number): TimelineLayout {
	const headerHeight = HEADER_HEIGHT;
	// The waveform is a fixed navigation/scrubbing surface. Vertical zoom only
	// changes the annotation workspace beneath it.
	const waveformHeight = WAVEFORM_HEIGHT;
	const trackHeight = Math.round(TRACK_HEIGHT * zoomY);
	const annotationLaneHeight = Math.round(ANNOTATION_LANE_HEIGHT * zoomY);
	const minimapHeight = MINIMAP_HEIGHT;
	const trackAreaY = headerHeight + waveformHeight;
	return {
		headerHeight,
		waveformHeight,
		trackHeight,
		annotationLaneHeight,
		minimapHeight,
		trackAreaY,
		trackStartY: trackAreaY + trackHeight,
	};
}

export function computeBottomAnchoredLayout(
	zoomY: number,
	layerCount: number,
	viewportHeight: number,
): { layout: TimelineLayout; totalHeight: number; rowCount: number } {
	const layout = computeLayout(zoomY);
	// Row 0 is the empty insertion lane above the highest z layer. There is
	// deliberately no empty row below the lowest layer: z=0 is the floor.
	const rowCount = Math.max(1, layerCount + 1);
	const naturalHeight = layout.trackStartY + rowCount * layout.trackHeight;
	const totalHeight = Math.max(viewportHeight, naturalHeight);

	return {
		layout: {
			...layout,
			trackStartY: layout.trackStartY + totalHeight - naturalHeight,
		},
		totalHeight,
		rowCount,
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

export function getPatternColor(patternId: string): string {
	let hash = 0;
	for (let i = 0; i < patternId.length; i++) {
		hash = (hash * 31 + patternId.charCodeAt(i)) | 0;
	}
	return patternColors[Math.abs(hash) % patternColors.length];
}
