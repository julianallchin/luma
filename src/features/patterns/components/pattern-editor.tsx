import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Pause, Play, Repeat, Save, SkipBack } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type {
	BeatGrid,
	Graph,
	GraphContext,
	HostAudioSnapshot,
	MelSpec,
	NodeTypeDef,
	Series,
	TrackSummary,
} from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import {
	type PatternAnnotationInstance,
	PatternAnnotationProvider,
} from "@/features/patterns/contexts/pattern-annotation-context";
import { useHostAudioStore } from "@/features/patterns/stores/use-host-audio-store";
import type {
	TrackAnnotation,
	TrackWaveform,
} from "@/features/track-editor/stores/use-track-editor-store";
import { formatTime } from "@/shared/lib/react-flow/base-node";
import {
	type EditorController,
	ReactFlowEditorWrapper,
} from "@/shared/lib/react-flow-editor";

type RunResult = {
	views: Record<string, number[]>;
	melSpecs: Record<string, MelSpec>;
	seriesViews: Record<string, Series>;
	colorViews: Record<string, string>;
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

function sliceBeatGrid(grid: BeatGrid | null, start: number, end: number) {
	if (!grid) return null;
	const beats = grid.beats
		.filter((t) => t >= start && t <= end)
		.map((t) => t - start);
	const downbeats = grid.downbeats
		.filter((t) => t >= start && t <= end)
		.map((t) => t - start);
	return {
		...grid,
		beats,
		downbeats,
		downbeatOffset: Math.max(0, grid.downbeatOffset - start),
	};
}

function TransportBar({ beatGrid, segmentDuration }: TransportBarProps) {
	const isPlaying = useHostAudioStore((s) => s.isPlaying);
	const currentTime = useHostAudioStore((s) => s.currentTime);
	const durationSeconds = useHostAudioStore((s) => s.durationSeconds);
	const loopEnabled = useHostAudioStore((s) => s.loopEnabled);
	const [scrubValue, setScrubValue] = useState<number | null>(null);
	const scrubberRef = useRef<HTMLDivElement>(null);
	const displayTime = scrubValue ?? currentTime;
	const total = Math.max(durationSeconds, 0.0001);
	const progress = (displayTime / total) * 100;
	const beatPosition = secondsToBeats(displayTime, beatGrid);
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
	const [instances, setInstances] = useState<PatternAnnotationInstance[]>([]);
	const [instancesLoading, setInstancesLoading] = useState(false);
	const [instancesError, setInstancesError] = useState<string | null>(null);
	const [selectedInstanceId, setSelectedInstanceId] = useState<number | null>(
		null,
	);
	const selectedInstance = useMemo(
		() => instances.find((inst) => inst.id === selectedInstanceId) ?? null,
		[instances, selectedInstanceId],
	);
	useEffect(() => {
		if (selectedInstance) {
			setGraphError(null);
		}
	}, [selectedInstance]);

	const editorRef = useRef<EditorController | null>(null);
	const pendingRunId = useRef(0);
	const nodeTypesRef = useRef<NodeTypeDef[]>([]);
	const goBack = useAppViewStore((state) => state.goBack);

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
		nodeTypesRef.current = nodeTypes;
	}, [nodeTypes]);

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
			views: Record<string, number[]>,
			melSpecs: Record<string, MelSpec>,
			seriesViews: Record<string, Series>,
			colorViews: Record<string, string>,
		) => {
			if (!editorRef.current) return;
			editorRef.current.updateViewData(
				views,
				melSpecs,
				seriesViews,
				colorViews,
			);
		},
		[],
	);

	const executeGraph = useCallback(
		async (graph: Graph) => {
			if (!selectedInstance) {
				setGraphError(
					"Select an annotated track section from the Context panel to run this pattern.",
				);
				await updateViewResults({}, {}, {}, {});
				return;
			}

			const hasAudioInput = graph.nodes.some(
				(node) => node.typeId === "audio_input",
			);
			const hasBeatClock = graph.nodes.some(
				(node) => node.typeId === "beat_clock",
			);
			if (!hasAudioInput || !hasBeatClock) {
				setGraphError("Add Audio Input and Beat Clock nodes to the canvas.");
				await updateViewResults({}, {}, {}, {});
				return;
			}

			if (graph.nodes.length === 0) {
				setGraphError(null);
				await updateViewResults({}, {}, {}, {});
				return;
			}

			const runId = ++pendingRunId.current;

			try {
				// Context is now passed separately from the graph
				// The graph stays pure (no track-specific params injected)
				const context: GraphContext = {
					trackId: selectedInstance.track.id,
					startTime: selectedInstance.startTime,
					endTime: selectedInstance.endTime,
					beatGrid: selectedInstance.beatGrid,
				};

				const result = await invoke<RunResult>("run_graph", {
					graph,
					context,
				});
				if (runId !== pendingRunId.current) return;

				setGraphError(null);
				await updateViewResults(
					result.views ?? {},
					result.melSpecs ?? {},
					result.seriesViews ?? {},
					result.colorViews ?? {},
				);
			} catch (err) {
				if (runId !== pendingRunId.current) return;
				console.error("Failed to execute graph", err);
				setGraphError(err instanceof Error ? err.message : String(err));
			}
		},
		[updateViewResults, selectedInstance],
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
		// Don't load until we have node types
		if (nodeTypes.length === 0) {
			return;
		}

		let active = true;
		setLoading(true);
		setError(null);

		invoke<string>("get_pattern_graph", { id: patternId })
			.then((graphJson) => {
				if (!active) return;
				try {
					const parsed: Graph = JSON.parse(graphJson);
					const graph = sanitizeGraph(parsed);
					// Store graph to load when editor ref is ready
					setLoadedGraph(graph);
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
	}, [patternId, nodeTypes]);

	// Load graph into editor when both graph and editor are ready
	useEffect(() => {
		if (
			!loadedGraph ||
			!editorReady ||
			!editorRef.current ||
			nodeTypes.length === 0
		) {
			return;
		}

		editorRef.current.loadGraph(loadedGraph, () => nodeTypesRef.current);

		// Execute the graph after loading
		setTimeout(async () => {
			await executeGraph(loadedGraph);
		}, 100);

		// Clear loaded graph after loading to avoid reloading
		setLoadedGraph(null);
	}, [loadedGraph, editorReady, nodeTypes, executeGraph]);

	const serializeGraph = useCallback((): Graph | null => {
		if (!editorRef.current) return null;
		const graph = editorRef.current.serialize();
		return graph;
	}, []);

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
						<div className="h-1/2 flex flex-col bg-card">
							<div className="flex-1 flex items-center justify-center text-center text-foreground bg-black/80">
								<div className="space-y-2">
									<p className="text-sm font-semibold uppercase tracking-[0.2em] text-primary">
										Visualizer Stage
									</p>
									<p className="text-xs text-muted-foreground">
										3D preview coming soon. Use transport to audition your
										pattern.
									</p>
									{selectedInstance ? (
										<p className="text-[11px] text-primary/80">
											Loaded{" "}
											{selectedInstance.track.title ??
												`Track ${selectedInstance.track.id}`}{" "}
											· {formatTime(selectedInstance.startTime)} –{" "}
											{formatTime(selectedInstance.endTime)}
										</p>
									) : null}
								</div>
							</div>
						</div>
						<TransportBar
							beatGrid={selectedInstance?.beatGrid ?? null}
							segmentDuration={
								(selectedInstance?.endTime ?? 0) -
								(selectedInstance?.startTime ?? 0)
							}
						/>
						<div className="flex-1 bg-black/10 relative min-h-0 border-t">
							{graphError && (
								<div className="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-center justify-center rounded-b-md bg-red-500/20 px-4 py-2 text-sm font-semibold text-red-700 shadow-sm backdrop-blur-sm">
									{graphError}
								</div>
							)}
							<ReactFlowEditorWrapper
								onChange={handleGraphChange}
								getNodeDefinitions={() => nodeTypesRef.current}
								controllerRef={editorRef}
								onReady={() => {
									setEditorReady(true);
								}}
							/>
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
	);
}
