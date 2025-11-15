import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2Icon } from "lucide-react";

import type {
	Graph,
	MelSpec,
	NodeTypeDef,
	PatternEntrySummary,
	PlaybackStateSnapshot,
} from "@/bindings/schema";
import {
	ReactFlowEditorWrapper,
	type EditorController,
} from "@/lib/reactFlowEditor";
import { useAppViewStore } from "@/useAppViewStore";
import { usePatternPlaybackStore } from "@/usePatternPlaybackStore";

type RunResult = {
	views: Record<string, number[]>;
	melSpecs: Record<string, MelSpec>;
	patternEntries: Record<string, PatternEntrySummary>;
};

type PatternEditorProps = {
	patternId: number;
	patternName: string;
	nodeTypes: NodeTypeDef[];
};

export function PatternEditor({
	patternId,
	patternName,
	nodeTypes,
}: PatternEditorProps) {
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const [graphError, setGraphError] = useState<string | null>(null);
	const [isRunningGraph, setIsRunningGraph] = useState(false);
	const [runResult, setRunResult] = useState<RunResult | null>(null);
	const [isSaving, setIsSaving] = useState(false);
	const [loadedGraph, setLoadedGraph] = useState<Graph | null>(null);
	const [editorReady, setEditorReady] = useState(false);

	const editorRef = useRef<EditorController | null>(null);
	const pendingRunId = useRef(0);
	const nodeTypesRef = useRef<NodeTypeDef[]>([]);
	const setView = useAppViewStore((state) => state.setView);
	const setPatternEntries = useCallback(
		(entries: Record<string, PatternEntrySummary>) => {
			usePatternPlaybackStore.getState().setEntries(entries);
		},
		[],
	);

	useEffect(() => {
		nodeTypesRef.current = nodeTypes;
	}, [nodeTypes]);

	useEffect(() => {
		let unsub: (() => void) | null = null;
		let cancelled = false;
		const store = usePatternPlaybackStore;
		const handleSnapshot = (snapshot: PlaybackStateSnapshot) => {
			store.getState().handleSnapshot(snapshot);
		};
		const reset = () => store.getState().reset();

		listen<PlaybackStateSnapshot>("pattern-playback://state", (event) => {
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
					"[PatternEditor] Failed to subscribe to playback state",
					err,
				);
			});

		invoke<PlaybackStateSnapshot>("playback_snapshot")
			.then((snapshot) => {
				if (!cancelled) {
					handleSnapshot(snapshot);
				}
			})
			.catch((err) => {
				console.error("[PatternEditor] Failed to fetch playback snapshot", err);
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
		(views: Record<string, number[]>, melSpecs: Record<string, MelSpec>) => {
			if (!editorRef.current) return;
			editorRef.current.updateViewData(views, melSpecs);
		},
		[],
	);

	const executeGraph = useCallback(
		async (graph: Graph) => {
			if (graph.nodes.length === 0) {
				setRunResult(null);
				setGraphError(null);
				setPatternEntries({});
				editorRef.current?.updatePatternEntries({});
				await updateViewResults({}, {});
				return;
			}

			const runId = ++pendingRunId.current;
			setIsRunningGraph(true);

			try {
				const result = await invoke<RunResult>("run_graph", { graph });
				if (runId !== pendingRunId.current) return;

				setRunResult(result);
				setGraphError(null);
				setPatternEntries(result.patternEntries ?? {});
				editorRef.current?.updatePatternEntries(result.patternEntries ?? {});
				await updateViewResults(result.views ?? {}, result.melSpecs ?? {});
			} catch (err) {
				if (runId !== pendingRunId.current) return;
				console.error("Failed to execute graph", err);
				setGraphError(err instanceof Error ? err.message : String(err));
			} finally {
				if (runId === pendingRunId.current) {
					setIsRunningGraph(false);
				}
			}
		},
		[updateViewResults],
	);

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
					const graph: Graph = JSON.parse(graphJson);
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
					onClick={() => setView({ type: "welcome" })}
					className="text-sm text-muted-foreground hover:text-foreground"
				>
					Back to patterns
				</button>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col">
			<div className="border-b border-border bg-background p-4">
				<div className="flex items-center justify-between">
					<div className="flex items-center gap-4">
						<button
							type="button"
							onClick={() => setView({ type: "welcome" })}
							className="text-sm text-muted-foreground hover:text-foreground"
						>
							← Back
						</button>
						<h1 className="text-xl font-semibold">{patternName}</h1>
					</div>
					<div className="flex items-center gap-2">
						{isRunningGraph && (
							<Loader2Icon
								className="h-4 w-4 animate-spin text-foreground/70"
								aria-hidden="true"
							/>
						)}
						<button
							type="button"
							onClick={saveGraph}
							disabled={isSaving}
							className="px-4 py-2 text-sm font-medium text-white bg-primary rounded-md hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed"
						>
							{isSaving ? "Saving..." : "Save"}
						</button>
					</div>
				</div>
			</div>

			<div className="flex-1 border-b border-border bg-background relative">
				<ReactFlowEditorWrapper
					onChange={handleGraphChange}
					getNodeDefinitions={() => nodeTypesRef.current}
					controllerRef={editorRef}
					onReady={() => {
						setEditorReady(true);
					}}
				/>
			</div>
		</div>
	);
}
