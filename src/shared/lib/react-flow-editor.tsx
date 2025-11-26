import * as React from "react";
import ReactFlow, {
	addEdge,
	Background,
	type Connection,
	type Edge,
	type Node,
	type ReactFlowInstance,
	ReactFlowProvider,
	useEdgesState,
	useNodesState,
} from "reactflow";
import "reactflow/dist/style.css";
import type { Graph, NodeTypeDef, Series } from "@/bindings/schema";
import {
	getNodeParamsSnapshot,
	removeNodeParams,
	replaceAllNodeParams,
	setNodeParamsSnapshot,
	useGraphStore,
} from "@/features/patterns/stores/use-graph-store";
import { makeIsValidConnection } from "./react-flow/connection-validation";
import {
	buildNode,
	serializeParams,
	syncNodeIdCounter,
} from "./react-flow/node-builder";
import {
	AudioInputNode,
	BeatClockNode,
	ColorNode,
	HarmonyColorVisualizerNode,
	MelSpecNode,
	StandardNode,
	ViewChannelNode,
} from "./react-flow/nodes";
import type {
	AudioInputNodeData,
	BaseNodeData,
	BeatClockNodeData,
	HarmonyColorVisualizerNodeData,
	MelSpecNodeData,
	ViewChannelNodeData,
} from "./react-flow/types";

type AnyNodeData =
	| BaseNodeData
	| ViewChannelNodeData
	| MelSpecNodeData
	| AudioInputNodeData
	| BeatClockNodeData;

// Editor component
export type EditorController = {
	addNode(definition: NodeTypeDef, position?: { x: number; y: number }): void;
	serialize(): Graph;
	loadGraph(graph: Graph, getNodeDefinitions: () => NodeTypeDef[]): void;
	updateViewData(
		views: Record<string, number[]>,
		melSpecs: Record<string, { width: number; height: number; data: number[] }>,
		seriesViews: Record<string, Series>,
		colorViews: Record<string, string>,
	): void;
	updateNodeContext(context: {
		trackName?: string;
		timeLabel?: string;
		bpmLabel?: string;
	}): void;
};

type ReactFlowEditorProps = {
	onChange: () => void;
	getNodeDefinitions: () => NodeTypeDef[];
	controllerRef?: React.MutableRefObject<EditorController | null>;
	onReady?: () => void;
};

