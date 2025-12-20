import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
	Loader2,
	Pause,
	Pencil,
	Play,
	Repeat,
	Save,
	SkipBack,
	Trash2,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import type {
	BeatGrid,
	Graph,
	GraphContext,
	HostAudioSnapshot,
	MelSpec,
	NodeTypeDef,
	PatternArgDef,
	PatternSummary,
	Signal,
	TrackSummary,
} from "@/bindings/schema";
import {
	type PatternAnnotationInstance,
	PatternAnnotationProvider,
} from "@/features/patterns/contexts/pattern-annotation-context";
import { useHostAudioStore } from "@/features/patterns/stores/use-host-audio-store";
import type {
	TrackAnnotation,
	TrackWaveform,
} from "@/features/track-editor/stores/use-track-editor-store";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import {
	Dialog,
	DialogContent,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";
import { formatTime } from "@/shared/lib/react-flow/base-node";
import {
	type EditorController,
	ReactFlowEditorWrapper,
} from "@/shared/lib/react-flow-editor";

type RunResult = {
	views: Record<string, Signal>;
	melSpecs: Record<string, MelSpec>;
	colorViews: Record<string, string>;
	universeState?: unknown;
};

const REQUIRED_NODE_TYPES = ["audio_input", "beat_clock"] as const;
const LEGACY_NODE_TYPES = new Set([
	"audio_source",
	"pattern_entry",
	"beat_crop",
]);

function sanitizeGraph(graph: Graph): Graph {
	const prunedNodes = graph.nodes.filter(
		(node) => !LEGACY_NODE_TYPES.has(node.typeId),
	);
	const removedIds = new Set(
		graph.nodes
			.filter((node) => LEGACY_NODE_TYPES.has(node.typeId))
			.map((node) => node.id),
	);
	const remainingIds = new Set(prunedNodes.map((n) => n.id));
	const filteredEdges = graph.edges.filter(
		(edge) =>
			!removedIds.has(edge.fromNode) &&
			!removedIds.has(edge.toNode) &&
			remainingIds.has(edge.fromNode) &&
			remainingIds.has(edge.toNode),
	);

	const ensureNode = (
		nodes: typeof prunedNodes,
		typeId: (typeof REQUIRED_NODE_TYPES)[number],
		position: { x: number; y: number },
	) => {
		const exists = nodes.some((n) => n.typeId === typeId);
		if (exists) return nodes;
		let counter = 1;
		let id = `${typeId}-${counter}`;
		while (remainingIds.has(id)) {
			counter += 1;
			id = `${typeId}-${counter}`;
		}
		remainingIds.add(id);
		return [
			...nodes,
			{
				id,
				typeId,
				params: {},
				positionX: position.x,
				positionY: position.y,
			},
		];
	};

	const withAudio = ensureNode(prunedNodes, "audio_input", { x: 0, y: 0 });
	const withBeat = ensureNode(withAudio, "beat_clock", { x: 240, y: 0 });

	return {
		nodes: withBeat,
		edges: filteredEdges,
		args: graph.args ?? [],
	};
}

function ensureRequiredNodes(graph: Graph): Graph {
	const existing = new Set(graph.nodes.map((n) => n.typeId));
	let nodes = graph.nodes.slice();

	const ensure = (
		typeId: (typeof REQUIRED_NODE_TYPES)[number],
		position: { x: number; y: number },
	) => {
		if (existing.has(typeId)) return;
		const idBase = typeId.replace("_", "-");
		let counter = 1;
		let id = `${idBase}-${counter}`;
		const idSet = new Set(nodes.map((n) => n.id));
		while (idSet.has(id)) {
			counter += 1;
			id = `${idBase}-${counter}`;
		}
		nodes = [
			...nodes,
			{
				id,
				typeId,
				params: {},
				positionX: position.x,
				positionY: position.y,
			},
		];
		existing.add(typeId);
	};

	ensure("audio_input", { x: 0, y: 0 });
	ensure("beat_clock", { x: 240, y: 0 });

	return {
		...graph,
		nodes,
	};
}

function withPatternArgsNode(graph: Graph, args: PatternArgDef[]): Graph {
	const hasArgs = args.length > 0;
	const filteredEdges = hasArgs
		? graph.edges
		: graph.edges.filter(
				(edge) =>
					edge.fromNode !== "pattern_args" && edge.toNode !== "pattern_args",
			);

	let nodes = hasArgs
		? graph.nodes
		: graph.nodes.filter((node) => node.typeId !== "pattern_args");
	const hasNode = nodes.some((n) => n.typeId === "pattern_args");

	if (hasArgs && !hasNode) {
		nodes = [
			...nodes,
			{
				id: "pattern_args",
				typeId: "pattern_args",
				params: {},
				positionX: -320,
				positionY: -120,
			},
		];
	}

	// Filter edges from pattern_args that refer to non-existent args
	const validArgIds = new Set(args.map((a) => a.id));
	const cleanedEdges = filteredEdges.filter((edge) => {
		if (edge.fromNode === "pattern_args") {
			return validArgIds.has(edge.fromPort);
		}
		return true;
	});

	return {
		...graph,
		nodes,
		edges: cleanedEdges,
		args,
	};
}

function computeBarRangeLabel(
	start: number,
	end: number,
	beatGrid: BeatGrid | null,
): string {
	if (!beatGrid) return "Bars —";
	const barDuration = (60 / beatGrid.bpm) * beatGrid.beatsPerBar;
	const offset = beatGrid.downbeatOffset ?? 0;
	const startBar = Math.max(1, Math.floor((start - offset) / barDuration) + 1);
	const endBar = Math.max(
		startBar,
		Math.floor((end - offset) / barDuration) + 1,
	);
	return `Bars ${startBar}–${endBar}`;
}

type MiniWaveformPreviewProps = {
	waveform: TrackWaveform | null;
	startTime: number;
	endTime: number;
};

function MiniWaveformPreview({
	waveform,
	startTime,
	endTime,
}: MiniWaveformPreviewProps) {
	const canvasRef = useRef<HTMLCanvasElement | null>(null);

	useEffect(() => {
		const canvas = canvasRef.current;
		if (!canvas) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const width = canvas.clientWidth || 240;
		const height = canvas.clientHeight || 56;
		const dpr = window.devicePixelRatio || 1;
		if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
			canvas.width = width * dpr;
			canvas.height = height * dpr;
			canvas.style.width = `${width}px`;
			canvas.style.height = `${height}px`;
			ctx.scale(dpr, dpr);
		}

		ctx.clearRect(0, 0, width, height);

		const totalDuration = waveform?.durationSeconds ?? 0;
		const windowStart = Math.max(0, startTime);
		const windowEnd =
			endTime > 0 ? Math.min(endTime, totalDuration) : totalDuration;
		const windowDuration = Math.max(0.001, windowEnd - windowStart);

		const BLUE = [0, 85, 226];
		const ORANGE = [242, 170, 60];
		const WHITE = [255, 255, 255];

		if (waveform?.bands) {
			const { low, mid, high } = waveform.bands;
			const numBuckets = low.length;
			for (let x = 0; x < width; x += 1) {
				const t = windowStart + (windowDuration * x) / width;
				const bucketIdx = Math.min(
					numBuckets - 1,
					Math.floor((t / totalDuration) * numBuckets),
				);
				const lowH = Math.floor(low[bucketIdx] * (height / 2));
				const midH = Math.floor(mid[bucketIdx] * (height / 2));
				const highH = Math.floor(high[bucketIdx] * (height / 2));
				const centerY = height / 2;

				if (lowH > 0) {
					ctx.fillStyle = `rgb(${BLUE[0]}, ${BLUE[1]}, ${BLUE[2]})`;
					ctx.fillRect(x, centerY - lowH, 1, lowH * 2);
				}
				if (midH > 0) {
					ctx.fillStyle = `rgb(${ORANGE[0]}, ${ORANGE[1]}, ${ORANGE[2]})`;
					ctx.fillRect(x, centerY - midH, 1, midH * 2);
				}
				if (highH > 0) {
					ctx.fillStyle = `rgb(${WHITE[0]}, ${WHITE[1]}, ${WHITE[2]})`;
					ctx.fillRect(x, centerY - highH, 1, highH * 2);
				}
			}
		} else if (waveform?.fullSamples?.length) {
			const samples = waveform.fullSamples;
			const numBuckets = samples.length / 2;
			ctx.fillStyle = "rgba(94, 234, 212, 0.6)";
			for (let x = 0; x < width; x += 1) {
				const t = windowStart + (windowDuration * x) / width;
				const bucketIndex = Math.floor((t / totalDuration) * numBuckets) * 2;
				const min = samples[bucketIndex] ?? 0;
				const max = samples[bucketIndex + 1] ?? 0;
				const yTop = height / 2 - max * (height / 2) * 0.9;
				const yBottom = height / 2 - min * (height / 2) * 0.9;
				const h = Math.abs(yBottom - yTop) || 1;
				ctx.fillRect(x, Math.min(yTop, yBottom), 1, h);
			}
		} else {
			ctx.fillStyle = "rgba(255,255,255,0.05)";
			for (let i = 0; i < width; i += 6) {
				const h = (Math.sin(i / 10) * 0.5 + 0.5) * height * 0.3 + 8;
				ctx.fillRect(i, height / 2 - h / 2, 3, h);
			}
		}
	}, [waveform, startTime, endTime]);

	return <canvas ref={canvasRef} className="w-full h-14 bg-transparent" />;
}

