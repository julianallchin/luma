import type { BeatGrid } from "@/bindings/schema";
import type {
	TimelineAnnotation,
	TrackWaveform,
} from "../stores/use-track-editor-store";
import { getCanvasColor, getCanvasColorRgba } from "./canvas-colors";
import type { TimelineLayout } from "./timeline-constants";

/** Height of the annotation header bar (label + resize handles) */
export const ANNOTATION_HEADER_H = 18;

/** Returns true if a hex color is perceptually light (should use dark text). */
function isLightColor(hex: string): boolean {
	const c = hex.replace("#", "");
	const r = parseInt(c.substring(0, 2), 16);
	const g = parseInt(c.substring(2, 4), 16);
	const b = parseInt(c.substring(4, 6), 16);
	// Relative luminance (sRGB)
	const luminance = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
	return luminance > 0.5;
}

export function drawBeatGrid(
	ctx: CanvasRenderingContext2D,
	beatGrid: BeatGrid,
	startTime: number,
	endTime: number,
	currentZoom: number,
	scrollLeft: number,
	height: number,
	layout: TimelineLayout,
) {
	const beats = beatGrid.beats;
	const downbeats = beatGrid.downbeats;

	const averageBeatDuration =
		beats.length > 1
			? (beats[beats.length - 1] - beats[0]) / (beats.length - 1)
			: 0.5;
	const barDuration =
		downbeats.length > 1
			? downbeats[1] - downbeats[0]
			: averageBeatDuration * (beatGrid.beatsPerBar || 4);
	const pixelsPerBar = barDuration * currentZoom;
	const barLabelStep = Math.max(1, Math.ceil(80 / Math.max(1, pixelsPerBar)));
	const minBeatSpacingPx = 6;

	// Create a set of downbeat times for O(1) lookup
	const downbeatSet = new Set(downbeats.map((t) => Math.round(t * 1000)));

	// 1. Draw regular beats
	ctx.strokeStyle = getCanvasColorRgba("--primary", 0.25);
	ctx.fillStyle = getCanvasColor("--muted-foreground");
	ctx.lineWidth = 1;

	let lastBeatX: number | null = null;
	const renderBeats = true;
	for (const beat of beats) {
		if (beat < startTime || beat > endTime) continue;
		if (!renderBeats) continue;

		const beatTimeMs = Math.round(beat * 1000);
		if (downbeatSet.has(beatTimeMs)) continue; // Handled by downbeat loop

		const x = Math.floor(beat * currentZoom - scrollLeft) + 0.5;
		if (lastBeatX !== null && x - lastBeatX < minBeatSpacingPx) continue;
		lastBeatX = x;
		ctx.beginPath();
		ctx.moveTo(x, layout.headerHeight);
		ctx.lineTo(x, height);
		ctx.stroke();

		if (currentZoom > 100) {
			ctx.beginPath();
			ctx.moveTo(x, layout.headerHeight - 5);
			ctx.lineTo(x, layout.headerHeight);
			ctx.stroke();
		}
	}

	// 2. Draw Downbeats (Measure Starts)
	ctx.fillStyle = getCanvasColor("--foreground");

	beatGrid.downbeats.forEach((downbeat, index) => {
		if (downbeat < startTime || downbeat > endTime) return;

		const x = Math.floor(downbeat * currentZoom - scrollLeft) + 0.5;
		const isMajorBar = index % barLabelStep === 0;

		ctx.strokeStyle = isMajorBar
			? getCanvasColorRgba("--primary", 0.6)
			: getCanvasColorRgba("--primary", 0.35);
		ctx.lineWidth = isMajorBar ? 2 : 1;
		ctx.beginPath();
		ctx.moveTo(x, layout.headerHeight - (isMajorBar ? 12 : 8));
		ctx.lineTo(x, height);
		ctx.stroke();

		if (isMajorBar) {
			ctx.fillText(`${index + 1}`, x + 4, layout.headerHeight - 10);
		}
	});
}

