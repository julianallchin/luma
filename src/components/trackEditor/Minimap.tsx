import { useRef, useCallback, useEffect } from "react";
import { useTrackEditorStore } from "@/useTrackEditorStore";

const MINIMAP_HEIGHT = 40;

export function Minimap() {
	const canvasRef = useRef<HTMLCanvasElement>(null);
	const containerRef = useRef<HTMLDivElement>(null);
	
	const waveform = useTrackEditorStore((s) => s.waveform);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const durationSeconds = useTrackEditorStore((s) => s.durationSeconds);
	const zoom = useTrackEditorStore((s) => s.zoom);
	const scrollX = useTrackEditorStore((s) => s.scrollX);
	const setScrollX = useTrackEditorStore((s) => s.setScrollX);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);

	const getViewportWindow = useCallback(() => {
		if (!containerRef.current || durationSeconds <= 0) return { left: 0, width: 100 };
		const containerWidth = containerRef.current.clientWidth;
		const totalTimelineWidth = durationSeconds * zoom;
		const viewportWidth = containerWidth;
		const scrollPercent = scrollX / Math.max(1, totalTimelineWidth - viewportWidth);
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

		if (waveform && waveform.previewSamples.length > 0) {
			const samples = waveform.previewSamples;
			ctx.strokeStyle = "rgba(139, 92, 246, 0.5)";
			ctx.beginPath();
			for (let x = 0; x < width; x++) {
				const sampleIndex = Math.floor((x / width) * (samples.length / 2)) * 2;
				const min = samples[sampleIndex] ?? 0;
				const max = samples[sampleIndex + 1] ?? 0;
				const yMin = height / 2 + min * (height / 2) * 0.8;
				const yMax = height / 2 + max * (height / 2) * 0.8;
				ctx.moveTo(x, yMin);
				ctx.lineTo(x, yMax);
			}
			ctx.stroke();
		}

		if (durationSeconds > 0) {
			for (const ann of annotations) {
				const x = (ann.startTime / durationSeconds) * width;
				const w = ((ann.endTime - ann.startTime) / durationSeconds) * width;
				ctx.fillStyle = ann.patternColor || "#8b5cf6";
				ctx.globalAlpha = 0.6;
				ctx.fillRect(x, height - 8, Math.max(2, w), 6);
				ctx.globalAlpha = 1;
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
	}, [waveform, annotations, durationSeconds, getViewportWindow, playheadPosition]);

	const handleMouseDown = useCallback((e: React.MouseEvent) => {
		const container = containerRef.current;
		if (!container || durationSeconds <= 0) return;
		const rect = container.getBoundingClientRect();
		const totalTimelineWidth = durationSeconds * zoom;
		const viewportWidth = container.clientWidth;

		const updateScroll = (clientX: number) => {
			const clickPercent = (clientX - rect.left) / rect.width;
			const targetScrollX = clickPercent * totalTimelineWidth - viewportWidth / 2;
			setScrollX(Math.max(0, Math.min(totalTimelineWidth - viewportWidth, targetScrollX)));
		};

		updateScroll(e.clientX);

		const handleMouseMove = (moveEvent: MouseEvent) => updateScroll(moveEvent.clientX);
		const handleMouseUp = () => {
			document.removeEventListener("mousemove", handleMouseMove);
			document.removeEventListener("mouseup", handleMouseUp);
		};
		document.addEventListener("mousemove", handleMouseMove);
		document.addEventListener("mouseup", handleMouseUp);
	}, [durationSeconds, zoom, setScrollX]);

	return (
		<div
			ref={containerRef}
			className="relative border-b border-border/50 cursor-pointer"
			style={{ height: MINIMAP_HEIGHT }}
			onMouseDown={handleMouseDown}
		>
			<canvas ref={canvasRef} className="absolute inset-0" />
		</div>
	);
}

