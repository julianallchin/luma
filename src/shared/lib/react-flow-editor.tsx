import * as React from "react";
import ReactFlow, {
	addEdge,
	type Connection,
	type Edge,
	type Node,
	type ReactFlowInstance,
	ReactFlowProvider,
	useEdgesState,
	useNodesState,
} from "reactflow";
import "reactflow/dist/style.css";
import { Trash2 } from "lucide-react";
import type { Graph, NodeTypeDef, Signal } from "@/bindings/schema";
import {
	getNodeParamsSnapshot,
	removeNodeParams,
	replaceAllNodeParams,
	setNodeParamsSnapshot,
	useGraphStore,
} from "@/features/patterns/stores/use-graph-store";
import {
	type MelSpecData,
	useViewDataStore,
} from "@/features/patterns/stores/use-view-data-store";
import {
	Command,
	CommandEmpty,
	CommandGroup,
	CommandInput,
	CommandItem,
	CommandList,
} from "@/shared/components/ui/command";
import {
	Popover,
	PopoverAnchor,
	PopoverContent,
} from "@/shared/components/ui/popover";
import {
	findPort,
	makeIsValidConnection,
} from "./react-flow/connection-validation";
import { FilletConnectionLine, FilletEdge } from "./react-flow/fillet-edge";
import {
	buildNode,
	serializeParams,
	syncNodeIdCounter,
} from "./react-flow/node-builder";
import {
	AdsrNode,
	AudioInputNode,
	BeatEnvelopeNode,
	ColorNode,
	FalloffNode,
	FilterSelectionNode,
	FrequencyAmplitudeNode,
	GradientNode,
	InvertNode,
	MelSpecNode,
	NoiseNode,
	PaletteNode,
	RainbowNode,
	StandardNode,
	ThresholdNode,
	UvViewNode,
	ViewChannelNode,
} from "./react-flow/nodes";
import type {
	AudioInputNodeData,
	BaseNodeData,
	MelSpecNodeData,
	UvViewNodeData,
	ViewChannelNodeData,
} from "./react-flow/types";
import { DEFAULT_PORT_COLOR, PORT_TYPE_COLORS } from "./react-flow/types";

type AnyNodeData =
	| BaseNodeData
	| ViewChannelNodeData
	| UvViewNodeData
	| MelSpecNodeData
	| AudioInputNodeData;

// Get port type color for an edge
function getEdgeColor(nodes: Node<AnyNodeData>[], edge: Edge): string {
	const sourceNode = nodes.find((n) => n.id === edge.source);
	if (!sourceNode) return DEFAULT_PORT_COLOR;

	const port = findPort(sourceNode, edge.sourceHandle);
	if (!port) return DEFAULT_PORT_COLOR;

	return PORT_TYPE_COLORS[port.portType] ?? DEFAULT_PORT_COLOR;
}

// Editor component
export type EditorController = {
	addNode(definition: NodeTypeDef, position?: { x: number; y: number }): void;
	serialize(): Graph;
	loadGraph(graph: Graph, getNodeDefinitions: () => NodeTypeDef[]): void;
	updateViewData(
		views: Record<string, Signal>,
		melSpecs: Record<string, { width: number; height: number; data: number[] }>,
		colorViews: Record<string, string>,
	): void;
	updateNodeContext(context: { trackName?: string; timeLabel?: string }): void;
};

type ReactFlowEditorProps = {
	/** `structural` is false for param-only edits (slider drags etc.). */
	onChange: (change: { structural: boolean }) => void;
	getNodeDefinitions: () => NodeTypeDef[];
	controllerRef?: React.MutableRefObject<EditorController | null>;
	onReady?: () => void;
	readOnly?: boolean;
};