export function drawTimeRuler(
	ctx: CanvasRenderingContext2D,
	startTime: number,
	endTime: number,
	currentZoom: number,
	scrollLeft: number,
	layout: TimelineLayout,
) {
	const tickInterval = currentZoom < 50 ? 5 : 1;
	const firstTick = Math.floor(startTime / tickInterval) * tickInterval;

	for (let t = firstTick; t <= endTime; t += tickInterval) {
		const x = Math.floor(t * currentZoom - scrollLeft) + 0.5;
		const isMajor = t % 10 === 0;

		ctx.strokeStyle = isMajor
			? getCanvasColor("--border")
			: getCanvasColor("--muted");
		ctx.beginPath();
		ctx.moveTo(x, layout.headerHeight - (isMajor ? 10 : 5));
		ctx.lineTo(x, layout.headerHeight);
		ctx.stroke();

		if (isMajor) {
			ctx.fillStyle = getCanvasColor("--muted-foreground");
			ctx.fillText(
				`${Math.floor(t / 60)}:${(t % 60).toString().padStart(2, "0")}`,
				x + 3,
				layout.headerHeight - 12,
			);
		}
	}
}

export function drawWaveform(
	ctx: CanvasRenderingContext2D,
	waveform: TrackWaveform | null,
	startTime: number,
	endTime: number,
	durationSeconds: number,
	currentZoom: number,
	scrollLeft: number,
	width: number,
	layout: TimelineLayout,
) {
	const waveformY = layout.headerHeight;
	ctx.fillStyle = getCanvasColor("--muted");
	ctx.fillRect(0, waveformY, width, layout.waveformHeight);

	ctx.strokeStyle = getCanvasColor("--border");
	ctx.beginPath();
	ctx.moveTo(0, waveformY + layout.waveformHeight);
	ctx.lineTo(width, waveformY + layout.waveformHeight);
	ctx.stroke();

	const centerY = waveformY + layout.waveformHeight / 2;
	const halfHeight = (layout.waveformHeight - 8) / 2;

	if (waveform?.bands) {
		const { low, mid, high } = waveform.bands;
		const numBuckets = low.length;
		const bucketsPerSecond = numBuckets / durationSeconds;

		const startBucket = Math.floor(startTime * bucketsPerSecond);
		const endBucket = Math.min(
			numBuckets,
			Math.ceil(endTime * bucketsPerSecond),
		);
		const barWidth = Math.max(1, currentZoom / bucketsPerSecond);

		const BLUE = [0, 85, 226];
		const ORANGE = [242, 170, 60];
		const WHITE = [255, 255, 255];

		for (let i = startBucket; i < endBucket; i++) {
			const time = i / bucketsPerSecond;
			const x = Math.floor(time * currentZoom - scrollLeft);
			if (x < -1 || x > width + 1) continue;

			const lowH = Math.floor(low[i] * halfHeight);
			if (lowH > 0) {
				ctx.fillStyle = `rgb(${BLUE[0]}, ${BLUE[1]}, ${BLUE[2]})`;
				ctx.fillRect(x, centerY - lowH, Math.ceil(barWidth), lowH * 2);
			}

			const midH = Math.floor(mid[i] * halfHeight);
			if (midH > 0) {
				ctx.fillStyle = `rgb(${ORANGE[0]}, ${ORANGE[1]}, ${ORANGE[2]})`;
				ctx.fillRect(x, centerY - midH, Math.ceil(barWidth), midH * 2);
			}

			const highH = Math.floor(high[i] * halfHeight);
			if (highH > 0) {
				ctx.fillStyle = `rgb(${WHITE[0]}, ${WHITE[1]}, ${WHITE[2]})`;
				ctx.fillRect(x, centerY - highH, Math.ceil(barWidth), highH * 2);
			}
		}
	} else if (waveform?.fullSamples) {
		const samples = waveform.fullSamples;
		const numBuckets = samples.length / 2;
		const bucketsPerSecond = numBuckets / durationSeconds;
		const colors = waveform.colors;

		const startBucket = Math.floor(startTime * bucketsPerSecond);
		const endBucket = Math.min(
			numBuckets,
			Math.ceil(endTime * bucketsPerSecond),
		);

		if (colors && colors.length === numBuckets * 3) {
			const barWidth = Math.max(1, currentZoom / bucketsPerSecond);

			for (let i = startBucket; i < endBucket; i++) {
				const time = i / bucketsPerSecond;
				const x = Math.floor(time * currentZoom - scrollLeft);
				if (x < -1 || x > width + 1) continue;

				const min = samples[i * 2];
				const max = samples[i * 2 + 1];
				const yTop = centerY - max * halfHeight;
				const yBottom = centerY - min * halfHeight;

				const r = colors[i * 3];
				const g = colors[i * 3 + 1];
				const b = colors[i * 3 + 2];

				ctx.fillStyle = `rgb(${r}, ${g}, ${b})`;
				ctx.fillRect(x, yTop, Math.ceil(barWidth), Math.max(1, yBottom - yTop));
			}
		} else {
			ctx.fillStyle = getCanvasColor("--chart-4");
			ctx.beginPath();

			for (let i = startBucket; i < endBucket; i++) {
				const time = i / bucketsPerSecond;
				const x = Math.floor(time * currentZoom - scrollLeft);
				if (x < -1 || x > width + 1) continue;

				const min = samples[i * 2];
				const max = samples[i * 2 + 1];
				const yTop = centerY - max * halfHeight;
				const yBottom = centerY - min * halfHeight;
				const h = Math.max(1, yBottom - yTop);

				ctx.rect(x, yTop, Math.max(1, currentZoom / bucketsPerSecond), h);
			}
			ctx.fill();
		}
	}
}

