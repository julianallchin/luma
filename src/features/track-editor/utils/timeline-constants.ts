// Timeline rendering constants
export const MIN_ZOOM = 25;
export const MAX_ZOOM = 500;
export const ZOOM_SENSITIVITY = 0.002;
export const HEADER_HEIGHT = 32;
export const WAVEFORM_HEIGHT = 96;
export const TRACK_HEIGHT = 80;
export const ANNOTATION_LANE_HEIGHT = 80; // Taller lane for patterns
export const MINIMAP_HEIGHT = 48;
export const ALWAYS_DRAW = false; // only draw when needed; rAF loop keeps cadence

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
