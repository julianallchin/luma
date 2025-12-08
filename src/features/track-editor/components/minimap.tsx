import { useCallback, useEffect, useRef } from "react";
import { useTrackEditorStore } from "../stores/use-track-editor-store";

const MINIMAP_HEIGHT = 40;

export function Minimap() {
	const canvasRef = useRef<HTMLCanvasElement>(null);
	const containerRef = useRef<HTMLDivElement>(null);

	const waveform = useTrackEditorStore((s) => s.waveform);
	const durationSeconds = useTrackEditorStore((s) => s.durationSeconds);
	const zoom = useTrackEditorStore((s) => s.zoom);
	const scrollX = useTrackEditorStore((s) => s.scrollX);
	const setScrollX = useTrackEditorStore((s) => s.setScrollX);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);

	const getViewportWindow = useCallback(() => {
		if (!containerRef.current || durationSeconds <= 0)
			return { left: 0, width: 100 };
		const containerWidth = containerRef.current.clientWidth;
		const totalTimelineWidth = durationSeconds * zoom;
		const viewportWidth = containerWidth;
		const scrollPercent =
			scrollX / Math.max(1, totalTimelineWidth - viewportWidth);
		const viewportPercent = viewportWidth / totalTimelineWidth;
		return {
			left: scrollPercent * (100 - viewportPercent * 100),
			width: Math.min(100, viewportPercent * 100),
		};
	}, [durationSeconds, zoom, scrollX]);

	useEffect(() => {
		const canvas = canvasRef.current;
		const container = containerRef.current;
		if (!canvas || !container) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const rect = container.getBoundingClientRect();
		const dpr = window.devicePixelRatio || 1;
		canvas.width = rect.width * dpr;
		canvas.height = MINIMAP_HEIGHT * dpr;
		canvas.style.width = `${rect.width}px`;
		canvas.style.height = `${MINIMAP_HEIGHT}px`;
		ctx.scale(dpr, dpr);

		const width = rect.width;
		const height = MINIMAP_HEIGHT;

		ctx.fillStyle = "rgba(0, 0, 0, 0.3)";
		ctx.fillRect(0, 0, width, height);

		// Rekordbox 3-band style minimap waveform
		if (waveform?.previewBands) {
			const { low, mid, high } = waveform.previewBands;
			const numBuckets = low.length;
			const centerY = height / 2;
			const halfHeight = (height - 4) / 2; // 2px margin

			// Band colors
			const BLUE = [0, 85, 226]; // Low (bass)
			const ORANGE = [242, 170, 60]; // Mid
			const WHITE = [255, 255, 255]; // High

			for (let x = 0; x < width; x++) {
				const bucketIdx = Math.min(
					numBuckets - 1,
					Math.floor((x / width) * numBuckets),
				);

				// Draw low (blue)
				const lowH = Math.floor(low[bucketIdx] * halfHeight);
				if (lowH > 0) {
					ctx.fillStyle = `rgb(${BLUE[0]}, ${BLUE[1]}, ${BLUE[2]})`;
					ctx.fillRect(x, centerY - lowH, 1, lowH * 2);
				}

				// Draw mid (orange)
				const midH = Math.floor(mid[bucketIdx] * halfHeight);
				if (midH > 0) {
					ctx.fillStyle = `rgb(${ORANGE[0]}, ${ORANGE[1]}, ${ORANGE[2]})`;
					ctx.fillRect(x, centerY - midH, 1, midH * 2);
				}

				// Draw high (white)
				const highH = Math.floor(high[bucketIdx] * halfHeight);
				if (highH > 0) {
					ctx.fillStyle = `rgb(${WHITE[0]}, ${WHITE[1]}, ${WHITE[2]})`;
					ctx.fillRect(x, centerY - highH, 1, highH * 2);
				}
			}
		} else if (waveform && waveform.previewSamples.length > 0) {
			// Fallback to legacy color-based rendering
			const samples = waveform.previewSamples;
			const colors = waveform.previewColors;

			if (colors && colors.length === (samples.length / 2) * 3) {
				const numBuckets = samples.length / 2;

				for (let x = 0; x < width; x++) {
					const bucketIdx = Math.floor((x / width) * numBuckets);
					const min = samples[bucketIdx * 2] ?? 0;
					const max = samples[bucketIdx * 2 + 1] ?? 0;

					const r = colors[bucketIdx * 3];
					const g = colors[bucketIdx * 3 + 1];
					const b = colors[bucketIdx * 3 + 2];

					const yMin = height / 2 + min * (height / 2) * 0.9;
					const yMax = height / 2 + max * (height / 2) * 0.9;

					ctx.fillStyle = `rgb(${r}, ${g}, ${b})`;
					ctx.fillRect(x, yMin, 1, Math.max(1, yMax - yMin));
				}
			} else {
				// Fallback monochrome
				ctx.strokeStyle = "rgba(139, 92, 246, 0.5)";
				ctx.beginPath();
				for (let x = 0; x < width; x++) {
					const sampleIndex =
						Math.floor((x / width) * (samples.length / 2)) * 2;
					const min = samples[sampleIndex] ?? 0;
					const max = samples[sampleIndex + 1] ?? 0;
					const yMin = height / 2 + min * (height / 2) * 0.8;
					const yMax = height / 2 + max * (height / 2) * 0.8;
					ctx.moveTo(x, yMin);
					ctx.lineTo(x, yMax);
				}
				ctx.stroke();
			}
		}

		const viewport = getViewportWindow();
		const viewportX = (viewport.left / 100) * width;
		const viewportW = (viewport.width / 100) * width;
		ctx.strokeStyle = "rgba(255, 255, 255, 0.5)";
		ctx.lineWidth = 1;
		ctx.strokeRect(viewportX + 0.5, 0.5, viewportW - 1, height - 1);
		ctx.fillStyle = "rgba(255, 255, 255, 0.05)";
		ctx.fillRect(viewportX, 0, viewportW, height);

		if (durationSeconds > 0) {
			const playheadX = (playheadPosition / durationSeconds) * width;
			ctx.strokeStyle = "#f59e0b";
			ctx.lineWidth = 2;
			ctx.beginPath();
			ctx.moveTo(playheadX, 0);
			ctx.lineTo(playheadX, height);
			ctx.stroke();
		}
	}, [waveform, durationSeconds, getViewportWindow, playheadPosition]);

	const handleMouseDown = useCallback(
		(e: React.MouseEvent) => {
			const container = containerRef.current;
			if (!container || durationSeconds <= 0) return;
			const rect = container.getBoundingClientRect();
			const totalTimelineWidth = durationSeconds * zoom;
			const viewportWidth = container.clientWidth;

			const updateScroll = (clientX: number) => {
				const clickPercent = (clientX - rect.left) / rect.width;
				const targetScrollX =
					clickPercent * totalTimelineWidth - viewportWidth / 2;
				setScrollX(
					Math.max(
						0,
						Math.min(totalTimelineWidth - viewportWidth, targetScrollX),
					),
				);
			};

			updateScroll(e.clientX);

			const handleMouseMove = (moveEvent: MouseEvent) =>
				updateScroll(moveEvent.clientX);
			const handleMouseUp = () => {
				document.removeEventListener("mousemove", handleMouseMove);
				document.removeEventListener("mouseup", handleMouseUp);
			};
			document.addEventListener("mousemove", handleMouseMove);
			document.addEventListener("mouseup", handleMouseUp);
		},
		[durationSeconds, zoom, setScrollX],
	);

	return (
		<div
			ref={containerRef}
			role="slider"
			tabIndex={0}
			aria-label="Timeline minimap"
			aria-valuemin={0}
			aria-valuemax={durationSeconds}
			aria-valuenow={scrollX / zoom}
			className="relative border-b border-border/50 cursor-pointer"
			style={{ height: MINIMAP_HEIGHT }}
			onMouseDown={handleMouseDown}
		>
			<canvas ref={canvasRef} className="absolute inset-0" />
		</div>
	);
}
