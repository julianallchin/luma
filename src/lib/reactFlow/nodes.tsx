import * as React from "react";
import { Handle, type NodeProps, Position } from "reactflow";
import { Input } from "@/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerEyeDropper,
	ColorPickerFormat,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/components/ui/shadcn-io/color-picker";
import { useGraphStore } from "@/useGraphStore";
import { usePatternPlaybackStore } from "@/usePatternPlaybackStore";
import { useTracksStore } from "@/useTracksStore";
import type {
	BaseNodeData,
	HarmonyColorVisualizerNodeData,
	MelSpecNodeData,
	PatternEntryNodeData,
	ViewChannelNodeData,
} from "./types";

// BaseNode component that auto-renders handles
export function BaseNode<T extends BaseNodeData>(props: NodeProps<T>) {
	const { data } = props;

	return (
		<div className="relative bg-muted text-muted-foreground text-xs text-gray-100 border border-border shadow-sm overflow-hidden min-w-[170px] rounded">
			{/* header */}
			<div className="px-2 pt-1 pb-1 font-medium tracking-tight border-b">
				{data.title}
			</div>

			<div className="px-2 py-1 space-y-1.5">
				{data.inputs.map((port) => (
					<div key={port.id} className="flex items-center gap-1">
						<Handle
							type="target"
							id={port.id}
							position={Position.Left}
							className="!w-2 !h-2 !bg-orange-400 !rounded-full !border-none !relative !p-0 !m-0 !left-0 !top-0"
							style={{ transform: "none" }}
						/>
						<span>{port.label}</span>
					</div>
				))}
				{data.outputs.map((port) => (
					<div key={port.id} className="flex items-center justify-end gap-1">
						<span>{port.label}</span>
						<Handle
							type="source"
							id={port.id}
							position={Position.Right}
							className="!w-2 !h-2 !bg-orange-400 !rounded-full !border-none !relative !p-0 !m-0 !right-0 !top-0"
							style={{ transform: "none" }}
						/>
					</div>
				))}
			</div>

			{/* custom content hook (graphs, knobs, etc.) */}
			{"body" in data && (data as any).body}

			{/* parameters */}
			{"paramControls" in data && (data as any).paramControls}
		</div>
	);
}

// View Channel node with preview
const VIEW_SAMPLE_LIMIT = 128;
const SERIES_SAMPLE_LIMIT = 256;
const CHROMA_LINE_COLORS = Array.from({ length: 12 }, (_, idx) => {
	const hue = Math.round((idx * 360) / 12);
	return `hsl(${hue}, 82%, 62%)`;
});
const CANVAS_WIDTH = 360;
const CANVAS_HEIGHT = 140;
const DISABLED_PLAYBACK = {
	progress: 0,
	duration: 0,
	hasActive: false,
	currentTime: 0,
	isPlaying: false,
} as const;

// Local, event-driven subscription to avoid any chance of render loops.
// Safe to call with undefined: returns disabled playback and skips subscription.
function usePatternEntryPlayback(nodeId?: string | null) {
	const [playback, setPlayback] =
		React.useState<typeof DISABLED_PLAYBACK>(DISABLED_PLAYBACK);

	React.useEffect(() => {
		if (!nodeId) {
			setPlayback(DISABLED_PLAYBACK);
			return;
		}

		let mounted = true;
		const computePlayback = (
			state: ReturnType<typeof usePatternPlaybackStore.getState>,
		) => {
			if (state.activeNodeId !== nodeId) return DISABLED_PLAYBACK;
			const duration = state.durationSeconds || 0;
			const progress =
				duration > 0
					? Math.min(1, Math.max(0, state.currentTime / duration))
					: 0;
			return {
				progress,
				duration,
				hasActive: true,
				currentTime: state.currentTime,
				isPlaying: state.isPlaying,
			};
		};

		const shallowEqual = (
			a: typeof DISABLED_PLAYBACK,
			b: typeof DISABLED_PLAYBACK,
		) =>
			a.progress === b.progress &&
			a.duration === b.duration &&
			a.hasActive === b.hasActive &&
			a.currentTime === b.currentTime &&
			a.isPlaying === b.isPlaying;

		// Prime state
		setPlayback((prev) => {
			const next = computePlayback(usePatternPlaybackStore.getState());
			return shallowEqual(prev, next) ? prev : next;
		});

		const unsub = usePatternPlaybackStore.subscribe((state) => {
			if (!mounted) return;
			const next = computePlayback(state);
			setPlayback((prev) => (shallowEqual(prev, next) ? prev : next));
		});

		return () => {
			mounted = false;
			unsub();
		};
	}, [nodeId]);

	return playback;
}

