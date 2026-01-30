import * as React from "react";
import type { NodeProps } from "reactflow";
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

export const ViewSignalNode = React.memo(function ViewSignalNode(
	props: NodeProps<ViewChannelNodeData>,
) {
	const { data } = props;
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const lastDrawRef = React.useRef(0);
	const rafIdRef = React.useRef<number | null>(null);

	const seriesPlotData = React.useMemo(() => {
		const signal = data.viewSamples;
		if (!signal) return null;

		const { n, t, c, data: rawData } = signal;
		const numLines = n > 1 ? n : c;
		const isSpatial = n > 1;

		let maxValue = -Infinity;
		let minValue = Infinity;
		for (const v of rawData) {
			if (v > maxValue) maxValue = v;
			if (v < minValue) minValue = v;
		}
		// No data guard
		if (!Number.isFinite(maxValue) || !Number.isFinite(minValue)) return null;

		const lines: { color: string; points: number[] }[] = [];

		for (let i = 0; i < numLines; i++) {
			const points: number[] = [];
			for (let timeStep = 0; timeStep < t; timeStep++) {
				const idx = isSpatial ? i * (t * c) + timeStep * c : timeStep * c + i;
				points.push(rawData[idx] ?? 0);
			}
			lines.push({
				color: CHROMA_LINE_COLORS[i % CHROMA_LINE_COLORS.length],
				points,
			});
		}

		return {
			lines,
			t,
			maxValue,
			minValue,
		};
	}, [data.viewSamples]);

	const seriesLegendItems = React.useMemo(() => {
		const signal = data.viewSamples;
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
	}, [data.viewSamples]);

	// Draw series on canvas with throttling to avoid blocking main thread
	React.useEffect(() => {
		// Cancel any pending draw
		if (rafIdRef.current !== null) {
			cancelAnimationFrame(rafIdRef.current);
			rafIdRef.current = null;
		}

		const doDraw = () => {
			const canvas = canvasRef.current;
			if (!canvas) return;

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

			canvas.style.width = `${logicalWidth}px`;
			canvas.style.height = `${logicalHeight}px`;

			const width = canvas.width;
			const height = canvas.height;
			ctx.setTransform(1, 0, 0, 1, 0, 0);
			ctx.clearRect(0, 0, width, height);
			ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

			const padding = 6;

			if (!seriesPlotData) return;

			const logicalBgWidth = logicalWidth;
			const logicalBgHeight = logicalHeight;

			const drawWidth = logicalBgWidth - padding * 2;
			const drawHeight = logicalBgHeight - padding * 2;

			const { lines, maxValue, minValue } = seriesPlotData;
			const range = Math.max(maxValue - minValue, 1e-6);

			for (const line of lines) {
				ctx.beginPath();
				ctx.lineWidth = 1.5;
				ctx.lineJoin = "round";
				ctx.lineCap = "round";
				ctx.strokeStyle = line.color;

				const points = line.points;
				const numPoints = points.length;

				if (numPoints === 1) {
					const val = points[0];
					const normalizedY = Math.max(
						0,
						Math.min(1, (val - minValue) / range),
					);
					const y = logicalBgHeight - padding - normalizedY * drawHeight;
					ctx.moveTo(padding, y);
					ctx.lineTo(logicalBgWidth - padding, y);
				} else {
					for (let i = 0; i < numPoints; i++) {
						const val = points[i];
						const normalizedX = i / (numPoints - 1);
						const x = padding + normalizedX * drawWidth;
						const normalizedY = Math.max(
							0,
							Math.min(1, (val - minValue) / range),
						);
						const y = logicalBgHeight - padding - normalizedY * drawHeight;

						if (i === 0) ctx.moveTo(x, y);
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
			ctx.fillText(minValue.toFixed(2), padding, logicalBgHeight - padding);

			lastDrawRef.current = performance.now();
		};

		// Throttle: if we drew recently, defer to next animation frame
		const now = performance.now();
		const elapsed = now - lastDrawRef.current;
		if (elapsed < THROTTLE_MS) {
			rafIdRef.current = requestAnimationFrame(doDraw);
		} else {
			doDraw();
		}

		return () => {
			if (rafIdRef.current !== null) {
				cancelAnimationFrame(rafIdRef.current);
				rafIdRef.current = null;
			}
		};
	}, [seriesPlotData]);

	const handleScrub = React.useCallback(
		(event: React.PointerEvent<HTMLDivElement>) => {
			event.preventDefault();
		},
		[],
	);

	const body = (
		<div className="" style={{ width: `${CANVAS_WIDTH}px` }}>
			<div
				className="relative bg-background text-[11px]"
				onPointerDown={handleScrub}
			>
				{seriesPlotData ? (
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
								<span className="font-mono text-[9px] text-slate-400">
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