type ContextSidebarProps = {
	instances: PatternAnnotationInstance[];
	loading: boolean;
	error: string | null;
	selectedId: number | null;
	onSelect: (id: number) => void;
	onReload: () => void;
};

function ContextSidebar({
	instances,
	loading,
	error,
	selectedId,
	onSelect,
	onReload,
}: ContextSidebarProps) {
	return (
		<aside className="w-96 border-r border-border bg-card flex flex-col min-h-0">
			<div className="px-4 py-3 border-b border-border flex items-center justify-between bg-background">
				<div>
					<p className="text-xs font-semibold uppercase tracking-wide text-foreground">
						Context
					</p>
					<p className="text-[11px] text-muted-foreground">
						Track sections annotated with this pattern
					</p>
				</div>
				<button
					type="button"
					onClick={onReload}
					disabled={loading}
					className="text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-50"
				>
					Refresh
				</button>
			</div>
			<div className="flex-1 overflow-y-auto p-3 space-y-3">
				{error ? <div className="text-sm text-destructive">{error}</div> : null}
				{loading ? (
					<div className="space-y-2">
						<div className="h-14 bg-muted animate-pulse" />
						<div className="h-14 bg-muted animate-pulse" />
					</div>
				) : null}
				{!loading && instances.length === 0 ? (
					<div className="text-sm text-muted-foreground">
						Click a track and add this pattern on the timeline to create an
						instance to edit.
					</div>
				) : null}
				{instances.map((instance) => {
					const isActive = instance.id === selectedId;
					const barLabel = computeBarRangeLabel(
						instance.startTime,
						instance.endTime,
						instance.beatGrid,
					);
					const timeLabel = `${formatTime(instance.startTime)} – ${formatTime(
						instance.endTime,
					)}`;
					return (
						<button
							type="button"
							key={instance.id}
							onClick={() => onSelect(instance.id)}
							className={`w-full text-left rounded-lg border transition-colors ${
								isActive
									? "border-primary/70 bg-primary/10"
									: "border-border/60 bg-input hover:border-border hover:bg-muted shadow"
							}`}
						>
							<div className="px-3 py-2 flex items-center gap-3">
								{instance.track.albumArtData ? (
									<img
										src={instance.track.albumArtData}
										alt=""
										className="h-12 w-12 object-cover bg-muted/50 rounded"
									/>
								) : (
									<div className="h-12 w-12 bg-muted/60" />
								)}
								<div className="min-w-0 flex-1">
									<div className="flex items-center justify-between text-[11px] text-foreground gap-2">
										<span className="font-semibold truncate text-sm">
											{instance.track.title ?? `Track ${instance.track.id}`}
										</span>
										<span className="text-[10px] text-muted-foreground whitespace-nowrap">
											{barLabel}
										</span>
									</div>
									<div className="text-[10px] text-muted-foreground ">
										{timeLabel}
									</div>
								</div>
							</div>
							<div className="">
								<MiniWaveformPreview
									waveform={instance.waveform}
									startTime={instance.startTime}
									endTime={instance.endTime}
								/>
							</div>
						</button>
					);
				})}
			</div>
		</aside>
	);
}