function formatTime(totalSeconds: number): string {
	if (!Number.isFinite(totalSeconds) || totalSeconds <= 0) {
		return "0:00";
	}
	const clamped = Math.max(0, totalSeconds);
	const minutes = Math.floor(clamped / 60);
	const seconds = Math.floor(clamped % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${seconds}`;
}

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

export function PatternEntryNode(props: NodeProps<PatternEntryNodeData>) {
	const { id, data } = props;
	const entry = data.patternEntry ?? null;
	const durationSeconds = entry?.durationSeconds ?? 0;
	const playback = usePatternEntryPlayback(id);
	const beatSummary = entry?.beatGrid
		? `${entry.beatGrid.downbeats.length} downbeats`
		: "No beat grid";
	const sampleRateLabel = entry?.sampleRate
		? `${entry.sampleRate} Hz`
		: "Unknown rate";
	const durationLabel = entry ? formatTime(durationSeconds) : "0:00";
	const [pending, setPending] = React.useState(false);

	const handlePlay = async () => {
		if (!entry) return;
		setPending(true);
		try {
			await usePatternPlaybackStore.getState().play(id);
		} catch (err) {
			console.error("[PatternEntryNode] Failed to play", err);
		} finally {
			setPending(false);
		}
	};

	const handlePause = async () => {
		setPending(true);
		try {
			await usePatternPlaybackStore.getState().pause();
		} catch (err) {
			console.error("[PatternEntryNode] Failed to pause", err);
		} finally {
			setPending(false);
		}
	};

	const body = (
		<div className="px-2 pb-2 space-y-2 text-[11px]">
			<div className="flex items-center justify-between gap-2">
				<div className="flex gap-2">
					<button
						type="button"
						onClick={handlePlay}
						disabled={!entry || pending}
						className="rounded px-2 py-1 text-[11px] font-medium bg-emerald-600 text-white disabled:opacity-50"
					>
						Play
					</button>
					<button
						type="button"
						onClick={handlePause}
						disabled={pending}
						className="rounded px-2 py-1 text-[11px] font-medium bg-slate-700 text-white/80 disabled:opacity-50"
					>
						Pause
					</button>
				</div>
				<span className="text-[10px] uppercase tracking-wider text-slate-400">
					{entry ? durationLabel : "Awaiting audio"}
				</span>
			</div>
			<div className="relative h-3 rounded bg-slate-800 overflow-hidden">
				<div
					className="absolute inset-y-0 left-0 bg-emerald-500/70 transition-[width]"
					style={{ width: `${playback.progress * 100}%` }}
					aria-hidden
				/>
				<div className="absolute inset-0 flex items-center justify-center text-[10px] text-white/80 mix-blend-screen">
					{entry && playback.hasActive
						? `${formatTime(playback.currentTime)} / ${durationLabel}`
						: null}
				</div>
			</div>
			<div className="flex items-center justify-between text-[10px] uppercase tracking-wide text-slate-400">
				<span>{beatSummary}</span>
				<span>{sampleRateLabel}</span>
			</div>
			{!entry && (
				<p className="text-[10px] text-slate-500">
					Connect audio and beat grid inputs to enable preview playback.
				</p>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}

export function AudioSourceNode(props: NodeProps<BaseNodeData>) {
	const { data } = props;
	const { tracks } = useTracksStore();
	const params = useGraphStore(
		(state) => state.nodeParams[props.id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const rawTrackId = params.trackId;
	const selectedId =
		rawTrackId !== null && rawTrackId !== undefined ? Number(rawTrackId) : null;

	const validSelectedId =
		selectedId !== null &&
		!Number.isNaN(selectedId) &&
		tracks.some((t) => t.id === selectedId)
			? selectedId
			: null;

	React.useEffect(() => {
		if (validSelectedId === null && tracks.length > 0) {
			setParam(props.id, "trackId", tracks[0].id);
			data.onChange();
		}
	}, [tracks, validSelectedId, setParam, props.id, data]);

	const selectId = React.useId();

	// Ensure the selected value matches exactly with SelectItem values
	const selectValue =
		validSelectedId !== null && validSelectedId !== undefined
			? validSelectedId.toString()
			: "";

	const body = (
		<div className="px-2 pb-2">
			<label
				htmlFor={selectId}
				className="block text-[10px] text-gray-400 mb-1 uppercase tracking-wider"
			>
				Track
			</label>
			<Select
				value={selectValue}
				disabled={tracks.length === 0}
				onValueChange={(value) => {
					if (value === "") {
						setParam(props.id, "trackId", null);
						data.onChange();
						return;
					}
					const maybeId = parseInt(value, 10);
					setParam(props.id, "trackId", Number.isNaN(maybeId) ? null : maybeId);
					data.onChange();
				}}
			>
				<SelectTrigger id={selectId} className="w-full h-8 text-[11px]">
					<SelectValue placeholder="Select a track" />
				</SelectTrigger>
				<SelectContent>
					{tracks.map((track) => (
						<SelectItem key={track.id} value={track.id.toString()}>
							{track.title ?? `Track ${track.id}`}
						</SelectItem>
					))}
				</SelectContent>
			</Select>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}

export const MAGMA_LUT = [
	[0, 0, 4],
	[1, 0, 5],
	[1, 1, 6],
	[1, 1, 8],
	[2, 1, 9],
	[2, 2, 11],
	[2, 2, 13],
	[3, 3, 15],
	[3, 3, 18],
	[4, 4, 20],
	[5, 4, 22],
	[6, 5, 24],
	[6, 5, 26],
	[7, 6, 28],
	[8, 7, 30],
	[9, 7, 32],
	[10, 8, 34],
	[11, 9, 36],
	[12, 9, 38],
	[13, 10, 41],
	[14, 11, 43],
	[16, 11, 45],
	[17, 12, 47],
	[18, 13, 49],
	[19, 13, 52],
	[20, 14, 54],
	[21, 14, 56],
	[22, 15, 59],
	[24, 15, 61],
	[25, 16, 63],
	[26, 16, 66],
	[28, 16, 68],
	[29, 17, 71],
	[30, 17, 73],
	[32, 17, 75],
	[33, 17, 78],
	[34, 17, 80],
	[36, 18, 83],
	[37, 18, 85],
	[39, 18, 88],
	[41, 17, 90],
	[42, 17, 92],
	[44, 17, 95],
	[45, 17, 97],
	[47, 17, 99],
	[49, 17, 101],
	[51, 16, 103],
	[52, 16, 105],
	[54, 16, 107],
	[56, 16, 108],
	[57, 15, 110],
	[59, 15, 112],
	[61, 15, 113],
	[63, 15, 114],
	[64, 15, 116],
	[66, 15, 117],
	[68, 15, 118],
	[69, 16, 119],
	[71, 16, 120],
	[73, 16, 120],
	[74, 16, 121],
	[76, 17, 122],
	[78, 17, 123],
	[79, 18, 123],
	[81, 18, 124],
	[82, 19, 124],
	[84, 19, 125],
	[86, 20, 125],
	[87, 21, 126],
	[89, 21, 126],
	[90, 22, 126],
	[92, 22, 127],
	[93, 23, 127],
	[95, 24, 127],
	[96, 24, 128],
	[98, 25, 128],
	[100, 26, 128],
	[101, 26, 128],
	[103, 27, 128],
	[104, 28, 129],
	[106, 28, 129],
	[107, 29, 129],
	[109, 29, 129],
	[110, 30, 129],
	[112, 31, 129],
	[114, 31, 129],
	[115, 32, 129],
	[117, 33, 129],
	[118, 33, 129],
	[120, 34, 129],
	[121, 34, 130],
	[123, 35, 130],
	[124, 35, 130],
	[126, 36, 130],
	[128, 37, 130],
	[129, 37, 129],
	[131, 38, 129],
	[132, 38, 129],
	[134, 39, 129],
	[136, 39, 129],
	[137, 40, 129],
	[139, 41, 129],
	[140, 41, 129],
	[142, 42, 129],
	[144, 42, 129],
	[145, 43, 129],
	[147, 43, 128],
	[148, 44, 128],
	[150, 44, 128],
	[152, 45, 128],
	[153, 45, 128],
	[155, 46, 127],
	[156, 46, 127],
	[158, 47, 127],
	[160, 47, 127],
	[161, 48, 126],
	[163, 48, 126],
	[165, 49, 126],
	[166, 49, 125],
	[168, 50, 125],
	[170, 51, 125],
	[171, 51, 124],
	[173, 52, 124],
	[174, 52, 123],
	[176, 53, 123],
	[178, 53, 123],
	[179, 54, 122],
	[181, 54, 122],
	[183, 55, 121],
	[184, 55, 121],
	[186, 56, 120],
	[188, 57, 120],
	[189, 57, 119],
	[191, 58, 119],
	[192, 58, 118],
	[194, 59, 117],
	[196, 60, 117],
	[197, 60, 116],
	[199, 61, 115],
	[200, 62, 115],
	[202, 62, 114],
	[204, 63, 113],
	[205, 64, 113],
	[207, 64, 112],
	[208, 65, 111],
	[210, 66, 111],
	[211, 67, 110],
	[213, 68, 109],
	[214, 69, 108],
	[216, 69, 108],
	[217, 70, 107],
	[219, 71, 106],
	[220, 72, 105],
	[222, 73, 104],
	[223, 74, 104],
	[224, 76, 103],
	[226, 77, 102],
	[227, 78, 101],
	[228, 79, 100],
	[229, 80, 100],
	[231, 82, 99],
	[232, 83, 98],
	[233, 84, 98],
	[234, 86, 97],
	[235, 87, 96],
	[236, 88, 96],
	[237, 90, 95],
	[238, 91, 94],
	[239, 93, 94],
	[240, 95, 94],
	[241, 96, 93],
	[242, 98, 93],
	[242, 100, 92],
	[243, 101, 92],
	[244, 103, 92],
	[244, 105, 92],
	[245, 107, 92],
	[246, 108, 92],
	[246, 110, 92],
	[247, 112, 92],
	[247, 114, 92],
	[248, 116, 92],
	[248, 118, 92],
	[249, 120, 93],
	[249, 121, 93],
	[249, 123, 93],
	[250, 125, 94],
	[250, 127, 94],
	[250, 129, 95],
	[251, 131, 95],
	[251, 133, 96],
	[251, 135, 97],
	[252, 137, 97],
	[252, 138, 98],
	[252, 140, 99],
	[252, 142, 100],
	[252, 144, 101],
	[253, 146, 102],
	[253, 148, 103],
	[253, 150, 104],
	[253, 152, 105],
	[253, 154, 106],
	[253, 155, 107],
	[254, 157, 108],
	[254, 159, 109],
	[254, 161, 110],
	[254, 163, 111],
	[254, 165, 113],
	[254, 167, 114],
	[254, 169, 115],
	[254, 170, 116],
	[254, 172, 118],
	[254, 174, 119],
	[254, 176, 120],
	[254, 178, 122],
	[254, 180, 123],
	[254, 182, 124],
	[254, 183, 126],
	[254, 185, 127],
	[254, 187, 129],
	[254, 189, 130],
	[254, 191, 132],
	[254, 193, 133],
	[254, 194, 135],
	[254, 196, 136],
	[254, 198, 138],
	[254, 200, 140],
	[254, 202, 141],
	[254, 204, 143],
	[254, 205, 144],
	[254, 207, 146],
	[254, 209, 148],
	[254, 211, 149],
	[254, 213, 151],
	[254, 215, 153],
	[254, 216, 154],
	[253, 218, 156],
	[253, 220, 158],
	[253, 222, 160],
	[253, 224, 161],
	[253, 226, 163],
	[253, 227, 165],
	[253, 229, 167],
	[253, 231, 169],
	[253, 233, 170],
	[253, 235, 172],
	[252, 236, 174],
	[252, 238, 176],
	[252, 240, 178],
	[252, 242, 180],
	[252, 244, 182],
	[252, 246, 184],
	[252, 247, 185],
	[252, 249, 187],
	[252, 251, 189],
	[252, 253, 191],
];

function magmaColor(value: number): [number, number, number] {
	const x = Math.min(1, Math.max(0, value));
	const idx = x * (MAGMA_LUT.length - 1);
	const i0 = Math.floor(idx);
	const i1 = Math.min(MAGMA_LUT.length - 1, i0 + 1);
	const f = idx - i0;

	const [r0, g0, b0] = MAGMA_LUT[i0];
	const [r1, g1, b1] = MAGMA_LUT[i1];

	const r = r0 + f * (r1 - r0);
	const g = g0 + f * (g1 - g0);
	const b = b0 + f * (b1 - b0);
	return [r, g, b];
}

export function MelSpecNode(props: NodeProps<MelSpecNodeData>) {
	const { data } = props;
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const playback = usePatternEntryPlayback(data.playbackSourceId);

	React.useEffect(() => {
		if (!data.melSpec) return;
		const canvas = canvasRef.current;
		if (!canvas) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const { width, height, data: specData, beatGrid } = data.melSpec;
		const aspect = width / Math.max(1, height);
		const MIN_HEIGHT = 160;
		const MAX_HEIGHT = 320;
		let displayHeight = Math.max(MIN_HEIGHT, Math.min(MAX_HEIGHT, height * 2));
		let displayWidth = displayHeight * aspect;
		const MIN_WIDTH = 360;
		const MAX_WIDTH = 720;
		if (displayWidth < MIN_WIDTH) {
			displayWidth = MIN_WIDTH;
			displayHeight = Math.round(displayWidth / aspect);
		}
		if (displayWidth > MAX_WIDTH) {
			displayWidth = MAX_WIDTH;
			displayHeight = Math.round(displayWidth / aspect);
		}
		const dpr = window.devicePixelRatio ?? 1;
		canvas.width = Math.round(displayWidth * dpr);
		canvas.height = Math.round(displayHeight * dpr);
		canvas.style.width = `${displayWidth}px`;
		canvas.style.height = `${displayHeight}px`;

		const offscreen = document.createElement("canvas");
		offscreen.width = width;
		offscreen.height = height;
		const offCtx = offscreen.getContext("2d");
		if (!offCtx) return;

		const imageData = offCtx.createImageData(width, height);
		for (let col = 0; col < width; col += 1) {
			for (let row = 0; row < height; row += 1) {
				const value = specData[col * height + (height - 1 - row)] ?? 0;
				const [rFloat, gFloat, bFloat] = magmaColor(value);
				const r = Math.round(rFloat);
				const g = Math.round(gFloat);
				const b = Math.round(bFloat);
				const index = (row * width + col) * 4;
				imageData.data[index] = r;
				imageData.data[index + 1] = g;
				imageData.data[index + 2] = b;
				imageData.data[index + 3] = 255;
			}
		}
		offCtx.putImageData(imageData, 0, 0);

		ctx.save();
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.imageSmoothingEnabled = false;
		ctx.clearRect(0, 0, displayWidth, displayHeight);
		ctx.drawImage(offscreen, 0, 0, displayWidth, displayHeight);

		// Draw beat grid lines if available
		if (beatGrid) {
			// Calculate duration from beat grid (use max time from beats and downbeats)
			const allTimes = [...beatGrid.beats, ...beatGrid.downbeats];
			const duration = allTimes.length > 0 ? Math.max(...allTimes) : 0;

			if (duration > 0) {
				const scaleX = displayWidth / duration;

				// Draw beats as thin black lines
				ctx.strokeStyle = "#000000";
				ctx.lineWidth = 0.5;
				for (const beatTime of beatGrid.beats) {
					const x = beatTime * scaleX;
					ctx.beginPath();
					ctx.moveTo(x, 0);
					ctx.lineTo(x, displayHeight);
					ctx.stroke();
				}

				// Draw downbeats as very bright light blue lines
				ctx.strokeStyle = "#52e0ff"; // bright light blue
				ctx.lineWidth = 1;
				for (const downbeatTime of beatGrid.downbeats) {
					const x = downbeatTime * scaleX;
					ctx.beginPath();
					ctx.moveTo(x, 0);
					ctx.lineTo(x, displayHeight);
					ctx.stroke();
				}
			}
		}

		ctx.restore();
	}, [data.melSpec]);

	const melSpecAvailable = Boolean(data.melSpec);

	const handleScrub = React.useCallback(
		(event: React.PointerEvent<HTMLDivElement>) => {
			event.preventDefault();
		},
		[],
	);

	const body = (
		<div className="text-[11px]">
			<div
				className={`relative ${playback.hasActive ? "cursor-pointer" : "cursor-default"}`}
				onPointerDown={handleScrub}
			>
				{melSpecAvailable ? (
					<canvas
						ref={canvasRef}
						className="block bg-black"
						style={{ imageRendering: "pixelated" as const }}
						role="img"
						aria-label="Mel spectrogram"
					/>
				) : (
					<p className="text-muted-foreground">
						Send an audio signal to view its spectrogram.
					</p>
				)}
				{playback.hasActive && (
					<div
						className="pointer-events-none absolute inset-y-0 w-px bg-red-500/80"
						style={{ left: `${playback.progress * 100}%` }}
					/>
				)}
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}

// Standard node with parameter controls
export function StandardNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const controls: React.ReactNode[] = [];
	for (const param of data.definition.params) {
		if (param.paramType === "Number") {
			const value = (params[param.id] as number) ?? param.defaultNumber ?? 0;
			controls.push(
				<div key={param.id} className="px-3 pb-1">
					<label className="block text-[10px] text-gray-400 mb-1">
						{param.name}
					</label>
					<Input
						type="number"
						value={value}
						onChange={(e) => {
							const next = Number(e.target.value);
							setParam(id, param.id, Number.isFinite(next) ? next : 0);
						}}
						className="h-7 text-xs"
					/>
				</div>,
			);
		} else if (param.paramType === "Text") {
			const value = (params[param.id] as string) ?? param.defaultText ?? "";
			controls.push(
				<div key={param.id} className="px-3 pb-1">
					<label className="block text-[10px] text-gray-400 mb-1">
						{param.name}
					</label>
					<Input
						type="text"
						value={value ?? ""}
						onChange={(e) => {
							setParam(id, param.id, e.target.value);
						}}
						className="h-7 text-xs"
					/>
				</div>,
			);
		}
	}

	const paramControls =
		controls.length > 0 ? <div className="py-1">{controls}</div> : null;

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}

// Color node with color picker
export function ColorNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	// Parse color from JSON string stored in params
	const colorParam =
		(params["color"] as string) ??
		data.definition.params.find((p) => p.id === "color")?.defaultText ??
		'{"r":255,"g":0,"b":0,"a":1}';

	// Convert stored JSON to hex string for ColorPicker defaultValue
	let defaultValue = "#ff0000";
	try {
		const parsed = JSON.parse(colorParam);
		if (
			typeof parsed.r === "number" &&
			typeof parsed.g === "number" &&
			typeof parsed.b === "number"
		) {
			const r = Math.round(parsed.r).toString(16).padStart(2, "0");
			const g = Math.round(parsed.g).toString(16).padStart(2, "0");
			const b = Math.round(parsed.b).toString(16).padStart(2, "0");
			defaultValue = `#${r}${g}${b}`;
		}
	} catch {
		// Invalid JSON, use default
	}

	const handleColorChange = React.useCallback(
		(rgba: unknown) => {
			if (Array.isArray(rgba) && rgba.length >= 4) {
				const colorJson = JSON.stringify({
					r: Math.round(Number(rgba[0])),
					g: Math.round(Number(rgba[1])),
					b: Math.round(Number(rgba[2])),
					a: Number(rgba[3]),
				});
				setParam(id, "color", colorJson);
			}
		},
		[id, setParam],
	);

	const controls = (
		<div className="">
			<ColorPicker
				defaultValue={defaultValue}
				onChange={handleColorChange}
				className="max-w-md p-3"
			>
				<div className="flex flex-col gap-2">
					<ColorPickerSelection className="h-36 w-48 rounded" />
					<div className="flex gap-2">
						<ColorPickerHue className="flex-1" />
					</div>
					<ColorPickerAlpha />
				</div>
			</ColorPicker>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}

