import * as React from "react";
import { createRoot } from "react-dom/client";
import { ClassicPreset, NodeEditor } from "rete";
import { AreaExtensions, AreaPlugin } from "rete-area-plugin";
import {
	ConnectionPlugin,
	Presets as ConnectionPresets,
} from "rete-connection-plugin";
import { ContextMenuPlugin } from "rete-context-menu-plugin";
import type { ClassicScheme, ReactArea2D } from "rete-react-plugin";
import { ReactPlugin, Presets as ReactPresets } from "rete-react-plugin";
import type { Graph, NodeTypeDef, PortType, Series } from "../bindings/schema";

type Schemes = ClassicScheme;
type AreaExtra = ReactArea2D<Schemes>;

type NodeMeta = {
	typeId: string;
	params: Record<string, unknown>;
	definition: NodeTypeDef;
	viewSamples?: number[] | null;
	previewControl?: ViewPreviewControl;
};

type EditorNode = Schemes["Node"] & { meta: NodeMeta };

export type EditorController = {
	readonly editor: NodeEditor<Schemes>;
	addNode(
		definition: NodeTypeDef,
		position?: { x: number; y: number },
	): Promise<EditorNode>;
	serialize(): Graph;
	updateViewData(
		views: Record<string, number[]>,
		seriesViews: Record<string, Series>,
	): Promise<void>;
	destroy(): Promise<void>;
};

type CreateEditorOptions = {
	onChange: () => void;
	getNodeDefinitions: () => NodeTypeDef[];
};

type CatalogGroup = {
	category: string;
	nodes: NodeTypeDef[];
};

const intensitySocket = new ClassicPreset.Socket("Intensity");
const audioSocket = new ClassicPreset.Socket("Audio");
const beatSocket = new ClassicPreset.Socket("BeatGrid");
const seriesSocket = new ClassicPreset.Socket("Series");
const VIEW_NODE_WIDTH = 220;
const VIEW_NODE_HEIGHT = 160;
const VIEW_SAMPLE_LIMIT = 128;

class ViewPreviewControl extends ClassicPreset.Control {
	private samples: number[] = [];

	constructor() {
		super();
		this.index = 10;
	}

	setSamples(samples: number[] | null) {
		this.samples = samples ?? [];
	}

	getSamples(): number[] {
		return this.samples;
	}
}

const ViewPreviewControlComponent: React.FC<{
	control: ViewPreviewControl;
}> = ({ control }) => {
	const samples = control.getSamples() ?? [];
	const limited = React.useMemo(
		() => samples.slice(0, VIEW_SAMPLE_LIMIT),
		[samples],
	);
	const points = React.useMemo(() => {
		if (limited.length === 0) return "";
		const denom = Math.max(1, limited.length - 1);
		return limited
			.map((value, index) => {
				const clamped = Math.max(0, Math.min(1, value || 0));
				const x = (index / denom) * 100;
				const y = 100 - clamped * 90;
				return `${x.toFixed(3)},${y.toFixed(3)}`;
			})
			.join(" ");
	}, [limited]);

	return (
		<div className="rounded-md bg-slate-900/70 p-2 text-[11px] text-slate-200 shadow-inner">
			{limited.length > 0 ? (
				<svg
					viewBox="0 0 100 100"
					className="h-24 w-full"
					role="img"
					aria-label="Intensity preview waveform"
				>
					<rect
						x="0"
						y="0"
						width="100"
						height="100"
						rx="4"
						className="fill-slate-800/60"
					/>
					<polyline
						points={points}
						className="fill-none stroke-emerald-400"
						strokeWidth={2}
						strokeLinejoin="round"
					/>
				</svg>
			) : (
				<p className="text-center text-[11px] text-slate-400">
					waiting for signal…
				</p>
			)}
		</div>
	);
};

function groupNodeTypes(definitions: NodeTypeDef[]): CatalogGroup[] {
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
}

