import * as React from "react";
import type { NodeProps } from "reactflow";
import type { Signal } from "@/bindings/schema";
import { useViewDataStore } from "@/features/patterns/stores/use-view-data-store";
import { BaseNode, PlaybackIndicator } from "./base-node";
import type { ViewChannelNodeData } from "./types";

const CHROMA_LINE_COLORS = Array.from({ length: 12 }, (_, idx) => {
	const hue = Math.round((idx * 360) / 12);
	return `hsl(${hue}, 82%, 62%)`;
});
const CANVAS_WIDTH = 720;
const CANVAS_HEIGHT = 140;

// Minimum ms between canvas redraws (roughly 30fps max)
const THROTTLE_MS = 33;

/**
 * Stroke the signal onto the canvas. Runs inside requestAnimationFrame — all
 * per-sample work (min/max scan, coordinate transform) lives here so frames
 * dropped under load cost nothing.
 */
function drawSignal(canvas: HTMLCanvasElement, signal: Signal) {
	const ctx = canvas.getContext("2d");
	if (!ctx) return;

	const logicalWidth = CANVAS_WIDTH;
	const logicalHeight = CANVAS_HEIGHT;
	const dpr = Math.max(window.devicePixelRatio ?? 1, 1);
	const scaledWidth = Math.round(logicalWidth * dpr);
	const scaledHeight = Math.round(logicalHeight * dpr);

	if (canvas.width !== scaledWidth || canvas.height !== scaledHeight) {
		canvas.width = scaledWidth;
		canvas.height = scaledHeight;
	}

	ctx.setTransform(1, 0, 0, 1, 0, 0);
	ctx.clearRect(0, 0, canvas.width, canvas.height);
	ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

	const { n, t, c, data: rawData } = signal;
	const numLines = n > 1 ? n : c;
	const isSpatial = n > 1;

	let maxValue = -Infinity;
	let minValue = Infinity;
	for (const v of rawData) {
		if (v > maxValue) maxValue = v;
		if (v < minValue) minValue = v;
	}
	if (!Number.isFinite(maxValue) || !Number.isFinite(minValue)) return;

	const padding = 6;
	const drawWidth = logicalWidth - padding * 2;
	const drawHeight = logicalHeight - padding * 2;
	const range = Math.max(maxValue - minValue, 1e-6);

	ctx.lineWidth = 1.5;
	ctx.lineJoin = "round";
	ctx.lineCap = "round";

	for (let line = 0; line < numLines; line++) {
		ctx.beginPath();
		ctx.strokeStyle = CHROMA_LINE_COLORS[line % CHROMA_LINE_COLORS.length];

		if (t === 1) {
			const idx = isSpatial ? line * c : line;
			const val = rawData[idx] ?? 0;
			const normalizedY = Math.max(0, Math.min(1, (val - minValue) / range));
			const y = logicalHeight - padding - normalizedY * drawHeight;
			ctx.moveTo(padding, y);
			ctx.lineTo(logicalWidth - padding, y);
		} else {
			for (let timeStep = 0; timeStep < t; timeStep++) {
				const idx = isSpatial
					? line * (t * c) + timeStep * c
					: timeStep * c + line;
				const val = rawData[idx] ?? 0;
				const x = padding + (timeStep / (t - 1)) * drawWidth;
				const normalizedY = Math.max(0, Math.min(1, (val - minValue) / range));
				const y = logicalHeight - padding - normalizedY * drawHeight;

				if (timeStep === 0) ctx.moveTo(x, y);
				else ctx.lineTo(x, y);
			}
		}
		ctx.stroke();
	}

	// Axis labels
	ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
	ctx.fillStyle = "rgba(226, 232, 240, 0.85)";
	ctx.textBaseline = "top";
	ctx.fillText(maxValue.toFixed(2), padding, padding);
	ctx.textBaseline = "bottom";
	ctx.fillText(minValue.toFixed(2), padding, logicalHeight - padding);
}

