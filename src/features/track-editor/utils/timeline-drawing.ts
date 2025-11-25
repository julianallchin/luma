import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import type { BeatGrid } from "@/bindings/schema";
import type { TrackWaveform } from "../stores/use-track-editor-store";
import {
	HEADER_HEIGHT,
	WAVEFORM_HEIGHT,
	TRACK_HEIGHT,
	ANNOTATION_LANE_HEIGHT,
} from "./timeline-constants";

export function drawBeatGrid(
	ctx: CanvasRenderingContext2D,
	beatGrid: BeatGrid,
	startTime: number,
	endTime: number,
	currentZoom: number,
	scrollLeft: number,
	height: number,
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
	ctx.strokeStyle = "rgba(139, 92, 246, 0.1)";
	ctx.fillStyle = "#666";
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
		ctx.moveTo(x, HEADER_HEIGHT);
		ctx.lineTo(x, height);
		ctx.stroke();

		if (currentZoom > 100) {
			ctx.beginPath();
			ctx.moveTo(x, HEADER_HEIGHT - 5);
			ctx.lineTo(x, HEADER_HEIGHT);
			ctx.stroke();
		}
	}

	// 2. Draw Downbeats (Measure Starts)
	ctx.fillStyle = "#ddd";

	beatGrid.downbeats.forEach((downbeat, index) => {
		if (downbeat < startTime || downbeat > endTime) return;

		const x = Math.floor(downbeat * currentZoom - scrollLeft) + 0.5;
		const isMajorBar = index % barLabelStep === 0;

		ctx.strokeStyle = isMajorBar
			? "rgba(139, 92, 246, 0.35)"
			: "rgba(139, 92, 246, 0.15)";
		ctx.beginPath();
		ctx.moveTo(x, HEADER_HEIGHT - (isMajorBar ? 12 : 8));
		ctx.lineTo(x, height);
		ctx.stroke();

		if (isMajorBar) {
			ctx.fillText(`${index + 1}`, x + 4, HEADER_HEIGHT - 10);
		}
	});
}