type TransportBarProps = {
	beatGrid: BeatGrid | null;
	segmentDuration: number;
};

function secondsToBeats(seconds: number, grid: BeatGrid | null): number | null {
	if (!grid || grid.bpm === 0) return null;
	const beatLength = 60 / grid.bpm;
	return (seconds - grid.downbeatOffset) / beatLength;
}

function secondsToBeatsRelative(
	seconds: number,
	grid: BeatGrid | null,
	segmentStart: number,
): number | null {
	const absoluteBeat = secondsToBeats(seconds, grid);
	if (absoluteBeat === null) return null;
	const segmentStartBeat = secondsToBeats(segmentStart, grid) ?? 0;
	return absoluteBeat - segmentStartBeat;
}

function sliceBeatGrid(grid: BeatGrid | null, start: number, end: number) {
	if (!grid) return null;
	// Don't shift beats - keep absolute time for backend compatibility
	const beats = grid.beats.filter((t) => t >= start && t <= end);
	const downbeats = grid.downbeats.filter((t) => t >= start && t <= end);
	return {
		...grid,
		beats,
		downbeats,
		// downbeatOffset remains absolute
	};
}

type PatternInfoPanelProps = {
	pattern: PatternSummary | null;
	loading: boolean;
	args: PatternArgDef[];
	onAddArg: () => void;
	onEditArg: (arg: PatternArgDef) => void;
	onDeleteArg: (argId: string) => void;
};

function PatternInfoPanel({
	pattern,
	loading,
	args,
	onAddArg,
	onEditArg,
	onDeleteArg,
}: PatternInfoPanelProps) {
	if (loading) {
		return (
			<div className="w-96 bg-background border-l flex flex-col">
				<div className="px-4 py-3 border-b border-border bg-background">
					<div className="h-5 w-32 bg-muted animate-pulse rounded" />
				</div>
				<div className="p-4 space-y-3">
					<div className="h-4 w-full bg-muted animate-pulse rounded" />
					<div className="h-4 w-3/4 bg-muted animate-pulse rounded" />
				</div>
			</div>
		);
	}

	if (!pattern) {
		return (
			<div className="w-96 bg-background border-l flex flex-col">
				<div className="px-4 py-3 border-b border-border bg-background">
					<p className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
						Pattern Info
					</p>
				</div>
				<div className="p-4 text-sm text-muted-foreground">
					Pattern not found
				</div>
			</div>
		);
	}

	return (
		<div className="w-96 bg-background border-l flex flex-col">
			<div className="px-4 py-3 border-b border-border bg-background">
				<p className="text-xs font-semibold uppercase tracking-wide text-foreground">
					Pattern Info
				</p>
			</div>
			<div className="p-4 space-y-4">
				<div>
					<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
						Name
					</span>
					<h2 className="text-lg font-semibold text-foreground mt-0.5">
						{pattern.name}
					</h2>
				</div>

				<div>
					<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
						Description
					</span>
					<p className="text-sm text-foreground/80 mt-0.5 leading-relaxed">
						{pattern.description || (
							<span className="text-muted-foreground italic">
								No description provided
							</span>
						)}
					</p>
				</div>

				<div className="pt-2 border-t border-border">
					<div className="flex items-center justify-between">
						<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
							Args
						</span>
						<button
							type="button"
							onClick={onAddArg}
							className="text-xs text-primary hover:underline"
						>
							Add Arg
						</button>
					</div>
					{args.length === 0 ? (
						<p className="text-sm text-muted-foreground mt-1">No args yet</p>
					) : (
						<div className="mt-2 space-y-2">
							{args.map((arg) => (
								<div
									key={arg.id}
									className="flex items-center justify-between text-sm group"
								>
									<div className="flex flex-col">
										<span className="text-foreground">{arg.name}</span>
										<span className="text-[11px] text-muted-foreground uppercase">
											{arg.argType}
										</span>
									</div>
									<div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
										<button
											type="button"
											onClick={() => onEditArg(arg)}
											className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted rounded"
											title="Edit argument"
										>
											<Pencil size={12} />
										</button>
										<button
											type="button"
											onClick={() => onDeleteArg(arg.id)}
											className="p-1.5 text-muted-foreground hover:text-destructive hover:bg-red-500/10 rounded"
											title="Delete argument"
										>
											<Trash2 size={12} />
										</button>
									</div>
								</div>
							))}
						</div>
					)}
				</div>

				<div className="pt-2 border-t border-border">
					<div className="flex items-center justify-between text-[10px] text-muted-foreground">
						<span>Created</span>
						<span>
							{new Date(pattern.createdAt).toLocaleDateString(undefined, {
								year: "numeric",
								month: "short",
								day: "numeric",
							})}
						</span>
					</div>
					<div className="flex items-center justify-between text-[10px] text-muted-foreground mt-1">
						<span>Updated</span>
						<span>
							{new Date(pattern.updatedAt).toLocaleDateString(undefined, {
								year: "numeric",
								month: "short",
								day: "numeric",
							})}
						</span>
					</div>
				</div>
			</div>
		</div>
	);
}