export const ViewSignalNode = React.memo(function ViewSignalNode(
	props: NodeProps<ViewChannelNodeData>,
) {
	const { data, id } = props;
	// Subscribed per-node: only this node re-renders when its result changes.
	const viewSamples = useViewDataStore((s) => s.views[id] ?? null);
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const lastDrawRef = React.useRef(0);
	const rafIdRef = React.useRef<number | null>(null);
	const latestSignalRef = React.useRef<Signal | null>(null);

	const hasSignal = !!viewSamples;

	// Frame-dropping draw scheduler: never draw synchronously during the React
	// commit, and when results arrive faster than the throttle, the single
	// queued frame just picks up the latest data from the ref.
	React.useEffect(() => {
		latestSignalRef.current = viewSamples;
		if (!latestSignalRef.current) return;
		if (rafIdRef.current !== null) return;

		const renderFrame = () => {
			const elapsed = performance.now() - lastDrawRef.current;
			if (elapsed < THROTTLE_MS) {
				rafIdRef.current = requestAnimationFrame(renderFrame);
				return;
			}
			rafIdRef.current = null;
			const canvas = canvasRef.current;
			const signal = latestSignalRef.current;
			if (!canvas || !signal) return;
			drawSignal(canvas, signal);
			lastDrawRef.current = performance.now();
		};
		rafIdRef.current = requestAnimationFrame(renderFrame);
	}, [viewSamples]);

	React.useEffect(() => {
		return () => {
			if (rafIdRef.current !== null) {
				cancelAnimationFrame(rafIdRef.current);
				rafIdRef.current = null;
			}
		};
	}, []);

	// Legend values update at most a few times a second: results stream at
	// ~20 Hz during param drags, and committing 8 text nodes per result
	// invalidates layout each time.
	const LEGEND_THROTTLE_MS = 250;
	const [legendSignal, setLegendSignal] = React.useState<Signal | null>(null);
	const lastLegendRef = React.useRef(0);
	const legendTimeoutRef = React.useRef<NodeJS.Timeout | null>(null);
	React.useEffect(() => {
		const apply = () => {
			lastLegendRef.current = performance.now();
			setLegendSignal(latestSignalRef.current);
		};
		const elapsed = performance.now() - lastLegendRef.current;
		if (elapsed >= LEGEND_THROTTLE_MS) {
			apply();
		} else if (!legendTimeoutRef.current) {
			legendTimeoutRef.current = setTimeout(() => {
				legendTimeoutRef.current = null;
				apply();
			}, LEGEND_THROTTLE_MS - elapsed);
		}
		return () => {
			if (legendTimeoutRef.current) {
				clearTimeout(legendTimeoutRef.current);
				legendTimeoutRef.current = null;
			}
		};
	}, [viewSamples]);

	const seriesLegendItems = React.useMemo(() => {
		const signal = legendSignal;
		if (!signal) return [];

		const { n, c, data: rawData, t } = signal;
		const numItems = n > 1 ? n : c;
		const isSpatial = n > 1;
		const lastT = t > 0 ? t - 1 : 0;

		const items = [];
		const limit = 8;

		for (let i = 0; i < Math.min(numItems, limit); i++) {
			const idx = isSpatial ? i * (t * c) + lastT * c : lastT * c + i;
			const val = rawData[idx] ?? 0;

			items.push({
				label: isSpatial ? `Prim ${i}` : `Ch ${i}`,
				value: val,
				color: CHROMA_LINE_COLORS[i % CHROMA_LINE_COLORS.length],
			});
		}
		return items;
	}, [legendSignal]);

	const handleScrub = React.useCallback(
		(event: React.PointerEvent<HTMLDivElement>) => {
			event.preventDefault();
		},
		[],
	);

	const body = (
		<div
			className="overflow-hidden rounded-b-md"
			style={{ width: `${CANVAS_WIDTH}px` }}
		>
			<div
				className="relative bg-background text-[11px]"
				onPointerDown={handleScrub}
			>
				{hasSignal ? (
					<canvas
						ref={canvasRef}
						width={CANVAS_WIDTH}
						height={CANVAS_HEIGHT}
						className="block"
						style={{ width: `${CANVAS_WIDTH}px`, height: `${CANVAS_HEIGHT}px` }}
						role="img"
						aria-label="Signal preview graph"
					/>
				) : (
					<p className="text-center text-[11px] text-slate-400">
						waiting for signal data…
					</p>
				)}
				<PlaybackIndicator />
			</div>
			{/* Legend */}
			{seriesLegendItems.length > 0 && (
				<div className="text-[10px] text-slate-300 p-1">
					<div className="gap-1 flex flex-wrap overflow-x-hidden">
						{seriesLegendItems.map((item) => (
							<div
								key={item.label}
								className="flex items-center justify-between rounded-md border border-white/5 bg-white/5 px-1 py-0.5 gap-1"
							>
								<div className="flex items-center gap-1">
									<span
										className="h-2 w-2 rounded-full"
										style={{ background: item.color }}
									/>
									<span className="text-[9px] text-slate-200">
										{item.label}
									</span>
								</div>
								<span className="inline-block w-8 text-right font-mono text-[9px] tabular-nums text-slate-400">
									{item.value.toFixed(2)}
								</span>
							</div>
						))}
					</div>
				</div>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
});
