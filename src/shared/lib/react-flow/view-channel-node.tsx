import * as React from "react";
import type { NodeProps } from "reactflow";
import { BaseNode, usePatternEntryPlayback } from "./base-node";
import type { ViewChannelNodeData } from "./types";

const SERIES_SAMPLE_LIMIT = 256;
const CHROMA_LINE_COLORS = Array.from({ length: 12 }, (_, idx) => {
	const hue = Math.round((idx * 360) / 12);
	return `hsl(${hue}, 82%, 62%)`;
});
const CANVAS_WIDTH = 360;
const CANVAS_HEIGHT = 140;

export function ViewChannelNode(props: NodeProps<ViewChannelNodeData>) {
	const { data } = props;
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const playback = usePatternEntryPlayback(data.playbackSourceId);

	const seriesPlotData = React.useMemo(() => {
		const series = data.seriesData;
		if (!series?.samples.length) {
			return null;
		}

		const samples = series.samples.slice(-SERIES_SAMPLE_LIMIT);
		const startTime = samples[0].time;
		const endTime = samples[samples.length - 1].time;
		const timeRange = Math.max(0.001, endTime - startTime);

		let maxValue = 0;
		for (const sample of samples) {
			for (const value of sample.values) {
				if (value > maxValue) {
					maxValue = value;
				}
			}
		}

		return {
			samples,
			startTime,
			timeRange,
			maxValue: Math.max(maxValue, 1e-4),
			dimension: series.dim,
		};
	}, [data.seriesData]);

	const seriesLegendItems = React.useMemo(() => {
		const series = data.seriesData;
		const latestSample = series?.samples.length
			? series.samples[series.samples.length - 1]
			: null;
		if (!series || !latestSample) {
			return [];
		}

		const labels =
			series.labels ??
			Array.from({ length: latestSample.values.length }, (_, idx) => `${idx}`);
		const maxValue = Math.max(0.0001, ...latestSample.values);

		return labels.map((label, idx) => {
			const value = latestSample.values[idx] ?? 0;
			return {
				label,
				value,
				normalized: maxValue > 0 ? Math.min(1, value / maxValue) : 0,
				color: CHROMA_LINE_COLORS[idx % CHROMA_LINE_COLORS.length],
			};
		});
	}, [data.seriesData]);

	// Draw series on canvas
	React.useEffect(() => {
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

		if (!seriesPlotData) {
			const logicalBgWidth = logicalWidth;
			const logicalBgHeight = logicalHeight;
			ctx.fillStyle = "rgba(15, 23, 42, 0.9)";
			ctx.fillRect(0, 0, logicalBgWidth, logicalBgHeight);
			return;
		}

		const logicalBgWidth = logicalWidth;
		const logicalBgHeight = logicalHeight;
		ctx.fillStyle = "rgba(15, 23, 42, 0.9)";
		// ctx.fillRect(0, 0, logicalBgWidth, logicalBgHeight);

		const drawWidth = logicalBgWidth - padding * 2;
		const drawHeight = logicalBgHeight - padding * 2;

		for (
			let seriesIndex = 0;
			seriesIndex < seriesPlotData.dimension;
			seriesIndex += 1
		) {
			ctx.beginPath();
			ctx.lineWidth = 1.5;
			ctx.lineJoin = "round";
			ctx.lineCap = "round";
			ctx.strokeStyle =
				CHROMA_LINE_COLORS[seriesIndex % CHROMA_LINE_COLORS.length];

			for (
				let sampleIndex = 0;
				sampleIndex < seriesPlotData.samples.length;
				sampleIndex += 1
			) {
				const sample = seriesPlotData.samples[sampleIndex];
				const normalizedX =
					(sample.time - seriesPlotData.startTime) / seriesPlotData.timeRange;
				const x = padding + normalizedX * drawWidth;
				const value = sample.values[seriesIndex] ?? 0;
				const normalizedY = Math.max(
					0,
					Math.min(1, value / seriesPlotData.maxValue),
				);
				const y = logicalBgHeight - padding - normalizedY * drawHeight;

				if (sampleIndex === 0) {
					ctx.moveTo(x, y);
				} else {
					ctx.lineTo(x, y);
				}
			}

			ctx.stroke();
		}
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
				className={`relative bg-background text-[11px] ${playback.hasActive ? "cursor-pointer" : "cursor-default"}`}
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
						aria-label="Series preview graph"
					/>
				) : (
					<p className="text-center text-[11px] text-slate-400">
						waiting for series data…
					</p>
				)}
				{playback.hasActive && (
					<div
						className="pointer-events-none absolute inset-y-1 w-px bg-red-500/80"
						style={{ left: `${playback.progress * 100}%` }}
					/>
				)}
			</div>
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
}