function TransportBar({
	beatGrid,
	segmentDuration,
	startTime,
}: TransportBarProps & { startTime: number }) {
	const isPlaying = useHostAudioStore((s) => s.isPlaying);
	const currentTime = useHostAudioStore((s) => s.currentTime);
	const durationSeconds = useHostAudioStore((s) => s.durationSeconds);
	const loopEnabled = useHostAudioStore((s) => s.loopEnabled);
	const [scrubValue, setScrubValue] = useState<number | null>(null);
	const scrubberRef = useRef<HTMLDivElement>(null);
	const displayTime = scrubValue ?? currentTime;
	const total = Math.max(durationSeconds, 0.0001);
	const progress = (displayTime / total) * 100;

	// Calculate beat position relative to the segment start
	const absoluteTime = startTime + displayTime;
	const beatPosition = secondsToBeatsRelative(
		absoluteTime,
		beatGrid,
		startTime,
	);

	const totalBeats =
		beatGrid && beatGrid.bpm > 0
			? (segmentDuration || durationSeconds) / (60 / beatGrid.bpm)
			: null;

	const handleSeek = async (value: number) => {
		setScrubValue(null);
		await useHostAudioStore.getState().seek(value);
	};

	const handleScrub = (e: React.MouseEvent<HTMLDivElement>) => {
		if (!scrubberRef.current) return;
		const rect = scrubberRef.current.getBoundingClientRect();
		const x = e.clientX - rect.left;
		const percentage = Math.max(0, Math.min(1, x / rect.width));
		const newTime = percentage * total;
		setScrubValue(newTime);
		handleSeek(newTime);
	};

	const handlePlayPause = async () => {
		const hostAudio = useHostAudioStore.getState();
		if (hostAudio.isPlaying) {
			await hostAudio.pause();
		} else if (hostAudio.isLoaded) {
			await hostAudio.play();
		}
	};

	return (
		<div className="border-t border-border bg-background/80">
			{/* Scrubber Bar */}
			<div
				ref={scrubberRef}
				role="slider"
				aria-valuemin={0}
				aria-valuemax={total}
				aria-valuenow={displayTime}
				aria-label="Playback position"
				className="h-3 w-full bg-background border-b cursor-pointer group relative overflow-hidden focus:outline-none"
				onMouseDown={(e) => {
					handleScrub(e);
				}}
				onMouseMove={(e) => {
					if (e.buttons === 1) handleScrub(e);
				}}
				tabIndex={0}
			>
				{/* Progress Fill */}
				<div
					className="absolute top-0 bottom-0 left-0 bg-primary/20 transition-all duration-75 ease-linear border-r border-primary"
					style={{ width: `${progress}%` }}
				/>

				{/* Hover Indicator */}
				<div className="absolute inset-0 bg-white/5 opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none" />
			</div>

			{/* Controls */}
			<div className="flex items-center p-2 justify-between">
				<div className="text-[11px] text-muted-foreground w-36">
					{formatTime(displayTime)} / {formatTime(durationSeconds)}
					{beatPosition !== null && totalBeats !== null ? (
						<span className="ml-2 text-[10px] text-foreground/70">
							Beat {(beatPosition + 1).toFixed(1)} / {totalBeats.toFixed(1)}
						</span>
					) : null}
				</div>

				<div className="flex items-center gap-4">
					<button
						type="button"
						onClick={() => handleSeek(0)}
						className="p-2 text-muted-foreground hover:text-foreground rounded-full hover:bg-muted transition-colors"
					>
						<SkipBack size={16} />
					</button>
					<button
						type="button"
						onClick={handlePlayPause}
						className="w-10 h-10 bg-white text-black rounded-full flex items-center justify-center hover:scale-105 transition-transform"
					>
						{isPlaying ? (
							<Pause className="h-5 w-5" fill="currentColor" />
						) : (
							<Play className="h-5 w-5 ml-0.5" fill="currentColor" />
						)}
					</button>
					<button
						type="button"
						className={`p-2 rounded-full transition-colors ${
							loopEnabled
								? "text-primary bg-primary/10"
								: "text-muted-foreground hover:text-foreground hover:bg-muted"
						}`}
						title="Toggle Loop"
						onClick={() => useHostAudioStore.getState().setLoop(!loopEnabled)}
					>
						<Repeat size={16} />
					</button>
				</div>

				<div className="w-36"></div>
			</div>
		</div>
	);
}

type PatternEditorProps = {
	patternId: number;
	nodeTypes: NodeTypeDef[];
};