function resolveSocket(type: PortType): ClassicPreset.Socket {
	switch (type) {
		case "Audio":
			return audioSocket;
		case "BeatGrid":
			return beatSocket;
		case "Series":
			return seriesSocket;
		case "Intensity":
		default:
			return intensitySocket;
	}
}

function applyParamDefaults(node: EditorNode, onChange: () => void) {
	const { definition } = node.meta;

	for (const param of definition.params) {
		if (param.paramType === "Number") {
			const initial = param.defaultNumber ?? 0;
			node.meta.params[param.id] = initial;
			const control = new ClassicPreset.InputControl("number", {
				initial,
				change: (value) => {
					node.meta.params[param.id] = value ?? 0;
					onChange();
				},
			});
			node.addControl(param.id, control);
		} else if (param.paramType === "Text") {
			const initial = param.defaultText ?? "";
			node.meta.params[param.id] = initial;
			const control = new ClassicPreset.InputControl("text", {
				initial,
				change: (value) => {
					node.meta.params[param.id] = value ?? "";
					onChange();
				},
			});
			node.addControl(param.id, control);
		}
	}
}

function buildNode(definition: NodeTypeDef, onChange: () => void): EditorNode {
	const node = new ClassicPreset.Node(definition.name) as EditorNode;
	node.meta = {
		typeId: definition.id,
		definition,
		params: {},
		viewSamples: null,
	};

	for (const input of definition.inputs) {
		const socket = resolveSocket(input.portType);
		node.addInput(input.id, new ClassicPreset.Input(socket, input.name));
	}

	for (const output of definition.outputs) {
		const socket = resolveSocket(output.portType);
		node.addOutput(output.id, new ClassicPreset.Output(socket, output.name));
	}

	applyParamDefaults(node, onChange);

	if (definition.id === "view_channel") {
		const sizedNode = node as EditorNode & { width?: number; height?: number };
		sizedNode.width = VIEW_NODE_WIDTH;
		sizedNode.height = VIEW_NODE_HEIGHT;
		const previewControl = new ViewPreviewControl();
		node.addControl("preview", previewControl);
		node.meta.previewControl = previewControl;
		previewControl.setSamples(node.meta.viewSamples ?? null);
	}

	return node;
}

function serializeParams(params: Record<string, unknown>) {
	return Object.keys(params).reduce<Record<string, unknown>>((acc, key) => {
		const value = params[key];
		if (value !== undefined) {
			acc[key] = value;
		}
		return acc;
	}, {});
}

