import { useCallback, useRef } from "react";
import type { TrackWaveform } from "../stores/use-track-editor-store";
import { getCanvasColor, getCanvasColorRgba } from "../utils/canvas-colors";
import { createOffscreenCanvas } from "../utils/canvas-compat";
import { MINIMAP_HEIGHT } from "../utils/timeline-constants";

type MinimapProps = {
	minimapRef: React.RefObject<HTMLCanvasElement | null>;
	durationMs: number;
	waveform: TrackWaveform | null;
	playheadPosition: number;
	loopRegion: { start: number; end: number } | null;
	zoomRef: React.MutableRefObject<number>;
	containerRef: React.RefObject<HTMLDivElement | null>;
	minimapBitmapRef: React.MutableRefObject<{
		canvas: HTMLCanvasElement | OffscreenCanvas | null;
		width: number;
		zoom: number;
		waveformGen: number;
		durationMs: number;
	}>;
};

export function useMinimapDrawing({
	minimapRef,
	durationMs,
	waveform,
	playheadPosition,
	loopRegion,
	zoomRef,
	containerRef,
	minimapBitmapRef,
}: MinimapProps) {
	const cachedWaveformRef = useRef(waveform);
	const waveformGenRef = useRef(0);
	if (cachedWaveformRef.current !== waveform) {
		cachedWaveformRef.current = waveform;
		waveformGenRef.current += 1;
	}
	const currentWaveformGen = waveformGenRef.current;

	const drawMinimap = useCallback(
		(playheadOverride?: number) => {
			const canvas = minimapRef.current;
			const container = containerRef.current;
			if (!canvas || !container || durationMs <= 0) return;

			const ctx = canvas.getContext("2d", { alpha: false });
			if (!ctx) return;

			const dpr = window.devicePixelRatio || 1;
			const width = container.clientWidth;
			const height = MINIMAP_HEIGHT;

			if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
				canvas.width = width * dpr;
				canvas.height = height * dpr;
				ctx.scale(dpr, dpr);
				canvas.style.width = `${width}px`;
				canvas.style.height = `${height}px`;
			}

			// ── Cached waveform bitmap ──
			const bitmapCache = minimapBitmapRef.current;
			const needsNewBitmap =
				!bitmapCache.canvas ||
				bitmapCache.width !== width ||
				bitmapCache.durationMs !== durationMs ||
				bitmapCache.waveformGen !== currentWaveformGen;

			if (needsNewBitmap) {
				// Render waveform to offscreen canvas (once)
				const oc = createOffscreenCanvas(width * dpr, height * dpr);
				const octx = oc.getContext("2d") as CanvasRenderingContext2D;
				if (octx) {
					octx.scale(dpr, dpr);
					octx.fillStyle = getCanvasColor("--muted");
					octx.fillRect(0, 0, width, height);

					const centerY = height / 2;
					const halfHeight = (height - 4) / 2;

					if (waveform?.previewBands) {
						const { low, mid, high } = waveform.previewBands;
						const numBuckets = low.length;

						const BLUE = [0, 85, 226];
						const ORANGE = [242, 170, 60];
						const WHITE = [255, 255, 255];

						for (let x = 0; x < width; x++) {
							const bucketIdx = Math.min(
								numBuckets - 1,
								Math.floor((x / width) * numBuckets),
							);

							const lowH = Math.floor(low[bucketIdx] * halfHeight);
							if (lowH > 0) {
								octx.fillStyle = `rgb(${BLUE[0]}, ${BLUE[1]}, ${BLUE[2]})`;
								octx.fillRect(x, centerY - lowH, 1, lowH * 2);
							}

							const midH = Math.floor(mid[bucketIdx] * halfHeight);
							if (midH > 0) {
								octx.fillStyle = `rgb(${ORANGE[0]}, ${ORANGE[1]}, ${ORANGE[2]})`;
								octx.fillRect(x, centerY - midH, 1, midH * 2);
							}

							const highH = Math.floor(high[bucketIdx] * halfHeight);
							if (highH > 0) {
								octx.fillStyle = `rgb(${WHITE[0]}, ${WHITE[1]}, ${WHITE[2]})`;
								octx.fillRect(x, centerY - highH, 1, highH * 2);
							}
						}
					} else if (waveform?.previewSamples?.length) {
						const samples = waveform.previewSamples;
						const numBuckets = samples.length / 2;
						octx.fillStyle = getCanvasColor("--chart-4");
						octx.globalAlpha = 0.5;
						for (let i = 0; i < width; i++) {
							const bucketIndex = Math.floor((i / width) * numBuckets) * 2;
							const min = samples[bucketIndex] ?? 0;
							const max = samples[bucketIndex + 1] ?? 0;
							const yTop = centerY - max * halfHeight * 0.8;
							const yBottom = centerY - min * halfHeight * 0.8;
							const h = Math.abs(yBottom - yTop) || 1;
							octx.fillRect(i, Math.min(yTop, yBottom), 1, h);
						}
						octx.globalAlpha = 1.0;
					}
				}

				bitmapCache.canvas = oc;
				bitmapCache.width = width;
				bitmapCache.durationMs = durationMs;
				bitmapCache.waveformGen = currentWaveformGen;
			}

			// Blit cached waveform
			if (bitmapCache.canvas) {
				ctx.drawImage(bitmapCache.canvas, 0, 0, width, height);
			}

			const timeToPixel = width / durationMs;
			const currentZoom = zoomRef.current;
			const scrollLeft = container.scrollLeft;

			// Draw viewport lens
			const visibleTimeStart = (scrollLeft / currentZoom) * 1000;
			const visibleTimeEnd = ((scrollLeft + width) / currentZoom) * 1000;
			const lensX = Math.max(0, visibleTimeStart * timeToPixel);
			const lensEnd = Math.min(width, visibleTimeEnd * timeToPixel);
			const lensW = Math.max(4, lensEnd - lensX);

			// De-emphasize the rest of the song so the overview reads first as a
			// location control, then as a waveform.
			ctx.fillStyle = getCanvasColorRgba("--background", 0.55);
			ctx.fillRect(0, 0, lensX, height);
			ctx.fillRect(lensEnd, 0, width - lensEnd, height);

			ctx.fillStyle = getCanvasColorRgba("--foreground", 0.08);
			ctx.fillRect(lensX, 0, lensW, height);

			ctx.strokeStyle = getCanvasColorRgba("--chart-3", 0.85);
			ctx.lineWidth = 1;
			ctx.strokeRect(lensX + 0.5, 0.5, lensW - 1, height - 1);

			// Lens handles
			ctx.fillStyle = getCanvasColorRgba("--chart-3", 0.9);
			ctx.fillRect(lensX, 0, 3, height);
			ctx.fillRect(lensX + lensW - 3, 0, 3, height);

			// Loop region band (yellow, drawn under playhead)
			if (loopRegion) {
				const lx1 = loopRegion.start * 1000 * timeToPixel;
				const lx2 = loopRegion.end * 1000 * timeToPixel;
				ctx.fillStyle = "rgba(234, 179, 8, 0.25)";
				ctx.fillRect(lx1, 0, lx2 - lx1, height);
				ctx.strokeStyle = "rgba(234, 179, 8, 0.8)";
				ctx.lineWidth = 1;
				ctx.beginPath();
				ctx.moveTo(lx1 + 0.5, 0);
				ctx.lineTo(lx1 + 0.5, height);
				ctx.moveTo(lx2 - 0.5, 0);
				ctx.lineTo(lx2 - 0.5, height);
				ctx.stroke();
			}

			// Playhead in minimap
			const playheadX =
				(playheadOverride ?? playheadPosition) * 1000 * timeToPixel;
			ctx.fillStyle = getCanvasColor("--chart-3");
			ctx.fillRect(playheadX - 0.5, 0, 1, height);
		},
		[
			durationMs,
			waveform,
			playheadPosition,
			loopRegion,
			currentWaveformGen,
			zoomRef,
			minimapRef,
			containerRef,
			minimapBitmapRef,
		],
	);

	return drawMinimap;
}