export function drawAnnotations(
	ctx: CanvasRenderingContext2D,
	annotations: TimelineAnnotation[],
	startTime: number,
	endTime: number,
	currentZoom: number,
	scrollLeft: number,
	width: number,
	selectedAnnotationIds: number[],
	rowMap: Map<number, number>,
	insertionData: { type: "insert" | "add"; y?: number; row?: number } | null,
	layout: TimelineLayout,
	getPreviewBitmap?:
		| ((annotationId: number) => ImageBitmap | undefined)
		| undefined,
) {
	const trackStartY = layout.trackStartY;

	// Draw empty top lane (drop target for adding layers above)
	ctx.fillStyle = "rgba(0, 0, 0, 0.3)";
	ctx.fillRect(0, layout.trackAreaY, width, trackStartY - layout.trackAreaY);
	ctx.strokeStyle = getCanvasColor("--border");
	ctx.beginPath();
	ctx.moveTo(0, trackStartY);
	ctx.lineTo(width, trackStartY);
	ctx.stroke();

	// Draw background for all lanes that have content
	// Find max row to know how far to draw background
	let maxRow = -1;
	for (const r of rowMap.values()) {
		maxRow = Math.max(maxRow, r);
	}
	// Ensure we draw at least one track if empty, or up to the max row
	const visibleTracks = Math.max(1, maxRow + 1);

	for (let i = 0; i < visibleTracks; i++) {
		const y = trackStartY + i * layout.trackHeight;
		ctx.fillStyle =
			i % 2 === 0
				? getCanvasColorRgba("--muted", 0.2)
				: getCanvasColorRgba("--muted", 0.15);
		ctx.fillRect(0, y, width, layout.trackHeight);

		ctx.strokeStyle = getCanvasColor("--border");
		ctx.beginPath();
		ctx.moveTo(0, y + layout.trackHeight);
		ctx.lineTo(width, y + layout.trackHeight);
		ctx.stroke();
	}

	// Darken empty area below tracks
	const tracksBottomY = trackStartY + visibleTracks * layout.trackHeight;
	ctx.fillStyle = "rgba(0, 0, 0, 0.3)";
	ctx.fillRect(0, tracksBottomY, width, ctx.canvas.height - tracksBottomY);

	// Draw 'Add' Highlight
	if (insertionData?.type === "add" && insertionData.row !== undefined) {
		const y = trackStartY + insertionData.row * layout.trackHeight;
		ctx.fillStyle = getCanvasColorRgba("--accent", 0.1);
		ctx.fillRect(0, y, width, layout.trackHeight);

		ctx.strokeStyle = getCanvasColorRgba("--accent", 0.4);
		ctx.lineWidth = 1;
		ctx.strokeRect(0.5, y + 0.5, width - 1, layout.trackHeight - 1);
	}

	// Draw Annotations
	for (const ann of annotations) {
		if (ann.endTime < startTime || ann.startTime > endTime) continue;

		const row = rowMap.get(ann.id) ?? 0;
		const trackY = trackStartY + row * layout.trackHeight;

		const x = Math.floor(ann.startTime * currentZoom - scrollLeft);
		const w = Math.max(
			4,
			Math.floor((ann.endTime - ann.startTime) * currentZoom),
		);
		const y = trackY + 1;
		const h = layout.trackHeight - 2;
		const headerH = ANNOTATION_HEADER_H;

		const isSelected = selectedAnnotationIds.includes(ann.id);
		const fallbackColor = ann.patternColor || getCanvasColor("--chart-5");
		const bodyAlpha = isSelected ? 1 : 0.75;

		// Body: heatmap preview or solid color
		const bodyY = y + headerH;
		const bodyH = h - headerH;
		const bitmap = getPreviewBitmap?.(ann.id);

		if (bitmap && w >= 8 && bodyH > 0) {
			// Header bar — always fully opaque
			ctx.globalAlpha = 1;
			ctx.fillStyle = fallbackColor;
			ctx.fillRect(x, y, w, headerH);

			// Heatmap body — transparent to let beat lines through
			ctx.globalAlpha = bodyAlpha;
			ctx.imageSmoothingEnabled = false;
			ctx.drawImage(bitmap, x, bodyY, w, bodyH);
			ctx.imageSmoothingEnabled = true;
		} else {
			// Fallback: opaque header, transparent body
			ctx.globalAlpha = 1;
			ctx.fillStyle = fallbackColor;
			ctx.fillRect(x, y, w, headerH);
			ctx.globalAlpha = bodyAlpha;
			ctx.fillRect(x, bodyY, w, bodyH);
		}

		// Border around entire annotation
		ctx.globalAlpha = 1;
		ctx.strokeStyle = getCanvasColorRgba("--foreground", 0.35);
		ctx.lineWidth = 1;
		ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);

		// Border separating header from body
		ctx.beginPath();
		ctx.moveTo(x, y + headerH + 0.5);
		ctx.lineTo(x + w, y + headerH + 0.5);
		ctx.stroke();

		// Selection chrome
		if (isSelected) {
			ctx.strokeStyle = getCanvasColorRgba("--foreground", 0.9);
			ctx.lineWidth = 1.5;
			ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);

			// Resize handles in header area only
			ctx.fillStyle = getCanvasColorRgba("--foreground", 0.9);
			ctx.fillRect(x, y, 6, headerH);
			ctx.fillRect(x + w - 6, y, 6, headerH);

			// Grip dots (3 dots vertically centered in header handles)
			ctx.fillStyle = getCanvasColorRgba("--background", 0.5);
			const dotR = 1;
			const dotSpacing = 4;
			const dotCenterY = y + headerH / 2;
			for (let d = -1; d <= 1; d++) {
				const dotY = dotCenterY + d * dotSpacing;
				ctx.beginPath();
				ctx.arc(x + 3, dotY, dotR, 0, Math.PI * 2);
				ctx.fill();
				ctx.beginPath();
				ctx.arc(x + w - 3, dotY, dotR, 0, Math.PI * 2);
				ctx.fill();
			}
		}

		// Label in header bar
		if (w > 30) {
			const label = ann.patternName || `Pattern ${ann.patternId}`;
			ctx.fillStyle = isLightColor(fallbackColor) ? "#000000" : "#ffffff";
			ctx.globalAlpha = isSelected ? 0.95 : 0.8;
			ctx.save();
			ctx.beginPath();
			ctx.rect(x + 8, y, w - 16, headerH);
			ctx.clip();
			ctx.font = "10px system-ui, sans-serif";
			ctx.fillText(label, x + 9, y + 12);
			ctx.restore();
		}
		ctx.globalAlpha = 1;
	}

	// Draw Insertion Line
	if (insertionData?.y !== undefined) {
		const y = insertionData.y;
		ctx.strokeStyle = getCanvasColor("--accent");
		ctx.lineWidth = 2;
		ctx.beginPath();
		ctx.moveTo(0, y);
		ctx.lineTo(width, y);
		ctx.stroke();

		// Add a little handle/indicator at the start
		ctx.fillStyle = getCanvasColor("--accent");
		ctx.beginPath();
		ctx.moveTo(0, y - 4);
		ctx.lineTo(8, y);
		ctx.lineTo(0, y + 4);
		ctx.fill();
	}
}