export async function createEditor(
	container: HTMLElement,
	options: CreateEditorOptions,
): Promise<EditorController> {
	const editor = new NodeEditor<Schemes>();
	const area = new AreaPlugin<Schemes, AreaExtra>(container);
	const render = new ReactPlugin<Schemes, AreaExtra>({ createRoot });
	const connection = new ConnectionPlugin<Schemes, AreaExtra>();
	const selector = AreaExtensions.selector();
	const accumulating = AreaExtensions.accumulateOnCtrl();
	const contextMenu = new ContextMenuPlugin<Schemes>({
		items(context, plugin) {
			const definitions = options.getNodeDefinitions();
			const areaScope = plugin.parentScope(AreaPlugin) as AreaPlugin<
				Schemes,
				AreaExtra
			>;
			const editorScope = areaScope.parentScope(
				NodeEditor,
			) as NodeEditor<Schemes>;

			if (context === "root") {
				const groups = groupNodeTypes(definitions);
				const hasMultipleGroups = groups.length > 1;
				let list: Array<{
					label: string;
					key: string;
					handler: () => void | Promise<void>;
					subitems?: Array<{
						label: string;
						key: string;
						handler: () => void | Promise<void>;
					}>;
				}> = [];

				if (groups.length === 0) {
					list = [];
				} else if (!hasMultipleGroups) {
					list = groups[0].nodes.map((definition, index) => ({
						label: definition.name,
						key: `node-${index}`,
						handler: async () => {
							const node = buildNode(definition, options.onChange);
							await editorScope.addNode(node);
							await areaScope.translate(node.id, areaScope.area.pointer);
						},
					}));
				} else {
					list = groups.map((group, groupIndex) => ({
						label: group.category,
						key: `group-${groupIndex}`,
						handler: () => {},
						subitems: group.nodes.map((definition, nodeIndex) => ({
							label: definition.name,
							key: `node-${groupIndex}-${nodeIndex}`,
							handler: async () => {
								const node = buildNode(definition, options.onChange);
								await editorScope.addNode(node);
								await areaScope.translate(node.id, areaScope.area.pointer);
							},
						})),
					}));
				}

				return {
					searchBar: true,
					list,
				};
			}

			const deleteItem = {
				label: "Delete",
				key: "delete",
				handler: async () => {
					if (
						typeof context === "object" &&
						context &&
						"source" in context &&
						"target" in context
					) {
						await editorScope.removeConnection(context.id);
						return;
					}

					const nodeId = (context as Schemes["Node"]).id;
					const connections = editorScope
						.getConnections()
						.filter(
							(connectionData) =>
								connectionData.source === nodeId ||
								connectionData.target === nodeId,
						);

					for (const connectionData of connections) {
						await editorScope.removeConnection(connectionData.id);
					}

					await editorScope.removeNode(nodeId);
				},
			};

			return {
				searchBar: false,
				list: [deleteItem],
			};
		},
	});

	render.addPreset(
		ReactPresets.classic.setup({
			customize: {
				control(context) {
					const payload = (context as any)?.data?.payload;
					if (payload instanceof ViewPreviewControl) {
						return () => <ViewPreviewControlComponent control={payload} />;
					}
					return null;
				},
			},
		}),
	);
	render.addPreset(ReactPresets.contextMenu.setup() as any);
	connection.addPreset(ConnectionPresets.classic.setup());

	editor.use(area);
	area.use(render);
	area.use(connection);
	area.use(contextMenu as any);

	AreaExtensions.selectableNodes(area, selector, { accumulating });

	editor.addPipe(async (context) => {
		switch (context.type) {
			case "nodecreated":
			case "noderemoved":
			case "connectioncreated":
			case "connectionremoved":
				options.onChange();
				break;
		}
		return context;
	});

	const handleKeyDown = async (event: KeyboardEvent) => {
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
		const removals: Array<Promise<unknown>> = [];
		for (const entity of selector.entities.values()) {
			removals.push(editor.removeNode(entity.id));
		}
		await Promise.all(removals);
	};

	window.addEventListener("keydown", handleKeyDown);

	const controller: EditorController = {
		editor,
		async addNode(definition, position) {
			const node = buildNode(definition, options.onChange);
			await editor.addNode(node);

			if (position) {
				await area.translate(node.id, position);
			}

			options.onChange();
			return node;
		},
		serialize() {
			const nodes = editor.getNodes().flatMap((node) => {
				const meta = (node as EditorNode).meta;
				if (!meta || !meta.typeId) {
					return [];
				}
				return [
					{
						id: node.id,
						typeId: meta.typeId,
						params: serializeParams(meta.params ?? {}),
						positionX: null,
						positionY: null,
					},
				];
			});

			const edges = editor.getConnections().map((connectionData) => ({
				id: connectionData.id,
				fromNode: connectionData.source,
				fromPort: String(connectionData.sourceOutput),
				toNode: connectionData.target,
				toPort: String(connectionData.targetInput),
			}));

			return { nodes, edges } satisfies Graph;
		},
		async updateViewData(views, _seriesViews) {
			const viewPromises: Promise<void>[] = [];
			for (const node of editor.getNodes()) {
				const editorNode = node as EditorNode;
				if (editorNode.meta.typeId === "view_channel") {
					const samples = views[node.id] ?? null;
					editorNode.meta.viewSamples = samples;
					editorNode.meta.previewControl?.setSamples(samples);
					viewPromises.push(area.update("node", node.id));
				}
			}
			await Promise.allSettled(viewPromises);
		},
		async destroy() {
			await editor.clear();
			area.destroy();
			window.removeEventListener("keydown", handleKeyDown);
		},
	};

	return controller;
}