export function PatternEditor({ patternId, nodeTypes }: PatternEditorProps) {
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const [graphError, setGraphError] = useState<string | null>(null);
	const [loadedGraph, setLoadedGraph] = useState<Graph | null>(null);
	const [editorReady, setEditorReady] = useState(false);
	const [isSaving, setIsSaving] = useState(false);
	const [isBuildingGraph, setIsBuildingGraph] = useState(false);
	const [instances, setInstances] = useState<PatternAnnotationInstance[]>([]);
	const [instancesLoading, setInstancesLoading] = useState(false);
	const [instancesError, setInstancesError] = useState<string | null>(null);
	const [selectedInstanceId, setSelectedInstanceId] = useState<number | null>(
		null,
	);
	const [pattern, setPattern] = useState<PatternSummary | null>(null);
	const [patternLoading, setPatternLoading] = useState(true);
	const [patternArgs, setPatternArgs] = useState<PatternArgDef[]>([]);
	const [argDialogOpen, setArgDialogOpen] = useState(false);
	const [editingArgId, setEditingArgId] = useState<string | null>(null);
	const [newArgName, setNewArgName] = useState("");
	const [newArgColor, setNewArgColor] = useState("#ff0000");
	const [newArgScalar, setNewArgScalar] = useState(1.0);
	const [newArgType, setNewArgType] = useState<"Color" | "Scalar">("Color");
	const hostCurrentTime = useHostAudioStore((s) => s.currentTime);
	const selectedInstance = useMemo(
		() => instances.find((inst) => inst.id === selectedInstanceId) ?? null,
		[instances, selectedInstanceId],
	);
	const renderAudioTime =
		selectedInstance && Number.isFinite(hostCurrentTime)
			? selectedInstance.startTime + hostCurrentTime
			: hostCurrentTime;
	useEffect(() => {
		if (selectedInstance) {
			setGraphError(null);
		}
	}, [selectedInstance]);

	const navigate = useNavigate();
	const editorRef = useRef<EditorController | null>(null);
	const pendingRunId = useRef(0);
	const goBack = useCallback(() => navigate(-1), [navigate]);
	const hasHydratedGraphRef = useRef(false);
	const lastPatternArgsHashRef = useRef<string | null>(null);
	const patternArgsNodeDef = useMemo<NodeTypeDef | null>(() => {
		if (patternArgs.length === 0) return null;
		return {
			id: "pattern_args",
			name: "Pattern Args",
			description: "Arguments provided by track annotations.",
			category: "Input",
			inputs: [],
			outputs: patternArgs.map((arg) => ({
				id: arg.id,
				name: arg.name,
				portType: "Signal",
			})),
			params: [],
		};
	}, [patternArgs]);
	const getNodeDefinitions = useCallback(() => {
		const base = nodeTypes;
		return patternArgsNodeDef ? [...base, patternArgsNodeDef] : base;
	}, [nodeTypes, patternArgsNodeDef]);

	const loadInstances = useCallback(async () => {
		setInstancesLoading(true);
		setInstancesError(null);
		try {
			const tracks = await invoke<TrackSummary[]>("list_tracks");
			const collected: PatternAnnotationInstance[] = [];

			for (const track of tracks) {
				let annotations: TrackAnnotation[] = [];
				try {
					annotations = await invoke<TrackAnnotation[]>("list_annotations", {
						trackId: track.id,
					});
				} catch (err) {
					console.error(
						`[PatternEditor] Failed to load annotations for track ${track.id}`,
						err,
					);
				}
				const matching = annotations.filter(
					(ann) => ann.patternId === patternId,
				);
				if (matching.length === 0) continue;

				const [beatGrid, waveform] = await Promise.all([
					invoke<BeatGrid | null>("get_track_beats", {
						trackId: track.id,
					}).catch((err) => {
						console.error(
							`[PatternEditor] Failed to load beat grid for track ${track.id}`,
							err,
						);
						return null;
					}),
					invoke<TrackWaveform | null>("get_track_waveform", {
						trackId: track.id,
					}).catch((err) => {
						console.error(
							`[PatternEditor] Failed to load waveform for track ${track.id}`,
							err,
						);
						return null;
					}),
				]);

				for (const ann of matching) {
					const windowedGrid = sliceBeatGrid(
						beatGrid,
						ann.startTime,
						ann.endTime,
					);
					collected.push({
						...ann,
						track,
						beatGrid: windowedGrid,
						waveform,
					});
				}
			}

			setInstances(collected);
			if (collected.length > 0) {
				setSelectedInstanceId((prev) => prev ?? collected[0].id);
			}
		} catch (err) {
			console.error("[PatternEditor] Failed to load context instances", err);
			setInstances([]);
			setInstancesError(
				err instanceof Error ? err.message : String(err ?? "Failed to load"),
			);
		} finally {
			setInstancesLoading(false);
		}
	}, [patternId]);

	useEffect(() => {
		// Ensure fixtures are loaded for the visualizer
		useFixtureStore.getState().initialize();
	}, []);

	// Load pattern metadata
	useEffect(() => {
		let active = true;
		setPatternLoading(true);

		invoke<PatternSummary>("get_pattern", { id: patternId })
			.then((p) => {
				if (active) {
					setPattern(p);
				}
			})
			.catch((err) => {
				console.error("[PatternEditor] Failed to load pattern", err);
			})
			.finally(() => {
				if (active) {
					setPatternLoading(false);
				}
			});

		return () => {
			active = false;
		};
	}, [patternId]);

	useEffect(() => {
		loadInstances();
	}, [loadInstances]);

	useEffect(() => {
		if (
			selectedInstanceId !== null &&
			instances.some((inst) => inst.id === selectedInstanceId)
		) {
			return;
		}
		if (instances.length > 0) {
			setSelectedInstanceId(instances[0].id);
		}
	}, [instances, selectedInstanceId]);

	// Subscribe to host audio state broadcasts
	useEffect(() => {
		let unsub: (() => void) | null = null;
		let cancelled = false;
		const store = useHostAudioStore;
		const handleSnapshot = (snapshot: HostAudioSnapshot) => {
			store.getState().handleSnapshot(snapshot);
		};
		const reset = () => store.getState().reset();

		listen<HostAudioSnapshot>("host-audio://state", (event) => {
			handleSnapshot(event.payload);
		})
			.then((unlisten) => {
				if (cancelled) {
					unlisten();
				} else {
					unsub = unlisten;
				}
			})
			.catch((err) => {
				console.error(
					"[PatternEditor] Failed to subscribe to host audio state",
					err,
				);
			});

		invoke<HostAudioSnapshot>("host_snapshot")
			.then((snapshot) => {
				if (!cancelled) {
					handleSnapshot(snapshot);
				}
			})
			.catch((err) => {
				console.error(
					"[PatternEditor] Failed to fetch host audio snapshot",
					err,
				);
			});

		return () => {
			cancelled = true;
			if (unsub) {
				unsub();
			}
			reset();
		};
	}, []);

	const updateViewResults = useCallback(
		(
			views: Record<string, Signal>,
			melSpecs: Record<string, MelSpec>,
			colorViews: Record<string, string>,
		) => {
			if (!editorRef.current) return;
			editorRef.current.updateViewData(views, melSpecs, colorViews);
		},
		[],
	);

	const executeGraph = useCallback(
		async (graph: Graph) => {
			if (!selectedInstance) {
				// Don't error when no context is selected; just skip execution.
				setGraphError(null);
				setIsBuildingGraph(false);
				return;
			}

			if (graph.nodes.length === 0) {
				setGraphError(null);
				await updateViewResults({}, {}, {});
				setIsBuildingGraph(false);
				return;
			}

			const runId = ++pendingRunId.current;
			setIsBuildingGraph(true);

			try {
				const ensuredGraph = ensureRequiredNodes(graph);
				// Context is now passed separately from the graph
				// The graph stays pure (no track-specific params injected)
				const defaultArgValues = Object.fromEntries(
					(patternArgs ?? []).map((arg) => [arg.id, arg.defaultValue ?? {}]),
				);
				const instanceArgs =
					(selectedInstance.args as Record<string, unknown> | undefined) ?? {};
				const mergedArgValues = { ...defaultArgValues, ...instanceArgs };
				const context: GraphContext = {
					trackId: selectedInstance.track.id,
					startTime: selectedInstance.startTime,
					endTime: selectedInstance.endTime,
					beatGrid: selectedInstance.beatGrid,
					argValues: mergedArgValues,
				};

				const result = await invoke<RunResult>("run_graph", {
					graph: ensuredGraph,
					context,
				});
				if (runId !== pendingRunId.current) return;

				setGraphError(null);
				await updateViewResults(
					result.views ?? {},
					result.melSpecs ?? {},
					result.colorViews ?? {},
				);
			} catch (err) {
				if (runId !== pendingRunId.current) return;
				console.error("Failed to execute graph", err);
				setGraphError(err instanceof Error ? err.message : String(err));
			} finally {
				if (runId === pendingRunId.current) {
					setIsBuildingGraph(false);
				}
			}
		},
		[updateViewResults, selectedInstance, patternArgs],
	);

	// Load host audio segment when instance changes
	useEffect(() => {
		if (!selectedInstance) return;

		// Load the audio segment into host audio state for playback
		useHostAudioStore
			.getState()
			.loadSegment(
				selectedInstance.track.id,
				selectedInstance.startTime,
				selectedInstance.endTime,
				selectedInstance.beatGrid,
			)
			.catch((err) => {
				console.error("[PatternEditor] Failed to load audio segment", err);
			});
	}, [selectedInstance]);

	useEffect(() => {
		if (!editorReady || !selectedInstance) return;

		// Update visual context on nodes
		if (editorRef.current) {
			const trackName =
				selectedInstance.track.title ??
				selectedInstance.track.filePath ??
				"Track";
			const timeLabel = `${formatTime(selectedInstance.startTime)} – ${formatTime(
				selectedInstance.endTime,
			)}`;
			const bpmLabel = selectedInstance.beatGrid
				? `${Math.round(selectedInstance.beatGrid.bpm * 100) / 100} BPM`
				: "--";

			editorRef.current.updateNodeContext({
				trackName,
				timeLabel,
				bpmLabel,
			});
		}

		const graph = editorRef.current?.serialize();
		if (graph) {
			executeGraph(graph);
		}
	}, [selectedInstance, executeGraph, editorReady]);

	// Load pattern graph on mount - wait for nodeTypes to be available
	useEffect(() => {
		hasHydratedGraphRef.current = false;
		let active = true;
		setLoading(true);
		setError(null);

		invoke<string>("get_pattern_graph", { id: patternId })
			.then((graphJson) => {
				if (!active) return;
				try {
					const parsed: Graph = JSON.parse(graphJson);
					const graph = ensureRequiredNodes(sanitizeGraph(parsed));
					console.log("[PatternEditor] Loaded graph JSON", {
						patternId,
						nodes: graph.nodes.length,
						edges: graph.edges.length,
						args: graph.args?.length ?? 0,
						nodeSample: graph.nodes.slice(0, 5).map((n) => ({
							id: n.id,
							typeId: n.typeId,
						})),
					});
					setPatternArgs((prev) => {
						const next = graph.args ?? [];
						const prevHash = JSON.stringify(prev ?? []);
						const nextHash = JSON.stringify(next);
						if (prevHash === nextHash) {
							return prev;
						}
						return next;
					});
					const withArgs = withPatternArgsNode(graph, graph.args ?? []);
					// Store graph to load when editor ref is ready
					setLoadedGraph(withArgs);
				} catch (err) {
					console.error("[PatternEditor] Failed to parse graph JSON", err);
					setError(
						err instanceof Error ? err.message : "Failed to parse graph JSON",
					);
				}
			})
			.catch((err) => {
				if (!active) return;
				console.error("[PatternEditor] Failed to load pattern graph", err);
				setError(err instanceof Error ? err.message : String(err));
			})
			.finally(() => {
				if (!active) return;
				setLoading(false);
			});

		return () => {
			active = false;
		};
	}, [patternId]);

	// Load graph into editor when both graph and editor are ready
	useEffect(() => {
		if (!loadedGraph || !editorReady || !editorRef.current) {
			return;
		}

		console.log("[PatternEditor] Hydrating editor with graph", {
			patternId,
			editorReady,
			nodes: loadedGraph.nodes.length,
			edges: loadedGraph.edges.length,
			args: loadedGraph.args?.length ?? 0,
		});

		editorRef.current.loadGraph(loadedGraph, getNodeDefinitions);
		hasHydratedGraphRef.current = true;
		// Set initial args hash to prevent false positive change detection
		lastPatternArgsHashRef.current = JSON.stringify(loadedGraph.args ?? []);

		// Execute the graph after loading
		if (selectedInstance) {
			setTimeout(async () => {
				await executeGraph(loadedGraph);
			}, 100);
		}

		// Clear loaded graph after loading to avoid reloading
		setLoadedGraph(null);
	}, [loadedGraph, editorReady, nodeTypes, executeGraph, getNodeDefinitions]);

	const serializeGraph = useCallback((): Graph | null => {
		if (!editorRef.current) return null;
		const graph = editorRef.current.serialize();
		const withArgs = withPatternArgsNode(
			{ ...graph, args: patternArgs },
			patternArgs,
		);
		return ensureRequiredNodes(withArgs);
	}, [patternArgs]);

	useEffect(() => {
		if (!editorReady || !editorRef.current) return;
		// Don't reload graph if we haven't hydrated it yet (initial load)
		if (!hasHydratedGraphRef.current) return;
		const argsHash = JSON.stringify(patternArgs ?? []);
		if (patternArgs.length === 0) {
			// Avoid overwriting the graph when there are no pattern args defined
			// (initial load sets patternArgs to [] which would serialize only required nodes)
			return;
		}
		if (argsHash === lastPatternArgsHashRef.current) {
			return;
		}
		lastPatternArgsHashRef.current = argsHash;
		const graph = serializeGraph();
		if (!graph) return;
		console.log("[PatternEditor] Reloading graph after args change", {
			patternId,
			nodes: graph.nodes.length,
			edges: graph.edges.length,
		});
		editorRef.current.loadGraph(graph, getNodeDefinitions);
		if (selectedInstance) {
			void executeGraph(graph);
		}
	}, [
		patternArgs,
		editorReady,
		getNodeDefinitions,
		serializeGraph,
		selectedInstance,
		patternId,
	]);

	// Save graph to database (manual save only)
	const saveGraph = useCallback(async () => {
		const graph = serializeGraph();
		if (!graph) {
			return;
		}

		setIsSaving(true);
		try {
			await invoke("save_pattern_graph", {
				id: patternId,
				graphJson: JSON.stringify(graph),
			});
		} catch (err) {
			console.error("[PatternEditor] Failed to save pattern graph", err);
			setError(err instanceof Error ? err.message : "Failed to save");
		} finally {
			setIsSaving(false);
		}
	}, [patternId, serializeGraph]);

	const handleGraphChange = useCallback(async () => {
		const graph = serializeGraph();
		if (!graph) {
			return;
		}

		// Only execute graph, don't save automatically
		await executeGraph(graph);
	}, [serializeGraph, executeGraph]);

	const handleEditArg = useCallback((arg: PatternArgDef) => {
		setEditingArgId(arg.id);
		setNewArgName(arg.name);
		setNewArgType(arg.argType);
		if (arg.argType === "Color") {
			const c = arg.defaultValue as {
				r: number;
				g: number;
				b: number;
				a: number;
			};
			const toHex = (v: number) =>
				Math.round(Number(v)).toString(16).padStart(2, "0");
			const hex = `#${toHex(c.r)}${toHex(c.g)}${toHex(c.b)}${toHex(
				Math.round(c.a * 255),
			)}`;
			setNewArgColor(hex);
		} else if (arg.argType === "Scalar") {
			setNewArgScalar(arg.defaultValue as unknown as number);
		}
		setArgDialogOpen(true);
	}, []);

	const handleDeleteArg = useCallback((argId: string) => {
		// eslint-disable-next-line no-restricted-globals
		if (confirm("Are you sure you want to delete this argument?")) {
			setPatternArgs((prev) => prev.filter((a) => a.id !== argId));
		}
	}, []);

	if (loading) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-muted-foreground">Loading pattern...</p>
			</div>
		);
	}

	if (error) {
		return (
			<div className="flex h-full flex-col items-center justify-center gap-4">
				<p className="text-destructive">{error}</p>
				<button
					type="button"
					onClick={goBack}
					className="text-sm text-muted-foreground hover:text-foreground"
				>
					Back to patterns
				</button>
			</div>
		);
	}

	return (
		<>
			<PatternAnnotationProvider
				value={{
					instances,
					selectedId: selectedInstanceId,
					selectInstance: setSelectedInstanceId,
					loading: instancesLoading,
				}}
			>
				<div className="flex h-full flex-col">
					<div className="flex flex-1 min-h-0">
						<ContextSidebar
							instances={instances}
							loading={instancesLoading}
							error={instancesError}
							selectedId={selectedInstanceId}
							onSelect={setSelectedInstanceId}
							onReload={loadInstances}
						/>
						<div className="flex-1 flex flex-col min-h-0">
							<div className="h-[45%] flex bg-card">
								<div className="flex-1 flex flex-col min-w-0">
									<div className="flex-1 relative">
										<StageVisualizer
											enableEditing={false}
											renderAudioTimeSec={renderAudioTime}
										/>
										{selectedInstance && (
											<div className="absolute top-2 right-2 pointer-events-none text-[10px] text-white/50 bg-black/50 px-2 py-1 rounded">
												{selectedInstance.track.title ??
													`Track ${selectedInstance.track.id}`}
											</div>
										)}
									</div>
									<TransportBar
										beatGrid={selectedInstance?.beatGrid ?? null}
										segmentDuration={
											(selectedInstance?.endTime ?? 0) -
											(selectedInstance?.startTime ?? 0)
										}
										startTime={selectedInstance?.startTime ?? 0}
									/>
								</div>
								<PatternInfoPanel
									pattern={pattern}
									loading={patternLoading}
									args={patternArgs}
									onAddArg={() => setArgDialogOpen(true)}
									onEditArg={handleEditArg}
									onDeleteArg={handleDeleteArg}
								/>
							</div>
							<div className="flex-1 bg-black/10 relative min-h-0 border-t">
								{graphError && (
									<div className="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-center justify-center rounded-b-md bg-red-500/20 px-4 py-2 text-sm font-semibold text-red-700 shadow-sm backdrop-blur-sm">
										{graphError}
									</div>
								)}
								<ReactFlowEditorWrapper
									onChange={handleGraphChange}
									getNodeDefinitions={getNodeDefinitions}
									controllerRef={editorRef}
									onReady={() => {
										setEditorReady(true);
									}}
								/>
								{isBuildingGraph && (
									<div className="absolute bottom-3 right-3 z-30 pointer-events-none">
										<div className="flex items-center gap-2 rounded-full border border-border/80 bg-background/90 px-3 py-2 text-xs text-muted-foreground shadow-lg">
											<Loader2 className="h-4 w-4 animate-spin text-primary" />
											<span>Building graph…</span>
										</div>
									</div>
								)}
								{/* Floating Save Button */}
								<div className="absolute top-4 right-4 z-30">
									<button
										type="button"
										onClick={saveGraph}
										disabled={isSaving}
										className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-primary-foreground bg-primary rounded-md hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed shadow-lg"
									>
										<Save size={16} />
										{isSaving ? "Saving..." : "Save"}
									</button>
								</div>
							</div>
						</div>
					</div>
				</div>
			</PatternAnnotationProvider>

			<Dialog
				open={argDialogOpen}
				onOpenChange={(open) => {
					setArgDialogOpen(open);
					if (!open) {
						setEditingArgId(null);
						setNewArgName("");
						setNewArgColor("#ff0000");
						setNewArgScalar(1.0);
						setNewArgType("Color");
					}
				}}
			>
				<DialogContent className="bg-background">
					<DialogHeader>
						<DialogTitle>
							{editingArgId ? "Edit Pattern Arg" : "Add Pattern Arg"}
						</DialogTitle>
					</DialogHeader>
					<div className="space-y-4">
						<div className="space-y-2">
							<label
								htmlFor="pattern-arg-name"
								className="text-xs text-muted-foreground"
							>
								Name
							</label>
							<Input
								id="pattern-arg-name"
								value={newArgName}
								onChange={(e) => setNewArgName(e.target.value)}
								placeholder="Color"
							/>
						</div>
						<div className="space-y-2">
							<label
								htmlFor="pattern-arg-type"
								className="text-xs text-muted-foreground"
							>
								Type
							</label>
							<Select
								value={newArgType}
								onValueChange={(v) => setNewArgType(v as "Color" | "Scalar")}
								disabled={!!editingArgId}
							>
								<SelectTrigger id="pattern-arg-type">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="Color">Color</SelectItem>
									<SelectItem value="Scalar">Scalar</SelectItem>
								</SelectContent>
							</Select>
						</div>
						{newArgType === "Color" && (
							<div className="space-y-2">
								<label
									htmlFor="pattern-arg-color"
									className="text-xs text-muted-foreground"
								>
									Default Color
								</label>
								<Popover>
									<PopoverTrigger asChild>
										<button
											id="pattern-arg-color"
											type="button"
											className="w-full flex items-center justify-between bg-muted rounded px-2 py-2"
										>
											<span
												className="w-6 h-6 rounded border"
												style={{ backgroundColor: newArgColor }}
											/>
											<span className="font-mono text-xs">{newArgColor}</span>
										</button>
									</PopoverTrigger>
									<PopoverContent className="w-auto bg-neutral-900 border border-neutral-800 p-3">
										<ColorPicker
											defaultValue={newArgColor}
											onChange={(rgba) => {
												if (Array.isArray(rgba) && rgba.length >= 3) {
													const toHex = (v: number) =>
														Math.round(Number(v)).toString(16).padStart(2, "0");
													const a =
														rgba.length >= 4
															? Math.round(Number(rgba[3]) * 255)
															: 255;
													setNewArgColor(
														`#${toHex(rgba[0])}${toHex(rgba[1])}${toHex(rgba[2])}${toHex(
															a,
														)}`,
													);
												}
											}}
										>
											<div className="flex flex-col gap-2">
												<ColorPickerSelection className="h-28 w-48 rounded" />
												<ColorPickerHue className="flex-1" />
												<ColorPickerAlpha />
											</div>
										</ColorPicker>
									</PopoverContent>
								</Popover>
							</div>
						)}
						{newArgType === "Scalar" && (
							<div className="space-y-2">
								<label
									htmlFor="pattern-arg-scalar"
									className="text-xs text-muted-foreground"
								>
									Default Value
								</label>
								<Input
									id="pattern-arg-scalar"
									type="number"
									step="0.1"
									value={newArgScalar}
									onChange={(e) => setNewArgScalar(Number(e.target.value))}
								/>
							</div>
						)}
					</div>
					<DialogFooter>
						<button
							type="button"
							onClick={() => setArgDialogOpen(false)}
							className="px-3 py-2 text-sm text-muted-foreground"
						>
							Cancel
						</button>
						<button
							type="button"
							onClick={() => {
								let id = editingArgId;
								if (!id) {
									const slug =
										newArgName
											.trim()
											.toLowerCase()
											.replace(/[^a-z0-9]+/g, "_") || "arg";
									id = slug;
									let counter = 1;
									while (patternArgs.some((a) => a.id === id)) {
										id = `${slug}_${counter++}`;
									}
								}

								let defaultValue: Record<string, unknown>;
								if (newArgType === "Color") {
									const hex = newArgColor.startsWith("#")
										? newArgColor
										: `#${newArgColor}`;
									const safe = hex.replace("#", "");
									const r = parseInt(safe.slice(0, 2), 16) || 0;
									const g = parseInt(safe.slice(2, 4), 16) || 0;
									const b = parseInt(safe.slice(4, 6), 16) || 0;
									let a = 1;
									if (safe.length === 8) {
										a = (parseInt(safe.slice(6, 8), 16) || 255) / 255;
									}
									defaultValue = { r, g, b, a };
								} else {
									defaultValue = newArgScalar as unknown as Record<
										string,
										unknown
									>;
								}

								const newArg: PatternArgDef = {
									id,
									name: newArgName.trim() || "Arg",
									argType: newArgType,
									defaultValue,
								};

								let nextArgs: PatternArgDef[];
								if (editingArgId) {
									nextArgs = patternArgs.map((a) =>
										a.id === editingArgId ? newArg : a,
									);
								} else {
									nextArgs = [...patternArgs, newArg];
								}

								setPatternArgs(nextArgs);
								setArgDialogOpen(false);
								setEditingArgId(null);
								setNewArgName("");
								setNewArgColor("#ff0000");
								setNewArgScalar(1.0);
								setNewArgType("Color");

								const graph = serializeGraph();
								if (graph && editorRef.current) {
									const withNode = withPatternArgsNode(
										{ ...graph, args: nextArgs },
										nextArgs,
									);
									editorRef.current.loadGraph(withNode, getNodeDefinitions);
									void executeGraph(withNode);
								}
							}}
							className="px-3 py-2 text-sm font-medium bg-primary text-primary-foreground rounded-md"
						>
							{editingArgId ? "Save Changes" : "Add Arg"}
						</button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</>
	);
}