export function drawDragPreview(
	ctx: CanvasRenderingContext2D,
	dragPreview: {
		startTime: number;
		endTime: number;
		color: string;
		name: string;
	},
	currentZoom: number,
	scrollLeft: number,
	activeRow: number,
	layout: TimelineLayout,
) {
	const trackY = layout.trackStartY;
	const previewX = Math.floor(dragPreview.startTime * currentZoom - scrollLeft);
	const previewW = Math.max(
		4,
		Math.floor((dragPreview.endTime - dragPreview.startTime) * currentZoom),
	);
	const previewY = trackY + activeRow * layout.trackHeight + 1;
	const previewH = layout.trackHeight - 2;

	ctx.setLineDash([4, 4]);
	ctx.strokeStyle = dragPreview.color;
	ctx.lineWidth = 2;
	ctx.strokeRect(previewX + 0.5, previewY + 0.5, previewW - 1, previewH - 1);
	ctx.setLineDash([]);

	ctx.fillStyle = dragPreview.color;
	ctx.globalAlpha = 0.2;
	ctx.fillRect(previewX, previewY, previewW, previewH);
	ctx.globalAlpha = 1;

	if (previewW > 40) {
		ctx.fillStyle = dragPreview.color;
		ctx.font = "11px system-ui, sans-serif";
		ctx.fillText(dragPreview.name, previewX + 8, previewY + previewH / 2 + 4);
	}
}