// Color palette generation utilities
function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
	r /= 255;
	g /= 255;
	b /= 255;

	const max = Math.max(r, g, b);
	const min = Math.min(r, g, b);
	let h = 0;
	let s = 0;
	const l = (max + min) / 2;

	if (max !== min) {
		const d = max - min;
		s = l > 0.5 ? d / (2 - max - min) : d / (max + min);

		switch (max) {
			case r:
				h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
				break;
			case g:
				h = ((b - r) / d + 2) / 6;
				break;
			case b:
				h = ((r - g) / d + 4) / 6;
				break;
		}
	}

	return [h * 360, s, l];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
	h = h / 360;
	let r: number, g: number, b: number;

	if (s === 0) {
		r = g = b = l;
	} else {
		const hue2rgb = (p: number, q: number, t: number) => {
			if (t < 0) t += 1;
			if (t > 1) t -= 1;
			if (t < 1 / 6) return p + (q - p) * 6 * t;
			if (t < 1 / 2) return q;
			if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
			return p;
		};

		const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
		const p = 2 * l - q;
		r = hue2rgb(p, q, h + 1 / 3);
		g = hue2rgb(p, q, h);
		b = hue2rgb(p, q, h - 1 / 3);
	}

	return [Math.round(r * 255), Math.round(g * 255), Math.round(b * 255)];
}

