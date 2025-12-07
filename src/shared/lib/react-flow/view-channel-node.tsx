import * as React from "react";
import type { NodeProps } from "reactflow";
import { useHostAudioStore } from "@/features/patterns/stores/use-host-audio-store";
import { BaseNode, computePlaybackState } from "./base-node";
import type { ViewChannelNodeData } from "./types";

const SERIES_SAMPLE_LIMIT = 256;
const CHROMA_LINE_COLORS = Array.from({ length: 12 }, (_, idx) => {
	const hue = Math.round((idx * 360) / 12);
	return `hsl(${hue}, 82%, 62%)`;
});
const CANVAS_WIDTH = 720;
const CANVAS_HEIGHT = 140;

export function ViewSignalNode(props: NodeProps<ViewChannelNodeData>) {
	const { data } = props;
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const isLoaded = useHostAudioStore((state) => state.isLoaded);
	const currentTime = useHostAudioStore((state) => state.currentTime);
	const durationSeconds = useHostAudioStore((state) => state.durationSeconds);
	const isPlaying = useHostAudioStore((state) => state.isPlaying);
	const playback = React.useMemo(
		() =>
			computePlaybackState({
				isLoaded,
				currentTime,
				durationSeconds,
				isPlaying,
			}),
		[isLoaded, currentTime, durationSeconds, isPlaying],
	);

	const seriesPlotData = React.useMemo(() => {
		// Handle Signal input (Signal struct)
		if (data.viewSamples) {
			const signal = data.viewSamples;
			// Signal has { n, t, c, data }
			// We prioritize showing N lines (primitives) if n > 1.
			// If n=1 and c > 1, show C lines (channels).
			// If n=1 and c=1, show 1 line.

			// Flattened data layout: [n * (t*c) + t*c + c]
			// We want to extract 'lines' of length T.

			const { n, t, c, data: rawData } = signal;
			const numLines = n > 1 ? n : c;
			const pointsPerLine = t;
			
			// If N > 1, we plot N lines (using first channel c=0)
			// If N=1, we plot C lines.
			const isSpatial = n > 1;

			let maxValue = 0;
			// Scan max value for scaling
			for (const v of rawData) {
				if (Math.abs(v) > maxValue) maxValue = Math.abs(v);
			}
			maxValue = Math.max(maxValue, 1.0); // Minimum scale 1.0

			const lines: { color: string; points: number[] }[] = [];

			for (let i = 0; i < numLines; i++) {
				const points: number[] = [];
				for (let timeStep = 0; timeStep < t; timeStep++) {
					let idx: number;
					if (isSpatial) {
						// Plotting primitive i, channel 0
						// idx = i * (t*c) + timeStep * c + 0
						idx = i * (t * c) + timeStep * c;
					} else {
						// Plotting primitive 0, channel i
						// idx = 0 * (t*c) + timeStep * c + i
						idx = timeStep * c + i;
					}
					points.push(rawData[idx] ?? 0);
				}
				lines.push({
					color: CHROMA_LINE_COLORS[i % CHROMA_LINE_COLORS.length],
					points,
				});
			}

			return {
				type: "signal",
				lines,
				t,
				maxValue,
			};
		}

		// Legacy Series Support
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
			type: "series",
			samples,
			startTime,
			timeRange,
			maxValue: Math.max(maxValue, 1e-4),
			dimension: series.dim,
		};
	}, [data.seriesData, data.viewSamples]);

	const seriesLegendItems = React.useMemo(() => {
		if (data.viewSamples) {
			const signal = data.viewSamples;
			const { n, c, data: rawData, t } = signal;
			
			// Show legend for current values (last time step)
			// Similar logic: if n>1 show primitives, else channels
			const numItems = n > 1 ? n : c;
			const isSpatial = n > 1;
			const lastT = t > 0 ? t - 1 : 0;
			
			const items = [];
			// Limit legend items to avoid UI clutter
			const limit = 8; 
			
			for (let i = 0; i < Math.min(numItems, limit); i++) {
				let val = 0;
				if (isSpatial) {
					// Val for primitive i, last time step, channel 0
					const idx = i * (t * c) + lastT * c;
					val = rawData[idx] ?? 0;
				} else {
					// Val for primitive 0, last time step, channel i
					const idx = lastT * c + i;
					val = rawData[idx] ?? 0;
				}
				
				items.push({
					label: isSpatial ? `Prim ${i}` : `Ch ${i}`,
					value: val,
					color: CHROMA_LINE_COLORS[i % CHROMA_LINE_COLORS.length],
				});
			}
			return items;
		}

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
				color: CHROMA_LINE_COLORS[idx % CHROMA_LINE_COLORS.length],
			};
		});
	}, [data.seriesData, data.viewSamples]);

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

		if (seriesPlotData.type === "signal") {
			// Draw Signal Lines
			const { lines, t, maxValue } = seriesPlotData as { lines: {color:string, points:number[]}[], t: number, maxValue: number };
			
			for (const line of lines) {
				ctx.beginPath();
				ctx.lineWidth = 1.5;
				ctx.lineJoin = "round";
				ctx.lineCap = "round";
				ctx.strokeStyle = line.color;
				
				const points = line.points;
				const numPoints = points.length;

				// If single point (T=1), draw horizontal line
				if (numPoints === 1) {
					const val = points[0];
					const normalizedY = Math.max(0, Math.min(1, val / maxValue));
					const y = logicalBgHeight - padding - normalizedY * drawHeight;
					ctx.moveTo(padding, y);
					ctx.lineTo(logicalBgWidth - padding, y);
				} else {
					// Draw curve
					for (let i = 0; i < numPoints; i++) {
						const val = points[i];
						const normalizedX = i / (numPoints - 1);
						const x = padding + normalizedX * drawWidth;
						const normalizedY = Math.max(0, Math.min(1, val / maxValue));
						const y = logicalBgHeight - padding - normalizedY * drawHeight;
						
						if (i === 0) ctx.moveTo(x, y);
						else ctx.lineTo(x, y);
					}
				}
				ctx.stroke();
			}
			return;
		}

		// Legacy Series drawing
		if (
			seriesPlotData.type === "raw" &&
			"samples" in seriesPlotData &&
			Array.isArray(seriesPlotData.samples)
		) {
			// Draw raw 1D samples
			const samples = seriesPlotData.samples as number[]; // Hint TS

			ctx.beginPath();
			ctx.lineWidth = 1.5;
			ctx.lineJoin = "round";
			ctx.lineCap = "round";
			ctx.strokeStyle = CHROMA_LINE_COLORS[0];

			for (let i = 0; i < samples.length; i++) {
				const val = samples[i];
				const normalizedX = i / (samples.length - 1);
				const x = padding + normalizedX * drawWidth;

				const normalizedY = Math.max(
					0,
					Math.min(1, val / seriesPlotData.maxValue),
				);
				const y = logicalBgHeight - padding - normalizedY * drawHeight;

				if (i === 0) ctx.moveTo(x, y);
				else ctx.lineTo(x, y);
			}
			ctx.stroke();
			return;
		}

		for (
			let seriesIndex = 0;
			seriesIndex < seriesPlotData.dimension;
			seriesIndex += 1
		) {
			// ... existing loop for Series types ...
			ctx.beginPath();
			ctx.lineWidth = 1.5;
			ctx.lineJoin = "round";
			ctx.lineCap = "round";
			ctx.strokeStyle =
				CHROMA_LINE_COLORS[seriesIndex % CHROMA_LINE_COLORS.length];

			const samples = seriesPlotData.samples as any[]; // TS hint

			for (
				let sampleIndex = 0;
				sampleIndex < samples.length;
				sampleIndex += 1
			) {
				const sample = samples[sampleIndex];
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
						waiting for signal data…
					</p>
				)}
				{playback.hasActive && (
					<div
						className="pointer-events-none absolute inset-y-1 w-px bg-red-500/80"
						style={{ left: `${playback.progress * 100}%` }}
					/>
				)}
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
}