export function drawDragPreviewAtY(
	ctx: CanvasRenderingContext2D,
	dragPreview: {
		startTime: number;
		endTime: number;
		color: string;
		name: string;
	},
	currentZoom: number,
	scrollLeft: number,
	y: number,
	layout: TimelineLayout,
) {
	const previewX = Math.floor(dragPreview.startTime * currentZoom - scrollLeft);
	const previewW = Math.max(
		4,
		Math.floor((dragPreview.endTime - dragPreview.startTime) * currentZoom),
	);
	// Ghost above the line when inserting at top, below the line otherwise
	const previewY = (y <= layout.trackStartY ? y - layout.trackHeight : y) + 1;
	const previewH = layout.trackHeight - 2;

	ctx.setLineDash([4, 4]);
	ctx.strokeStyle = dragPreview.color;
	ctx.lineWidth = 2;
	ctx.strokeRect(previewX + 0.5, previewY + 0.5, previewW - 1, previewH - 1);
	ctx.setLineDash([]);

	ctx.fillStyle = dragPreview.color;
	ctx.globalAlpha = 0.2;
	ctx.fillRect(previewX, previewY, previewW, previewH);
	ctx.globalAlpha = 1;

	if (previewW > 40) {
		ctx.fillStyle = dragPreview.color;
		ctx.font = "11px system-ui, sans-serif";
		ctx.fillText(dragPreview.name, previewX + 8, previewY + previewH / 2 + 4);
	}
}

