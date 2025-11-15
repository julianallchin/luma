import * as React from "react";
import { Handle, Position, NodeProps } from "reactflow";
import { Input } from "@/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import { useTracksStore } from "@/useTracksStore";
import { useGraphStore } from "@/useGraphStore";
import { usePatternPlaybackStore } from "@/usePatternPlaybackStore";
import type {
	BaseNodeData,
	ViewChannelNodeData,
	MelSpecNodeData,
	PatternEntryNodeData,
} from "./types";

// BaseNode component that auto-renders handles
export function BaseNode<T extends BaseNodeData>(props: NodeProps<T>) {
	const { data } = props;

	return (
		<div className="relative bg-muted text-muted-foreground text-xs text-gray-100 border border-border overflow-hidden min-w-[170px] rounded">
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
const DISABLED_PLAYBACK = {
	progress: 0,
	duration: 0,
	hasActive: false,
} as const;

const usePlaybackProgress = () => DISABLED_PLAYBACK;

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
	const playback = usePlaybackProgress();

	const limited = React.useMemo(
		() => (data.viewSamples ?? []).slice(0, VIEW_SAMPLE_LIMIT),
		[data.viewSamples],
	);

	// Draw waveform on canvas
	React.useEffect(() => {
		const canvas = canvasRef.current;
		if (!canvas) return;

		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const width = canvas.width;
		const height = canvas.height;

		// Clear canvas
		ctx.clearRect(0, 0, width, height);

		if (limited.length === 0) return;

		// Draw background
		// ctx.fillStyle = "rgba(30, 41, 59, 0.6)";
		ctx.fillRect(0, 0, width, height);

		// Draw waveform
		ctx.strokeStyle = "#34d399"; // emerald-400
		ctx.lineWidth = 2;
		ctx.lineJoin = "round";
		ctx.beginPath();

		const denom = Math.max(1, limited.length - 1);
		const padding = 5;
		const drawHeight = height - padding * 2;

		for (let i = 0; i < limited.length; i++) {
			const value = limited[i] ?? 0;
			const clamped = Math.max(0, Math.min(1, value));
			const x = (i / denom) * (width - padding * 2) + padding;
			const y = height - padding - clamped * drawHeight;

			if (i === 0) {
				ctx.moveTo(x, y);
			} else {
				ctx.lineTo(x, y);
			}
		}

		ctx.stroke();
	}, [limited]);

	const handleScrub = React.useCallback(
		(event: React.PointerEvent<HTMLDivElement>) => {
			event.preventDefault();
		},
		[],
	);

	const body = (
		<div className="">
			<div
				className={`relative bg-background text-[11px] ${playback.hasActive ? "cursor-pointer" : "cursor-default"}`}
				onPointerDown={handleScrub}
			>
				{limited.length > 0 ? (
					<canvas
						ref={canvasRef}
						width={300}
						height={96}
						className="w-full"
						role="img"
						aria-label="Intensity preview waveform"
					/>
				) : (
					<p className="text-center text-[11px] text-slate-400">
						waiting for signal…
					</p>
				)}
				{playback.hasActive && (
					<div
						className="pointer-events-none absolute inset-y-1 w-px bg-white/80"
						style={{ left: `${playback.progress * 100}%` }}
					/>
				)}
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}

export function PatternEntryNode(props: NodeProps<PatternEntryNodeData>) {
	const { id, data } = props;
	const entry = data.patternEntry ?? null;
	const durationSeconds = entry?.durationSeconds ?? 0;
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
			<div className="relative h-3 rounded bg-slate-800" />
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
	const playback = usePlaybackProgress();

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
						className="pointer-events-none absolute inset-y-0 w-px bg-white/80"
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