export function ReactFlowEditor({
	onChange,
	getNodeDefinitions,
	controllerRef,
	onReady,
	readOnly,
}: ReactFlowEditorProps) {
	const [nodes, setNodes, onNodesChange] = useNodesState<AnyNodeData>([]);
	const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
	const [reactFlowInstance, setReactFlowInstance] =
		React.useState<ReactFlowInstance | null>(null);
	const isLoadingRef = React.useRef(false);
	const pendingChangeRef = React.useRef(false);

	const isValidConnection = React.useMemo(
		() => makeIsValidConnection(nodes),
		[nodes],
	);

	const [connectionColor, setConnectionColor] = React.useState<string | null>(
		null,
	);
	const onConnectStart = React.useCallback<
		NonNullable<React.ComponentProps<typeof ReactFlow>["onConnectStart"]>
	>(
		(_event, { nodeId, handleId }) => {
			if (!nodeId) return;
			const node = nodes.find((n) => n.id === nodeId);
			if (!node) return;
			const port = findPort(node, handleId);
			if (!port) return;
			setConnectionColor(PORT_TYPE_COLORS[port.portType] ?? null);
		},
		[nodes],
	);
	const onConnectEnd = React.useCallback(() => {
		setConnectionColor(null);
	}, []);
	const connectionLineStyle = React.useMemo(
		() =>
			connectionColor ? { stroke: connectionColor, strokeWidth: 2 } : undefined,
		[connectionColor],
	);

	const nodeTypes = React.useMemo(
		() => ({
			standard: StandardNode,
			viewChannel: ViewChannelNode,
			uvView: UvViewNode,
			melSpec: MelSpecNode,
			audioInput: AudioInputNode,
			beatEnvelope: BeatEnvelopeNode,
			adsr: AdsrNode,
			color: ColorNode,
			palette: PaletteNode,
			gradient: GradientNode,
			noise: NoiseNode,
			rainbow: RainbowNode,
			threshold: ThresholdNode,
			falloff: FalloffNode,
			invert: InvertNode,
			filterSelection: FilterSelectionNode,
			frequencyAmplitude: FrequencyAmplitudeNode,
		}),
		[],
	);

	const edgeTypes = React.useMemo(() => ({ fillet: FilletEdge }), []);

	// Stable onChange ref to prevent infinite loops
	const onChangeRef = React.useRef(onChange);
	React.useEffect(() => {
		onChangeRef.current = onChange;
	}, [onChange]);

	// Throttle onChange with a leading edge so param drags (sliders, envelope
	// handles) re-execute the graph live, not only after the drag ends. A
	// trailing call guarantees the final value is never dropped. `structural`
	// flags topology changes (nodes/edges) vs param-only edits; it is OR-ed
	// across calls coalesced into one throttle window.
	const ONCHANGE_THROTTLE_MS = 50;
	const onChangeTimeoutRef = React.useRef<NodeJS.Timeout | null>(null);
	const lastOnChangeRef = React.useRef(Number.NEGATIVE_INFINITY);
	const pendingStructuralRef = React.useRef(false);
	const triggerOnChange = React.useCallback((structural = true) => {
		pendingStructuralRef.current = pendingStructuralRef.current || structural;
		const fire = () => {
			const wasStructural = pendingStructuralRef.current;
			pendingStructuralRef.current = false;
			lastOnChangeRef.current = performance.now();
			onChangeRef.current({ structural: wasStructural });
		};
		const elapsed = performance.now() - lastOnChangeRef.current;
		if (elapsed >= ONCHANGE_THROTTLE_MS) {
			fire();
		} else if (!onChangeTimeoutRef.current) {
			onChangeTimeoutRef.current = setTimeout(() => {
				onChangeTimeoutRef.current = null;
				fire();
			}, ONCHANGE_THROTTLE_MS - elapsed);
		}
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

				return { nodes: graphNodes, edges: graphEdges, args: [] };
			},
			loadGraph(graph: Graph, getNodeDefinitions: () => NodeTypeDef[]) {
				isLoadingRef.current = true;
				syncNodeIdCounter(graph.nodes.map((graphNode) => graphNode.id));
				const definitions = getNodeDefinitions();
				const defMap = new Map(definitions.map((def) => [def.id, def]));
				console.log("[ReactFlowEditor] loadGraph()", {
					nodes: graph.nodes.length,
					edges: graph.edges.length,
					definitions: definitions.length,
				});

				const paramEntries: Record<string, Record<string, unknown>> = {};
				for (const graphNode of graph.nodes) {
					paramEntries[graphNode.id] = serializeParams(graphNode.params ?? {});
				}
				replaceAllNodeParams(paramEntries);

				// Convert graph nodes to ReactFlow nodes
				const loadedNodes: Node<AnyNodeData>[] = graph.nodes
					.map((graphNode, index) => {
						const definition =
							defMap.get(graphNode.typeId) ??
							({
								id: graphNode.typeId,
								name: graphNode.typeId,
								description: null,
								category: "Unknown",
								inputs: [],
								outputs: [],
								params: [],
							} as NodeTypeDef);
						if (!defMap.has(graphNode.typeId)) {
							console.warn("[ReactFlowEditor] Unknown node type encountered", {
								typeId: graphNode.typeId,
								nodeId: graphNode.id,
							});
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
							definition.id === "view_channel" ||
							definition.id === "view_signal" ||
							definition.id === "view_events"
								? "viewChannel"
								: definition.id === "view_uv"
									? "uvView"
									: definition.id === "mel_spec_viewer"
										? "melSpec"
										: definition.id === "audio_input"
											? "audioInput"
											: definition.id === "beat_envelope"
												? "beatEnvelope"
												: definition.id === "adsr"
													? "adsr"
													: definition.id === "color"
														? "color"
														: definition.id === "palette"
															? "palette"
															: definition.id === "gradient"
																? "gradient"
																: definition.id === "noise"
																	? "noise"
																	: definition.id === "rainbow"
																		? "rainbow"
																		: definition.id === "threshold"
																			? "threshold"
																			: definition.id === "frequency_amplitude"
																				? "frequencyAmplitude"
																				: definition.id === "falloff"
																					? "falloff"
																					: definition.id === "filter_selection"
																						? "filterSelection"
																						: definition.id === "invert"
																							? "invert"
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
							};
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: viewData,
							} as Node<ViewChannelNodeData>;
						} else if (nodeType === "uvView") {
							const uvData: UvViewNodeData = {
								...baseData,
								viewSamples: null,
							};
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: uvData,
							} as Node<UvViewNodeData>;
						} else if (nodeType === "melSpec") {
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
						} else if (nodeType === "frequencyAmplitude") {
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: baseData,
							} as Node<BaseNodeData>;
						} else if (nodeType === "threshold") {
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: baseData,
							} as Node<BaseNodeData>;
						} else {
							// Default case
							return {
								id: graphNode.id,
								type: nodeType,
								position,
								data: baseData,
							} as Node<BaseNodeData>;
						}
					})
					.filter((node): node is Node<AnyNodeData> => node !== null);

				// Convert graph edges to ReactFlow edges with colors
				const loadedEdges: Edge[] = graph.edges.map((graphEdge) => {
					const edge: Edge = {
						id: graphEdge.id,
						source: graphEdge.fromNode,
						target: graphEdge.toNode,
						sourceHandle: graphEdge.fromPort,
						targetHandle: graphEdge.toPort,
						type: "fillet",
					};
					const color = getEdgeColor(loadedNodes, edge);
					return {
						...edge,
						style: { stroke: color, strokeWidth: 2 },
					};
				});

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
			updateViewData(views, melSpecs, colorViews) {
				// Results go through the view-data store, NOT setNodes: they
				// stream at ~20 Hz during param drags, and rebuilding the nodes
				// array re-rendered the entire editor per result. View nodes
				// subscribe to their own slice.
				useViewDataStore
					.getState()
					.setResults(
						views,
						melSpecs as Record<string, MelSpecData>,
						colorViews,
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

	// Update edge colors when nodes change (in case port types change).
	// Preserve object identity when the color is unchanged — nodes update on
	// every live view-data refresh, and new edge objects would force ReactFlow
	// to re-render every edge each time.
	React.useEffect(() => {
		setEdges((eds) => {
			let changed = false;
			const next = eds.map((edge) => {
				const color = getEdgeColor(nodes, edge);
				const prev = (edge.style as { stroke?: string } | undefined)?.stroke;
				if (prev === color) return edge;
				changed = true;
				return {
					...edge,
					style: { stroke: color, strokeWidth: 2 },
				};
			});
			return changed ? next : eds;
		});
	}, [nodes, setEdges]);

	// Detect graph changes worth re-executing, excluding positions. Topology
	// (nodes/edges) and params are compared separately so param-only edits can
	// skip expensive structural work downstream (e.g. mel spec recompute).
	const prevTopoRef = React.useRef<string>("");
	const prevParamsRef = React.useRef<string>("");
	const detectGraphChange = React.useCallback(() => {
		const topo = JSON.stringify({
			nodes: nodesRef.current.map((n) => ({
				id: n.id,
				typeId: n.data.typeId,
			})),
			edges: edgesRef.current.map((e) => ({
				id: e.id,
				source: e.source,
				target: e.target,
				sourceHandle: e.sourceHandle,
				targetHandle: e.targetHandle,
			})),
		});
		const params = JSON.stringify(
			nodesRef.current.map((n) => getNodeParamsSnapshot(n.id)),
		);
		const structural = topo !== prevTopoRef.current;
		if (!structural && params === prevParamsRef.current) return;
		prevTopoRef.current = topo;
		prevParamsRef.current = params;
		if (isLoadingRef.current) {
			pendingChangeRef.current = true;
		} else {
			triggerOnChange(structural);
		}
	}, [triggerOnChange]);
	// Topology changes arrive through React state…
	React.useEffect(() => {
		detectGraphChange();
	}, [nodes, edges, detectGraphChange]);
	// …param edits through the store, subscribed without re-rendering the
	// editor (a slider drag emits a change per pointermove).
	React.useEffect(
		() =>
			useGraphStore.subscribe((state, prev) => {
				if (state.version !== prev.version) detectGraphChange();
			}),
		[detectGraphChange],
	);

	// Node drag stop - don't trigger onChange since positions don't affect execution
	const onNodeDragStop = React.useCallback(() => {
		// No-op: positions don't affect execution
	}, []);

	// Handle connections
	const applyEdgeColors = React.useCallback((eds: Edge[]) => {
		return eds.map((edge) => {
			const color = getEdgeColor(nodesRef.current, edge);
			return {
				...edge,
				style: { stroke: color, strokeWidth: 2 },
			};
		});
	}, []);

	/**
	 * When the user inserts a node between two nodes by wiring A -> N and N -> B,
	 * automatically remove any existing direct A -> B edge (matching handles where possible).
	 */
	const removeDirectEdgesIfSplit = React.useCallback(
		(connection: Connection, eds: Edge[]) => {
			const source = connection.source;
			const target = connection.target;
			if (!source || !target) return eds;

			const removeCandidates = new Set<string>();

			const considerSplit = (
				fromNode: string,
				middleNode: string,
				toNode: string,
			) => {
				// Remove direct fromNode -> toNode edges, but only when the graph has
				// both fromNode -> middleNode and middleNode -> toNode connections.
				const inEdges = eds.filter(
					(e) => e.source === fromNode && e.target === middleNode,
				);
				const outEdges = eds.filter(
					(e) => e.source === middleNode && e.target === toNode,
				);
				if (inEdges.length === 0 || outEdges.length === 0) return;

				const directEdges = eds.filter(
					(e) => e.source === fromNode && e.target === toNode,
				);
				if (directEdges.length === 0) return;

				for (const inEdge of inEdges) {
					for (const outEdge of outEdges) {
						for (const directEdge of directEdges) {
							// Match handles when specified to avoid removing a different parallel edge.
							const sourceHandleMatches =
								!inEdge.sourceHandle ||
								!directEdge.sourceHandle ||
								directEdge.sourceHandle === inEdge.sourceHandle;
							const targetHandleMatches =
								!outEdge.targetHandle ||
								!directEdge.targetHandle ||
								directEdge.targetHandle === outEdge.targetHandle;

							if (sourceHandleMatches && targetHandleMatches) {
								removeCandidates.add(directEdge.id);
							}
						}
					}
				}
			};

			// If we just connected A -> N, see if N already connects to some B.
			{
				const fromNode = source;
				const middleNode = target;
				const outgoing = eds.filter((e) => e.source === middleNode);
				for (const outEdge of outgoing) {
					considerSplit(fromNode, middleNode, outEdge.target);
				}
			}

			// If we just connected N -> B, see if some A already connects to N.
			{
				const middleNode = source;
				const toNode = target;
				const incoming = eds.filter((e) => e.target === middleNode);
				for (const inEdge of incoming) {
					considerSplit(inEdge.source, middleNode, toNode);
				}
			}

			if (removeCandidates.size === 0) return eds;
			return eds.filter((e) => !removeCandidates.has(e.id));
		},
		[],
	);

	const onConnect = React.useCallback(
		(params: Connection) => {
			setEdges((eds) => {
				// An input is a scalar slot, not a multi-edge collection. Rewiring it
				// replaces the previous source so serialization, compilation, and the
				// authored-state merge key `(target, targetHandle)` all agree.
				const withoutPreviousInput = eds.filter(
					(edge) =>
						edge.target !== params.target ||
						edge.targetHandle !== params.targetHandle,
				);
				let nextEdges = addEdge(params, withoutPreviousInput);
				nextEdges = removeDirectEdgesIfSplit(params, nextEdges);
				const coloredEdges = applyEdgeColors(nextEdges);
				triggerOnChange();
				return coloredEdges;
			});
		},
		[setEdges, triggerOnChange, removeDirectEdgesIfSplit, applyEdgeColors],
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
		<div className="w-full h-full relative bg-trim">
			<ReactFlow
				nodes={nodes}
				edges={edges}
				onNodesChange={(changes) => {
					// Only pattern_args is synthetic (its ports mirror the args panel);
					// audio_input is no longer auto-added, so it must be deletable
					// like any other node (old graphs may still contain one).
					const PROTECTED_NODE_TYPES = new Set(["pattern_args"]);
					const filtered = changes.filter((change) => {
						if (change.type === "remove" && change.id) {
							const node = nodesRef.current.find((n) => n.id === change.id);
							if (node && PROTECTED_NODE_TYPES.has(node.data.typeId)) {
								return false; // Prevent removing the synthetic args node
							}
							removeNodeParams(change.id);
						}
						return true;
					});
					if (filtered.length > 0) {
						onNodesChange(filtered);
					}
				}}
				onEdgesChange={onEdgesChange}
				onNodeDragStop={onNodeDragStop}
				onConnect={onConnect}
				onConnectStart={onConnectStart}
				onConnectEnd={onConnectEnd}
				connectionLineComponent={FilletConnectionLine}
				connectionLineStyle={connectionLineStyle}
				isValidConnection={isValidConnection}
				nodeTypes={nodeTypes}
				edgeTypes={edgeTypes}
				defaultEdgeOptions={{ type: "fillet" }}
				onInit={setReactFlowInstance}
				onPaneContextMenu={readOnly ? undefined : onPaneContextMenu}
				onNodeContextMenu={readOnly ? undefined : onNodeContextMenu}
				onEdgeContextMenu={readOnly ? undefined : onEdgeContextMenu}
				nodesDraggable={!readOnly}
				nodesConnectable={!readOnly}
				elementsSelectable={!readOnly}
				maxZoom={8}
				fitView
				proOptions={{ hideAttribution: true }}
			/>

			<Popover
				open={contextMenuPosition !== null}
				onOpenChange={(open) => {
					if (!open) setContextMenuPosition(null);
				}}
			>
				<PopoverAnchor
					className="fixed"
					style={{
						left: contextMenuPosition?.x ?? 0,
						top: contextMenuPosition?.y ?? 0,
					}}
				/>
				<PopoverContent
					className="p-0 w-auto"
					align="start"
					sideOffset={0}
					onOpenAutoFocus={(e) => {
						// Prevent default focus behavior to allow CommandInput to handle it
						e.preventDefault();
					}}
				>
					{contextMenuPosition?.type === "pane" ? (
						// Show node catalog when right-clicking on pane
						<Command
							className="rounded-lg border-none w-[250px]"
							filter={(value, search) => {
								// Parse the value: format is "nodeName | category"
								const delimiter = " | ";
								const delimiterIndex = value.indexOf(delimiter);
								if (delimiterIndex === -1) {
									// Fallback: if no delimiter, treat entire value as node name
									return value.toLowerCase().includes(search.toLowerCase())
										? 1
										: 0;
								}
								const nodeName = value.slice(0, delimiterIndex);
								const category = value.slice(delimiterIndex + delimiter.length);
								const searchLower = search.toLowerCase();
								const nodeNameLower = nodeName.toLowerCase();
								const categoryLower = category.toLowerCase();

								// Prioritize node name matches
								if (nodeNameLower.includes(searchLower)) {
									// Higher score for matches at the start of the node name
									if (nodeNameLower.startsWith(searchLower)) {
										return 2;
									}
									return 1;
								}
								// Lower priority for category matches
								if (categoryLower.includes(searchLower)) {
									return 0.5;
								}
								// No match
								return 0;
							}}
						>
							<CommandInput
								placeholder="Search nodes..."
								className="h-9"
								autoFocus
							/>
							<CommandList className="max-h-[300px]">
								<CommandEmpty>No nodes found.</CommandEmpty>
								{getCatalogGroups().map((group) => (
									<CommandGroup key={group.category} heading={group.category}>
										{group.nodes.map((node) => (
											<CommandItem
												key={node.id}
												value={`${node.name} | ${group.category}`}
												onSelect={() => handleAddNode(node)}
											>
												{node.name}
											</CommandItem>
										))}
									</CommandGroup>
								))}
							</CommandList>
						</Command>
					) : (
						// Show delete option when right-clicking on node or edge
						<div className="min-w-[8rem] p-1">
							<button
								type="button"
								className="flex w-full cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-sm text-destructive hover:bg-destructive/10"
								onClick={() => {
									if (
										contextMenuPosition?.type === "node" &&
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
										contextMenuPosition?.type === "edge" &&
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
								<Trash2 className="size-4" />
								Delete
							</button>
						</div>
					)}
				</PopoverContent>
			</Popover>
		</div>
	);
}

// Wrapper component that provides ReactFlowProvider
export function ReactFlowEditorWrapper(
	props: ReactFlowEditorProps & {
		controllerRef?: React.MutableRefObject<EditorController | null>;
		readOnly?: boolean;
	},
) {
	return (
		<ReactFlowProvider>
			<ReactFlowEditor {...props} />
		</ReactFlowProvider>
	);
}