export function drawPlayhead(
	ctx: CanvasRenderingContext2D,
	playheadTime: number,
	startTime: number,
	endTime: number,
	currentZoom: number,
	scrollLeft: number,
	height: number,
	_layout: TimelineLayout,
) {
	if (playheadTime < startTime || playheadTime > endTime) return;

	const x = Math.floor(playheadTime * currentZoom - scrollLeft) + 0.5;
	ctx.strokeStyle = getCanvasColor("--chart-3"); // Orange for playhead
	ctx.lineWidth = 1;
	ctx.beginPath();
	ctx.moveTo(x, 0);
	ctx.lineTo(x, height);
	ctx.stroke();

	ctx.fillStyle = getCanvasColor("--chart-3");
	ctx.beginPath();
	ctx.moveTo(x - 6, 0);
	ctx.lineTo(x + 6, 0);
	ctx.lineTo(x, 8);
	ctx.closePath();
	ctx.fill();
}

export function drawSelectionCursor(
	ctx: CanvasRenderingContext2D,
	cursor: {
		trackRow: number;
		trackRowEnd: number | null;
		startTime: number;
		endTime: number | null;
	},
	startTimeVisible: number,
	endTimeVisible: number,
	currentZoom: number,
	scrollLeft: number,
	layout: TimelineLayout,
) {
	const trackStartY = layout.trackStartY;

	// Calculate row range
	const minRow = Math.min(
		cursor.trackRow,
		cursor.trackRowEnd ?? cursor.trackRow,
	);
	const maxRow = Math.max(
		cursor.trackRow,
		cursor.trackRowEnd ?? cursor.trackRow,
	);
	// Y is in world coordinates - the context is already translated for scroll
	const cursorY = trackStartY + minRow * layout.trackHeight;
	const cursorHeight = (maxRow - minRow + 1) * layout.trackHeight;

	// Primary color for cursor
	const primaryColor = getCanvasColor("--accent");

	if (cursor.endTime === null) {
		// Point cursor - single vertical line
		if (
			cursor.startTime < startTimeVisible ||
			cursor.startTime > endTimeVisible
		)
			return;

		const x = Math.floor(cursor.startTime * currentZoom - scrollLeft) + 0.5;

		ctx.strokeStyle = primaryColor;
		ctx.lineWidth = 2;
		ctx.beginPath();
		ctx.moveTo(x, cursorY);
		ctx.lineTo(x, cursorY + cursorHeight);
		ctx.stroke();
	} else {
		// Range cursor - filled rectangle with borders
		const rangeStart = Math.min(cursor.startTime, cursor.endTime);
		const rangeEnd = Math.max(cursor.startTime, cursor.endTime);

		if (rangeEnd < startTimeVisible || rangeStart > endTimeVisible) return;

		const x1 = Math.floor(rangeStart * currentZoom - scrollLeft);
		const x2 = Math.floor(rangeEnd * currentZoom - scrollLeft);

		// Fill the range
		ctx.fillStyle = getCanvasColorRgba("--accent", 0.15);
		ctx.fillRect(x1, cursorY, x2 - x1, cursorHeight);

		// Draw border around the entire selection rectangle
		ctx.strokeStyle = primaryColor;
		ctx.lineWidth = 2;
		ctx.strokeRect(x1 + 0.5, cursorY + 0.5, x2 - x1 - 1, cursorHeight - 1);
	}
}