export function drawTimeRuler(
	ctx: CanvasRenderingContext2D,
	startTime: number,
	endTime: number,
	currentZoom: number,
	scrollLeft: number,
) {
	const tickInterval = currentZoom < 50 ? 5 : 1;
	const firstTick = Math.floor(startTime / tickInterval) * tickInterval;

	for (let t = firstTick; t <= endTime; t += tickInterval) {
		const x = Math.floor(t * currentZoom - scrollLeft) + 0.5;
		const isMajor = t % 10 === 0;

		ctx.strokeStyle = isMajor ? "#404040" : "#262626";
		ctx.beginPath();
		ctx.moveTo(x, HEADER_HEIGHT - (isMajor ? 10 : 5));
		ctx.lineTo(x, HEADER_HEIGHT);
		ctx.stroke();

		if (isMajor) {
			ctx.fillStyle = "#888888";
			ctx.fillText(
				`${Math.floor(t / 60)}:${(t % 60).toString().padStart(2, "0")}`,
				x + 3,
				HEADER_HEIGHT - 12,
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
) {
	const waveformY = HEADER_HEIGHT;
	ctx.fillStyle = "#0a0a0a";
	ctx.fillRect(0, waveformY, width, WAVEFORM_HEIGHT);

	ctx.strokeStyle = "#333333";
	ctx.beginPath();
	ctx.moveTo(0, waveformY + WAVEFORM_HEIGHT);
	ctx.lineTo(width, waveformY + WAVEFORM_HEIGHT);
	ctx.stroke();

	const centerY = waveformY + WAVEFORM_HEIGHT / 2;
	const halfHeight = (WAVEFORM_HEIGHT - 8) / 2;

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
				ctx.fillRect(
					x,
					yTop,
					Math.ceil(barWidth),
					Math.max(1, yBottom - yTop),
				);
			}
		} else {
			ctx.fillStyle = "#6366f1";
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
	selectedAnnotationId: number | null,
	getBeatMetrics: (startTime: number, endTime: number) => {
		startBeatNumber: number;
		beatCount: number;
	} | null,
) {
	const trackY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
	ctx.fillStyle = "rgba(0, 0, 0, 0.2)";
	ctx.fillRect(0, trackY, width, TRACK_HEIGHT);

	ctx.strokeStyle = "#222222";
	ctx.beginPath();
	ctx.moveTo(0, trackY + TRACK_HEIGHT);
	ctx.lineTo(width, trackY + TRACK_HEIGHT);
	ctx.stroke();

	for (const ann of annotations) {
		if (ann.endTime < startTime || ann.startTime > endTime) continue;

		const x = Math.floor(ann.startTime * currentZoom - scrollLeft);
		const w = Math.max(
			4,
			Math.floor((ann.endTime - ann.startTime) * currentZoom),
		);
		const y = trackY + TRACK_HEIGHT + 4;
		const h = ANNOTATION_LANE_HEIGHT - 8;

		const isSelected = ann.id === selectedAnnotationId;

		ctx.fillStyle = ann.patternColor || "#8b5cf6";
		ctx.globalAlpha = isSelected ? 1 : 0.85;
		ctx.fillRect(x, y, w, h);

		if (isSelected) {
			ctx.strokeStyle = "rgba(255, 255, 255, 0.9)";
			ctx.lineWidth = 1;
			ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);

			ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
			ctx.fillRect(x, y, 6, h);
			ctx.fillRect(x + w - 6, y, 6, h);

			ctx.fillStyle = "rgba(0, 0, 0, 0.4)";
			ctx.fillRect(x + 2, y + h / 2 - 4, 2, 8);
			ctx.fillRect(x + w - 4, y + h / 2 - 4, 2, 8);
		} else {
			ctx.strokeStyle = "rgba(255, 255, 255, 0.15)";
			ctx.lineWidth = 1;
			ctx.strokeRect(x, y, w, h);
		}

		if (w > 30) {
			ctx.fillStyle = "white";
			ctx.globalAlpha = 0.9;
			ctx.save();
			ctx.beginPath();
			ctx.rect(x + 4, y, w - 8, h);
			ctx.clip();
			ctx.font = "11px system-ui, sans-serif";

			const beatMetrics = getBeatMetrics(ann.startTime, ann.endTime);
			const beatLabel = beatMetrics
				? `${beatMetrics.beatCount} beats · b${beatMetrics.startBeatNumber.toFixed(1)}`
				: `${(ann.endTime - ann.startTime).toFixed(2)}s`;
			const label = ann.patternName || `Pattern ${ann.patternId}`;

			ctx.fillText(`${label} · ${beatLabel}`, x + 8, y + h / 2 + 4);
			ctx.restore();
		}
		ctx.globalAlpha = 1;
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
) {
	const trackY = HEADER_HEIGHT + WAVEFORM_HEIGHT;
	const previewX = Math.floor(
		dragPreview.startTime * currentZoom - scrollLeft,
	);
	const previewW = Math.max(
		4,
		Math.floor((dragPreview.endTime - dragPreview.startTime) * currentZoom),
	);
	const previewY = trackY + TRACK_HEIGHT + 4;
	const previewH = ANNOTATION_LANE_HEIGHT - 8;

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
		ctx.fillText(
			dragPreview.name,
			previewX + 8,
			previewY + previewH / 2 + 4,
		);
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
) {
	if (playheadTime < startTime || playheadTime > endTime) return;

	const x = Math.floor(playheadTime * currentZoom - scrollLeft) + 0.5;
	ctx.strokeStyle = "#f59e0b";
	ctx.lineWidth = 1;
	ctx.beginPath();
	ctx.moveTo(x, 0);
	ctx.lineTo(x, height);
	ctx.stroke();

	ctx.fillStyle = "#f59e0b";
	ctx.beginPath();
	ctx.moveTo(x - 6, 0);
	ctx.lineTo(x + 6, 0);
	ctx.lineTo(x, 8);
	ctx.closePath();
	ctx.fill();
}