function rgbToHex(r: number, g: number, b: number): string {
	return `#${Math.round(r).toString(16).padStart(2, "0")}${Math.round(g).toString(16).padStart(2, "0")}${Math.round(b).toString(16).padStart(2, "0")}`;
}

function hexToRgb(hex: string): [number, number, number] {
	const normalized = hex.replace("#", "");
	const value = parseInt(normalized, 16);
	return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function adjustHexLightness(hex: string, brightness: number): string {
	const [r, g, b] = hexToRgb(hex);
	const [h, s, l] = rgbToHsl(r, g, b);
	const delta = (brightness - 0.5) * 0.4;
	const newL = Math.max(0, Math.min(1, l + delta));
	const [newR, newG, newB] = hslToRgb(h, s, newL);
	return rgbToHex(newR, newG, newB);
}

function parseColorJson(colorJson: string): [number, number, number] {
	try {
		const parsed = JSON.parse(colorJson);
		if (
			typeof parsed.r === "number" &&
			typeof parsed.g === "number" &&
			typeof parsed.b === "number"
		) {
			return [parsed.r, parsed.g, parsed.b];
		}
	} catch {
		// Invalid JSON, use default
	}
	return [255, 0, 0]; // Default red
}

function generateColorPalette(baseColorJson: string, n: number): string[] {
	const [r, g, b] = parseColorJson(baseColorJson);
	const [h, s, l] = rgbToHsl(r, g, b);

	const center = (n - 1) / 2;
	const lStep = 0.1; // Lightness step
	const hStep = 6; // Hue step in degrees

	const palette: string[] = [];

	for (let i = 0; i < n; i++) {
		const offset = i - center;
		const newL = Math.max(0, Math.min(1, l + offset * lStep));
		const newH = (((h + offset * hStep) % 360) + 360) % 360;
		const [newR, newG, newB] = hslToRgb(newH, s, newL);
		palette.push(rgbToHex(newR, newG, newB));
	}

	return palette;
}

export function HarmonyColorVisualizerNode(
	props: NodeProps<HarmonyColorVisualizerNodeData>,
) {
	const { data, id } = props;
	const playback = usePatternEntryPlayback(data.playbackSourceId);
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);

	const paletteSize = Math.max(
		2,
		Math.round((params.palette_size as number) ?? 4),
	);

	const palette = React.useMemo(() => {
		if (!data.baseColor) return null;
		return generateColorPalette(data.baseColor, paletteSize);
	}, [data.baseColor, paletteSize]);

	const currentColor = React.useMemo(() => {
		// data.seriesData now contains a color time series with palette indices
		if (!data.seriesData?.samples.length || !palette) {
			return null;
		}

		const samples = data.seriesData.samples;
		// Use currentTime if playback is active, otherwise use 0
		const currentTime = playback.hasActive ? playback.currentTime : 0;

		// Find the two samples to interpolate between
		let beforeIdx = -1;
		let afterIdx = -1;

		for (let i = 0; i < samples.length; i++) {
			if (samples[i].time <= currentTime) {
				beforeIdx = i;
			}
			if (samples[i].time >= currentTime && afterIdx === -1) {
				afterIdx = i;
				break;
			}
		}

		const getSampleValues = (idx: number) => {
			const values = samples[idx]?.values ?? [];
			return {
				paletteValue: values[0] ?? 0,
				brightnessValue: values[1] ?? 0.5,
			};
		};

		let paletteIdx = 0;
		let brightnessValue = 0.5;

		// If before the first sample, use first sample
		if (beforeIdx === -1) {
			const { paletteValue, brightnessValue: bright } = getSampleValues(0);
			paletteIdx = Math.round(paletteValue);
			brightnessValue = bright;
		}
		// If after the last sample, use last sample
		else if (afterIdx === -1) {
			const { paletteValue, brightnessValue: bright } = getSampleValues(
				samples.length - 1,
			);
			paletteIdx = Math.round(paletteValue);
			brightnessValue = bright;
		}
		// Interpolate between samples
		else {
			const beforeSample = samples[beforeIdx];
			const afterSample = samples[afterIdx];
			const timeRange = afterSample.time - beforeSample.time;
			const t =
				timeRange > 0 ? (currentTime - beforeSample.time) / timeRange : 0;

			const beforeIdxValue = beforeSample.values[0] ?? 0;
			const afterIdxValue = afterSample.values[0] ?? 0;
			const interpolatedIdx =
				beforeIdxValue + (afterIdxValue - beforeIdxValue) * t;
			paletteIdx = Math.round(interpolatedIdx);

			const beforeBrightness = beforeSample.values[1] ?? 0.5;
			const afterBrightness = afterSample.values[1] ?? 0.5;
			brightnessValue =
				beforeBrightness + (afterBrightness - beforeBrightness) * t;
		}

		// Clamp palette index and map to color
		const clampedIdx = Math.max(0, Math.min(palette.length - 1, paletteIdx));
		const clampedBrightness = Math.max(0, Math.min(1, brightnessValue ?? 0.5));
		return adjustHexLightness(palette[clampedIdx], clampedBrightness);
	}, [data.seriesData, palette, playback.currentTime, playback.hasActive]);

	const displayColor =
		currentColor ??
		(palette && palette.length > 0
			? palette[Math.floor(palette.length / 2)]
			: "#808080");

	const body = (
		<div className="px-2 pb-2 space-y-2">
			<div
				className="w-full aspect-square rounded border-2 border-border"
				style={{
					backgroundColor: displayColor,
					minHeight: "120px",
					transition: "background-color 0.1s ease-out",
				}}
			/>
			{!data.seriesData && (
				<p className="text-[10px] text-slate-500 text-center">
					Connect harmony series input
				</p>
			)}
			{!data.baseColor && (
				<p className="text-[10px] text-slate-500 text-center">
					Connect base color input
				</p>
			)}
			{data.seriesData && data.baseColor && !playback.hasActive && (
				<p className="text-[10px] text-slate-400 text-center">
					Play audio to see color changes
				</p>
			)}
			{palette && palette.length > 0 && (
				<div className="flex gap-1">
					{palette.map((color) => (
						<div
							key={color}
							className="flex-1 h-4 rounded border border-border"
							style={{ backgroundColor: color }}
							title={color}
						/>
					))}
				</div>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}