export function ReactFlowEditor({
	onChange,
	getNodeDefinitions,
	controllerRef,
	onReady,
}: ReactFlowEditorProps) {
	const [nodes, setNodes, onNodesChange] = useNodesState<AnyNodeData>([]);
	const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
	const paramsVersion = useGraphStore((state) => state.version);
	const [reactFlowInstance, setReactFlowInstance] =
		React.useState<ReactFlowInstance | null>(null);
	const isLoadingRef = React.useRef(false);
	const pendingChangeRef = React.useRef(false);

	const isValidConnection = React.useMemo(
		() => makeIsValidConnection(nodes),
		[nodes],
	);

	const nodeTypes = React.useMemo(
		() => ({
			standard: StandardNode,
			viewChannel: ViewChannelNode,
			melSpec: MelSpecNode,
			audioInput: AudioInputNode,
			beatClock: BeatClockNode,
			color: ColorNode,
			harmonyColorVisualizer: HarmonyColorVisualizerNode,
		}),
		[],
	);

	// Stable onChange ref to prevent infinite loops
	const onChangeRef = React.useRef(onChange);
	React.useEffect(() => {
		onChangeRef.current = onChange;
	}, [onChange]);

	// Debounce onChange calls to prevent infinite loops
	const onChangeTimeoutRef = React.useRef<NodeJS.Timeout | null>(null);
	const triggerOnChange = React.useCallback(() => {
		if (onChangeTimeoutRef.current) {
			clearTimeout(onChangeTimeoutRef.current);
		}
		onChangeTimeoutRef.current = setTimeout(() => {
			onChangeRef.current();
		}, 100);
	}, []);

	// Expose controller methods via ref - use refs to avoid recreating on every change
	const nodesRef = React.useRef(nodes);
	const edgesRef = React.useRef(edges);
	React.useEffect(() => {
		nodesRef.current = nodes;
	}, [nodes]);
	React.useEffect(() => {
		edgesRef.current = edges;
	}, [edges]);

	React.useEffect(() => {
		if (!controllerRef) return;

		controllerRef.current = {
			addNode(definition, position) {
				const node = buildNode(definition, triggerOnChange, position);
				setNodeParamsSnapshot(node.id, serializeParams(node.data.params ?? {}));
				setNodes((nds) => [...nds, node]);
			},
			serialize(): Graph {
				const graphNodes = nodesRef.current.map((node) => ({
					id: node.id,
					typeId: node.data.typeId,
					params: serializeParams(getNodeParamsSnapshot(node.id)),
					positionX: node.position.x,
					positionY: node.position.y,
				}));

				const graphEdges = edgesRef.current.map((edge) => ({
					id: edge.id,
					fromNode: edge.source,
					fromPort: edge.sourceHandle ?? "",
					toNode: edge.target,
					toPort: edge.targetHandle ?? "",
				}));

				return { nodes: graphNodes, edges: graphEdges };
			},
			loadGraph(graph: Graph, getNodeDefinitions: () => NodeTypeDef[]) {
				isLoadingRef.current = true;
				syncNodeIdCounter(graph.nodes.map((graphNode) => graphNode.id));
				const definitions = getNodeDefinitions();
				const defMap = new Map(definitions.map((def) => [def.id, def]));

				const paramEntries: Record<string, Record<string, unknown>> = {};
				for (const graphNode of graph.nodes) {
					paramEntries[graphNode.id] = serializeParams(graphNode.params ?? {});
				}
				replaceAllNodeParams(paramEntries);

				// Convert graph nodes to ReactFlow nodes
				const loadedNodes: Node<AnyNodeData>[] = graph.nodes
					.map((graphNode, index) => {
						const definition = defMap.get(graphNode.typeId);
						if (!definition) {
							console.warn(`Unknown node type: ${graphNode.typeId}`);
							return null;
						}

						const inputs = definition.inputs.map((p) => ({
							id: p.id,
							label: p.name,
							direction: "in" as const,
							portType: p.portType,
						}));
						const outputs = definition.outputs.map((p) => ({
							id: p.id,
							label: p.name,
							direction: "out" as const,
							portType: p.portType,
						}));

						const baseData: BaseNodeData = {
							title: definition.name,
							inputs,
							outputs,
							typeId: definition.id,
							definition,
							params: graphNode.params,
							onChange: triggerOnChange,
						};

						const nodeType =
							definition.id === "view_channel"
								? "viewChannel"
								: definition.id === "mel_spec_viewer"
									? "melSpec"
									: definition.id === "audio_input"
										? "audioInput"
										: definition.id === "beat_clock"
											? "beatClock"
											: "standard";
						// Use stored position if available, otherwise generate one
						const position = {
							x: graphNode.positionX ?? (index % 5) * 200,
							y: graphNode.positionY ?? Math.floor(index / 5) * 150,
						};

						if (nodeType === "viewChannel") {
							const viewData: ViewChannelNodeData = {
								...baseData,
								viewSamples: null,
								seriesData: null,
							};
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: viewData,
							} as Node<ViewChannelNodeData>;
						}

						if (nodeType === "melSpec") {
							const melData: MelSpecNodeData = {
								...baseData,
								melSpec: undefined,
							};
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: melData,
							} as Node<MelSpecNodeData>;
						}

						return {
							id: graphNode.id,
							type: nodeType,
							position,
							data: baseData,
						} as Node<BaseNodeData>;
					})
					.filter((node): node is Node<AnyNodeData> => node !== null);

				// Convert graph edges to ReactFlow edges
				const loadedEdges: Edge[] = graph.edges.map((graphEdge) => ({
					id: graphEdge.id,
					source: graphEdge.fromNode,
					target: graphEdge.toNode,
					sourceHandle: graphEdge.fromPort,
					targetHandle: graphEdge.toPort,
				}));

				setNodes(loadedNodes);
				setEdges(loadedEdges);
				// Reset loading flag after a short delay to allow state to settle.
				// If any node params changed while we were hydrating the graph, make sure
				// we schedule a run once loading completes.
				setTimeout(() => {
					isLoadingRef.current = false;
					if (pendingChangeRef.current) {
						pendingChangeRef.current = false;
						triggerOnChange();
					}
				}, 200);
			},
			updateViewData(views, melSpecs, seriesViews, colorViews) {
				setNodes((nds) =>
					nds.map((node) => {
						if (node.data.typeId === "view_channel") {
							const samples = views[node.id] ?? null;
							const series = seriesViews[node.id] ?? null;
							return {
								...node,
								data: {
									...node.data,
									viewSamples: samples,
									seriesData: series,
								} as ViewChannelNodeData,
							};
						}
						if (node.data.typeId === "mel_spec_viewer") {
							const spec = melSpecs[node.id];
							return {
								...node,
								data: {
									...node.data,
									melSpec: spec,
								} as MelSpecNodeData,
							};
						}
						if (node.data.typeId === "harmony_color_visualizer") {
							const series = seriesViews[node.id] ?? null;
							const baseColor = colorViews[node.id] ?? null;
							return {
								...node,
								data: {
									...node.data,
									seriesData: series,
									baseColor: baseColor,
								} as HarmonyColorVisualizerNodeData,
							};
						}
						return node;
					}),
				);
			},
			updateNodeContext(context) {
				setNodes((nds) =>
					nds.map((node) => {
						if (node.data.typeId === "audio_input") {
							return {
								...node,
								data: {
									...node.data,
									trackName: context.trackName,
									timeLabel: context.timeLabel,
								} as AudioInputNodeData,
							};
						}
						if (node.data.typeId === "beat_clock") {
							return {
								...node,
								data: {
									...node.data,
									bpmLabel: context.bpmLabel,
								} as BeatClockNodeData,
							};
						}
						return node;
					}),
				);
			},
		};

		// Notify that editor is ready
		if (onReady) {
			onReady();
		}
	}, [controllerRef, triggerOnChange, setNodes, setEdges, onReady]);

	// Track previous graph structure to only call onChange on structural changes
	// Exclude positions so that node movement doesn't trigger execution
	const prevGraphRef = React.useRef<string>("");
	React.useEffect(() => {
		// Serialize current graph structure (excluding positions)
		// Only compare structural changes: nodes, edges, and params
		const currentGraph = JSON.stringify({
			nodes: nodes.map((n) => ({
				id: n.id,
				typeId: n.data.typeId,
				params: getNodeParamsSnapshot(n.id),
			})),
			edges: edges.map((e) => ({
				id: e.id,
				source: e.source,
				target: e.target,
				sourceHandle: e.sourceHandle,
				targetHandle: e.targetHandle,
			})),
		});

		// Only trigger onChange if graph structure changed (not positions)
		// Skip if we're currently loading a graph
		if (currentGraph !== prevGraphRef.current) {
			prevGraphRef.current = currentGraph;
			if (isLoadingRef.current) {
				pendingChangeRef.current = true;
			} else {
				triggerOnChange();
			}
		}
	}, [nodes, edges, paramsVersion, triggerOnChange]);

	// Node drag stop - don't trigger onChange since positions don't affect execution
	const onNodeDragStop = React.useCallback(() => {
		// No-op: positions don't affect execution
	}, []);

	// Handle connections
	const onConnect = React.useCallback(
		(params: Connection) => {
			setEdges((eds) => {
				const newEdges = addEdge(params, eds);
				triggerOnChange();
				return newEdges;
			});
		},
		[setEdges, triggerOnChange],
	);

	// Handle context menu
	const [contextMenuPosition, setContextMenuPosition] = React.useState<{
		x: number;
		y: number;
		flowX: number;
		flowY: number;
		type: "pane" | "node" | "edge";
		nodeId?: string;
		edgeId?: string;
	} | null>(null);

	const onPaneContextMenu = React.useCallback(
		(event: React.MouseEvent) => {
			event.preventDefault();
			if (reactFlowInstance) {
				const flowPosition = reactFlowInstance.screenToFlowPosition({
					x: event.clientX,
					y: event.clientY,
				});
				setContextMenuPosition({
					x: event.clientX,
					y: event.clientY,
					flowX: flowPosition.x,
					flowY: flowPosition.y,
					type: "pane",
				});
			}
		},
		[reactFlowInstance],
	);

	const onNodeContextMenu = React.useCallback(
		(event: React.MouseEvent, node: Node) => {
			event.preventDefault();
			setContextMenuPosition({
				x: event.clientX,
				y: event.clientY,
				flowX: node.position.x,
				flowY: node.position.y,
				type: "node",
				nodeId: node.id,
			});
		},
		[],
	);

	const onEdgeContextMenu = React.useCallback(
		(event: React.MouseEvent, edge: Edge) => {
			event.preventDefault();
			setContextMenuPosition({
				x: event.clientX,
				y: event.clientY,
				flowX: 0,
				flowY: 0,
				type: "edge",
				edgeId: edge.id,
			});
		},
		[],
	);

	// Handle delete key
	React.useEffect(() => {
		const handleKeyDown = (event: KeyboardEvent) => {
			if (event.key !== "Delete" && event.key !== "Backspace") return;
			const target = event.target as HTMLElement | null;
			if (
				target &&
				(target.tagName === "INPUT" ||
					target.tagName === "TEXTAREA" ||
					target.isContentEditable)
			) {
				return;
			}

			event.preventDefault();
			setNodes((nds) => {
				const removed = nds.filter((node) => node.selected);
				if (removed.length > 0) {
					for (const node of removed) {
						removeNodeParams(node.id);
					}
				}
				const filtered = nds.filter((node) => !node.selected);
				if (filtered.length !== nds.length) {
					triggerOnChange();
				}
				return filtered;
			});
			setEdges((eds) => {
				const filtered = eds.filter((edge) => !edge.selected);
				if (filtered.length !== eds.length) {
					triggerOnChange();
				}
				return filtered;
			});
		};

		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [setNodes, setEdges, triggerOnChange]);

	// Clear any pending debounced runs when the editor unmounts.
	React.useEffect(() => {
		return () => {
			if (onChangeTimeoutRef.current) {
				clearTimeout(onChangeTimeoutRef.current);
			}
		};
	}, []);

	// Group node types for context menu
	const groupNodeTypes = React.useCallback((definitions: NodeTypeDef[]) => {
		const grouped = definitions.reduce<Record<string, NodeTypeDef[]>>(
			(acc, definition) => {
				const category = definition.category ?? "Nodes";
				if (!acc[category]) {
					acc[category] = [];
				}
				acc[category].push(definition);
				return acc;
			},
			{},
		);

		return Object.entries(grouped)
			.map(([category, nodes]) => ({
				category,
				nodes: nodes.sort((a, b) => a.name.localeCompare(b.name)),
			}))
			.sort((a, b) => a.category.localeCompare(b.category));
	}, []);

	const handleAddNode = React.useCallback(
		(definition: NodeTypeDef) => {
			if (contextMenuPosition) {
				const node = buildNode(definition, triggerOnChange, {
					x: contextMenuPosition.flowX,
					y: contextMenuPosition.flowY,
				});
				setNodeParamsSnapshot(node.id, serializeParams(node.data.params ?? {}));
				setNodes((nds) => [...nds, node]);
				setContextMenuPosition(null);
			}
		},
		[contextMenuPosition, triggerOnChange, setNodes],
	);

	// Compute catalog groups dynamically when context menu opens
	const getCatalogGroups = React.useCallback(() => {
		return groupNodeTypes(getNodeDefinitions());
	}, [getNodeDefinitions, groupNodeTypes]);

	return (
		<div className="w-full h-full relative">
			<ReactFlow
				nodes={nodes}
				edges={edges}
				onNodesChange={(changes) => {
					changes.forEach((change) => {
						if (change.type === "remove" && change.id) {
							removeNodeParams(change.id);
						}
					});
					onNodesChange(changes);
				}}
				onEdgesChange={onEdgesChange}
				onNodeDragStop={onNodeDragStop}
				onConnect={onConnect}
				isValidConnection={isValidConnection}
				nodeTypes={nodeTypes}
				onInit={setReactFlowInstance}
				onPaneContextMenu={onPaneContextMenu}
				onNodeContextMenu={onNodeContextMenu}
				onEdgeContextMenu={onEdgeContextMenu}
				maxZoom={1.2}
				fitView
				proOptions={{ hideAttribution: true }}
			>
				<Background gap={20} />
			</ReactFlow>

			{contextMenuPosition && (
				<div
					role="menu"
					className="bg-popover fixed border border-border rounded-lg shadow-lg p-2 z-50 max-h-96 overflow-y-auto min-w-[200px]"
					style={{
						left: `${contextMenuPosition.x}px`,
						top: `${contextMenuPosition.y}px`,
					}}
					onClick={(e) => e.stopPropagation()}
					onKeyDown={(e) => {
						if (e.key === "Escape") {
							setContextMenuPosition(null);
						}
					}}
				>
					{contextMenuPosition.type === "pane" ? (
						// Show node catalog when right-clicking on pane
						(() => {
							const groups = getCatalogGroups();
							if (groups.length === 0) {
								return (
									<div className="px-2 py-1 text-xs text-muted-foreground">
										No nodes available
									</div>
								);
							}
							return groups.map((group) => (
								<div key={group.category} className="mb-2">
									<div className="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-2 py-1">
										{group.category}
									</div>
									{group.nodes.map((node) => (
										<button
											type="button"
											key={node.id}
											className="w-full text-left px-2 py-1 text-sm text-foreground hover:bg-muted-foreground/10 transition-colors duration-100 rounded"
											onClick={() => handleAddNode(node)}
										>
											{node.name}
										</button>
									))}
								</div>
							));
						})()
					) : (
						// Show delete option when right-clicking on node or edge
						<button
							type="button"
							className="w-full text-left px-2 py-1 text-sm text-red-400 hover:bg-slate-700 rounded"
							onClick={() => {
								if (
									contextMenuPosition.type === "node" &&
									contextMenuPosition.nodeId
								) {
									// Delete node and connected edges
									const nodeId = contextMenuPosition.nodeId;
									setEdges((eds) =>
										eds.filter(
											(edge) =>
												edge.source !== nodeId && edge.target !== nodeId,
										),
									);
									setNodes((nds) => nds.filter((node) => node.id !== nodeId));
									triggerOnChange();
								} else if (
									contextMenuPosition.type === "edge" &&
									contextMenuPosition.edgeId
								) {
									// Delete edge
									setEdges((eds) =>
										eds.filter(
											(edge) => edge.id !== contextMenuPosition.edgeId,
										),
									);
									triggerOnChange();
								}
								setContextMenuPosition(null);
							}}
						>
							Delete
						</button>
					)}
				</div>
			)}

			{contextMenuPosition && (
				// biome-ignore lint/a11y/noStaticElementInteractions: Backdrop click to dismiss is a standard UX pattern
				<div
					role="presentation"
					className="fixed inset-0 z-40"
					onClick={() => setContextMenuPosition(null)}
					onKeyDown={(e) => {
						if (e.key === "Escape") {
							setContextMenuPosition(null);
						}
					}}
				/>
			)}
		</div>
	);
}

// Wrapper component that provides ReactFlowProvider
export function ReactFlowEditorWrapper(
	props: ReactFlowEditorProps & {
		controllerRef?: React.MutableRefObject<EditorController | null>;
	},
) {
	return (
		<ReactFlowProvider>
			<ReactFlowEditor {...props} />
		</ReactFlowProvider>
	);
}
