import * as React from "react";
import ReactFlow, {
	addEdge,
	Background,
	type Connection,
	type Edge,
	MarkerType,
	type Node,
	type ReactFlowInstance,
	ReactFlowProvider,
	useEdgesState,
	useNodesState,
} from "reactflow";
import "reactflow/dist/style.css";
import { Trash2 } from "lucide-react";
import type { Graph, NodeTypeDef, PortType, Signal } from "@/bindings/schema";
import {
	getNodeParamsSnapshot,
	removeNodeParams,
	replaceAllNodeParams,
	setNodeParamsSnapshot,
	useGraphStore,
} from "@/features/patterns/stores/use-graph-store";
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
import {
	buildNode,
	serializeParams,
	syncNodeIdCounter,
} from "./react-flow/node-builder";
import {
	AudioInputNode,
	BeatClockNode,
	BeatEnvelopeNode,
	ColorNode,
	FalloffNode,
	FilterSelectionNode,
	FrequencyAmplitudeNode,
	GetAttributeNode,
	GradientNode,
	InvertNode,
	MathNode,
	MelSpecNode,
	SelectNode,
	StandardNode,
	ThresholdNode,
	UvViewNode,
	ViewChannelNode,
} from "./react-flow/nodes";
import type {
	AudioInputNodeData,
	BaseNodeData,
	BeatClockNodeData,
	MelSpecNodeData,
	UvViewNodeData,
	ViewChannelNodeData,
} from "./react-flow/types";

type AnyNodeData =
	| BaseNodeData
	| ViewChannelNodeData
	| UvViewNodeData
	| MelSpecNodeData
	| AudioInputNodeData
	| BeatClockNodeData;

// Color mapping for port types
const PORT_TYPE_COLORS: Record<PortType, string> = {
	Intensity: "#f59e0b", // amber-500
	Audio: "#3b82f6", // blue-500
	BeatGrid: "#10b981", // emerald-500
	Series: "#8b5cf6", // violet-500 (Legacy/Viewers)
	Color: "#ec4899", // pink-500
	Signal: "#22d3ee", // cyan-400
	Selection: "#c084fc", // purple-400
	Gradient: "#f472b6", // pink-400
};

// Get port type color for an edge
function getEdgeColor(nodes: Node<AnyNodeData>[], edge: Edge): string {
	const sourceNode = nodes.find((n) => n.id === edge.source);
	if (!sourceNode) return "#6b7280"; // gray-500 default

	const port = findPort(sourceNode, edge.sourceHandle);
	if (!port) return "#6b7280"; // gray-500 default

	return PORT_TYPE_COLORS[port.portType] ?? "#6b7280";
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
			uvView: UvViewNode,
			melSpec: MelSpecNode,
			audioInput: AudioInputNode,
			beatClock: BeatClockNode,
			beatEnvelope: BeatEnvelopeNode,
			color: ColorNode,
			gradient: GradientNode,
			math: MathNode,
			threshold: ThresholdNode,
			falloff: FalloffNode,
			invert: InvertNode,
			filterSelection: FilterSelectionNode,
			getAttribute: GetAttributeNode,
			frequencyAmplitude: FrequencyAmplitudeNode,
			select: SelectNode,
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
							definition.id === "view_signal"
								? "viewChannel"
								: definition.id === "view_uv"
									? "uvView"
									: definition.id === "mel_spec_viewer"
										? "melSpec"
										: definition.id === "audio_input"
											? "audioInput"
											: definition.id === "beat_clock"
												? "beatClock"
												: definition.id === "beat_envelope"
													? "beatEnvelope"
													: definition.id === "color"
														? "color"
														: definition.id === "gradient"
															? "gradient"
															: definition.id === "math"
																? "math"
																: definition.id === "threshold"
																	? "threshold"
																	: definition.id === "select"
																		? "select"
																		: definition.id === "frequency_amplitude"
																			? "frequencyAmplitude"
																			: definition.id === "falloff"
																				? "falloff"
																				: definition.id === "get_attribute"
																					? "getAttribute"
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
						} else if (nodeType === "math") {
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
					};
					const color = getEdgeColor(loadedNodes, edge);
					return {
						...edge,
						style: { stroke: color },
						markerEnd: {
							type: MarkerType.ArrowClosed,
							color,
						},
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
			updateViewData(views, melSpecs, _colorViews) {
				setNodes((nds) =>
					nds.map((node) => {
						if (
							node.data.typeId === "view_channel" ||
							node.data.typeId === "view_signal" ||
							node.data.typeId === "view_uv"
						) {
							const samples = views[node.id] ?? null;
							return {
								...node,
								data: {
									...node.data,
									viewSamples: samples,
								} as ViewChannelNodeData | UvViewNodeData,
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

	// Update edge colors when nodes change (in case port types change)
	React.useEffect(() => {
		setEdges((eds) =>
			eds.map((edge) => {
				const color = getEdgeColor(nodes, edge);
				return {
					...edge,
					style: { stroke: color },
					markerEnd: {
						type: MarkerType.ArrowClosed,
						color,
					},
				};
			}),
		);
	}, [nodes, setEdges]);

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
	const applyEdgeColors = React.useCallback((eds: Edge[]) => {
		return eds.map((edge) => {
			const color = getEdgeColor(nodesRef.current, edge);
			return {
				...edge,
				style: { stroke: color },
				markerEnd: {
					type: MarkerType.ArrowClosed,
					color,
				},
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
				let nextEdges = addEdge(params, eds);
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
		<div className="w-full h-full relative">
			<ReactFlow
				nodes={nodes}
				edges={edges}
				onNodesChange={(changes) => {
					const REQUIRED_NODE_TYPES = new Set(["audio_input", "beat_clock"]);
					const filtered = changes.filter((change) => {
						if (change.type === "remove" && change.id) {
							const node = nodesRef.current.find((n) => n.id === change.id);
							if (node && REQUIRED_NODE_TYPES.has(node.data.typeId)) {
								return false; // Prevent removing required nodes
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
				isValidConnection={isValidConnection}
				nodeTypes={nodeTypes}
				onInit={setReactFlowInstance}
				onPaneContextMenu={readOnly ? undefined : onPaneContextMenu}
				onNodeContextMenu={readOnly ? undefined : onNodeContextMenu}
				onEdgeContextMenu={readOnly ? undefined : onEdgeContextMenu}
				nodesDraggable={!readOnly}
				nodesConnectable={!readOnly}
				elementsSelectable={!readOnly}
				maxZoom={1.2}
				fitView
				proOptions={{ hideAttribution: true }}
			>
				<Background gap={20} />
			</ReactFlow>

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
