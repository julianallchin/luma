import { z } from "zod";
import type {
	AnnotationPreview,
	Edge,
	Graph,
	NodeInstance,
	NodeTypeDef,
	PatternArgDef,
	PortDef,
	RunResult,
	Signal,
} from "@/bindings/schema";
import { previewToPngBase64 } from "@/features/track-editor/agent/preview-image";
import { tool } from "@/shared/lib/agent/agent-tool";
import { buildAskVenueTool } from "@/shared/lib/agent/ask-venue-tool";
import { buildPythonTool } from "@/shared/lib/agent/python-tool";
import { paramOptions } from "@/shared/lib/param-options";
import { toSnakeCase } from "@/shared/lib/utils";
import {
	renderGraphView,
	renderSubgraph,
	renderTypeCatalog,
} from "./graph-view";

/** Everything the graph tools need to read and mutate the live editor.
 *
 * Edits are applied live: every mutator updates the working graph and calls
 * `applyGraph`, which reloads the canvas (and re-runs it for the visualizer).
 * `runGraph` is the agent-facing execution that returns results / compile
 * errors for review. */
export type GraphAgentBindings = {
	/** The durable thread this turn belongs to — owns the Python workspace. */
	threadId: string;
	/** The durable user message that began this model turn. */
	turnMessageId: string;
	/** The turn's abort signal; stopping the model interrupts the Python cell. */
	abortSignal?: AbortSignal;
	getGraph: () => Graph;
	applyGraph: (graph: Graph) => void | Promise<void>;
	runGraph: (graph: Graph) => Promise<RunResult>;
	getNodeDefs: () => NodeTypeDef[];
	/** Preview span [startSec, endSec] — the Python scope's window. */
	getSpan: () => [number, number];
	getPatternId: () => string | null;
	getImplementationId: () => string | null;
	/** Track id of the current preview context. */
	getTrackId: () => string | null;
	/** Render the graph to a space-time heatmap (rows=fixtures, cols=time). */
	previewImage: (graph: Graph) => Promise<AnnotationPreview>;
	/** Overwrite the pattern's args entirely. */
	setArgs: (args: PatternArgDef[]) => void | Promise<void>;
	/** Set the preview-only selection (null → revert to the pattern's `all`). */
	setPreviewSelection: (expression: string | null) => void;
	getVenueId: () => string | null;
};

/** pattern_args is synthetic (its ports mirror the args panel) — the agent may
 * wire FROM it but must not delete, replace, or reparametrize it. */
const PROTECTED_TYPES = new Set(["pattern_args"]);

/** Keep the synthetic argument node and its edges consistent with the public
 * pattern interface. Both the mounted editor and detached child workspaces use
 * this exact normalization path. */
export function withPatternArgsNode(
	graph: Graph,
	args: PatternArgDef[],
): Graph {
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
	const hasNode = nodes.some((node) => node.typeId === "pattern_args");
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

	const validArgIds = new Set(args.map((arg) => arg.id));
	return {
		...graph,
		nodes,
		edges: filteredEdges.filter(
			(edge) =>
				edge.fromNode !== "pattern_args" || validArgIds.has(edge.fromPort),
		),
		args,
	};
}

function edgeId(e: Omit<Edge, "id">): string {
	return `${e.fromNode}:${e.fromPort}->${e.toNode}:${e.toPort}`;
}

function findOutPort(
	def: NodeTypeDef | undefined,
	portId: string,
): PortDef | undefined {
	return def?.outputs.find((p) => p.id === portId);
}
function findInPort(
	def: NodeTypeDef | undefined,
	portId: string,
): PortDef | undefined {
	return def?.inputs.find((p) => p.id === portId);
}

export function buildGraphAgentTools(b: GraphAgentBindings) {
	const defMap = () => new Map(b.getNodeDefs().map((d) => [d.id, d]));

	const graphView = tool({
		description:
			"Show the full working graph as text. Node ids (e.g. apply_color_1) are the handle you use in every edit tool. Format: <id> <Type>(<params>) -> <target_id>.<inport>. [out] marks a sink. Call this first to see what exists.",
		inputSchema: z.object({}),
		execute: async () => ({
			view: renderGraphView(b.getGraph(), b.getNodeDefs()),
		}),
	});

	const getSubgraph = tool({
		description:
			"Show only the neighborhood around a node, out to `depth` hops in both directions. Use on large graphs instead of graph_view.",
		inputSchema: z.object({
			id: z.string().describe("Node id to center on."),
			depth: z
				.number()
				.int()
				.min(1)
				.max(5)
				.optional()
				.describe("Hops. Default 2."),
		}),
		execute: async ({ id, depth }) => ({
			view: renderSubgraph(b.getGraph(), b.getNodeDefs(), id, depth ?? 2),
		}),
	});

	const listTypes = tool({
		description:
			"List every node type with its input/output ports (and port types) and params. Ports only connect when their PortType matches EXACTLY. Consult before wiring.",
		inputSchema: z.object({}),
		execute: async () => ({ catalog: renderTypeCatalog(b.getNodeDefs()) }),
	});

	const addNode = tool({
		description:
			"Add a node. `id` is the handle you'll reference later — pick something readable like `apply_color_2` (must be unique). `type` is a node type id from list_types. `params` overrides defaults.",
		inputSchema: z.object({
			id: z.string().describe("Unique node id / handle, e.g. apply_color_2."),
			type: z.string().describe("Node type id from list_types."),
			params: z.record(z.string(), z.unknown()).optional(),
		}),
		execute: async ({ id, type, params }) => {
			const graph = b.getGraph();
			if (graph.nodes.some((n) => n.id === id)) {
				return { err: `id '${id}' already exists` };
			}
			const def = defMap().get(type);
			if (!def) return { err: `unknown type '${type}'` };
			const merged: Record<string, unknown> = {};
			for (const p of def.params) {
				merged[p.id] =
					p.paramType === "Number"
						? (p.defaultNumber ?? 0)
						: (p.defaultText ?? "");
			}
			if (params) Object.assign(merged, params);
			const node: NodeInstance = {
				id,
				typeId: type,
				params: merged,
				positionX: stagger(graph.nodes.length).x,
				positionY: stagger(graph.nodes.length).y,
			};
			await b.applyGraph({ ...graph, nodes: [...graph.nodes, node] });
			return { ok: true, id };
		},
	});

	const removeNode = tool({
		description:
			"Remove a node and every edge touching it. Returns the dropped edges and any nodes left with a now-empty input.",
		inputSchema: z.object({ id: z.string() }),
		execute: async ({ id }) => {
			const graph = b.getGraph();
			const node = graph.nodes.find((n) => n.id === id);
			if (!node) return { err: `unknown id '${id}'` };
			if (PROTECTED_TYPES.has(node.typeId)) {
				return { err: `'${id}' is a protected ${node.typeId} node` };
			}
			const dropped = graph.edges.filter(
				(e) => e.fromNode === id || e.toNode === id,
			);
			const nowDangling = dropped
				.filter((e) => e.toNode !== id)
				.map((e) => `${e.toNode}.${e.toPort}`);
			await b.applyGraph({
				...graph,
				nodes: graph.nodes.filter((n) => n.id !== id),
				edges: graph.edges.filter((e) => e.fromNode !== id && e.toNode !== id),
			});
			return {
				ok: true,
				dropped_edges: dropped.map(edgeLabel),
				now_dangling: [...new Set(nowDangling)],
			};
		},
	});

	const setParams = tool({
		description:
			"Set one or more params on a node. Keys must exist on the node's type (see list_types); Number params want numbers, Text params want strings.",
		inputSchema: z.object({
			id: z.string(),
			params: z.record(z.string(), z.unknown()),
		}),
		execute: async ({ id, params }) => {
			const graph = b.getGraph();
			const node = graph.nodes.find((n) => n.id === id);
			if (!node) return { err: `unknown id '${id}'` };
			if (PROTECTED_TYPES.has(node.typeId)) {
				return { err: `'${id}' is a protected ${node.typeId} node` };
			}
			const def = defMap().get(node.typeId);
			for (const [k, v] of Object.entries(params)) {
				const pdef = def?.params.find((p) => p.id === k);
				if (!pdef) return { err: `'${k}' is not a param of ${node.typeId}` };
				if (pdef.paramType === "Number" && typeof v !== "number") {
					return { err: `param '${k}'`, expected: "Number", got: typeof v };
				}
				if (pdef.paramType === "Text" && typeof v !== "string") {
					return { err: `param '${k}'`, expected: "Text", got: typeof v };
				}
				const options = paramOptions(pdef);
				if (options && !options.some((option) => option.id === v)) {
					return {
						err: `param '${k}'`,
						expected: options.map((option) => option.id).join(" | "),
						got: v,
					};
				}
			}
			await b.applyGraph({
				...graph,
				nodes: graph.nodes.map((n) =>
					n.id === id ? { ...n, params: { ...n.params, ...params } } : n,
				),
			});
			return { ok: true };
		},
	});

	const replaceNode = tool({
		description:
			"Change a node's type in place (keeping its id). Edges whose ports still exist on the new type are kept; the rest are dropped.",
		inputSchema: z.object({
			id: z.string(),
			type: z.string(),
			params: z.record(z.string(), z.unknown()).optional(),
		}),
		execute: async ({ id, type, params }) => {
			const graph = b.getGraph();
			const node = graph.nodes.find((n) => n.id === id);
			if (!node) return { err: `unknown id '${id}'` };
			if (PROTECTED_TYPES.has(node.typeId)) {
				return { err: `'${id}' is a protected ${node.typeId} node` };
			}
			const def = defMap().get(type);
			if (!def) return { err: `unknown type '${type}'` };
			const merged: Record<string, unknown> = {};
			for (const p of def.params) {
				merged[p.id] =
					p.paramType === "Number"
						? (p.defaultNumber ?? 0)
						: (p.defaultText ?? "");
			}
			if (params) Object.assign(merged, params);

			const outIds = new Set(def.outputs.map((p) => p.id));
			const inIds = new Set(def.inputs.map((p) => p.id));
			const kept: Edge[] = [];
			const dropped: Edge[] = [];
			for (const e of graph.edges) {
				const ok =
					(e.fromNode !== id || outIds.has(e.fromPort)) &&
					(e.toNode !== id || inIds.has(e.toPort));
				(ok ? kept : dropped).push(e);
			}
			await b.applyGraph({
				...graph,
				nodes: graph.nodes.map((n) =>
					n.id === id ? { ...n, typeId: type, params: merged } : n,
				),
				edges: kept,
			});
			return {
				ok: true,
				kept_edges: kept
					.filter((e) => e.fromNode === id || e.toNode === id)
					.map(edgeLabel),
				dropped_edges: dropped.map(edgeLabel),
			};
		},
	});

	const connect = tool({
		description:
			"Wire from_node.from_port -> to_node.to_port. Port types must match exactly (see list_types). If the target input already has an edge it is replaced.",
		inputSchema: z.object({
			from_node: z.string(),
			from_port: z.string(),
			to_node: z.string(),
			to_port: z.string(),
		}),
		execute: async ({ from_node, from_port, to_node, to_port }) => {
			const graph = b.getGraph();
			const dm = defMap();
			const fromNode = graph.nodes.find((n) => n.id === from_node);
			const toNode = graph.nodes.find((n) => n.id === to_node);
			if (!fromNode) return { err: `unknown from_node '${from_node}'` };
			if (!toNode) return { err: `unknown to_node '${to_node}'` };
			const outPort = findOutPort(dm.get(fromNode.typeId), from_port);
			const inPort = findInPort(dm.get(toNode.typeId), to_port);
			if (!outPort) return { err: `${from_node} has no output '${from_port}'` };
			if (!inPort) return { err: `${to_node} has no input '${to_port}'` };
			if (outPort.portType !== inPort.portType) {
				return {
					err: "port type mismatch",
					expected_type: inPort.portType,
					got_type: outPort.portType,
				};
			}
			const newEdge: Edge = {
				id: edgeId({
					fromNode: from_node,
					fromPort: from_port,
					toNode: to_node,
					toPort: to_port,
				}),
				fromNode: from_node,
				fromPort: from_port,
				toNode: to_node,
				toPort: to_port,
			};
			// One edge per input: drop any existing edge into this (to_node,to_port).
			const replaced = graph.edges.filter(
				(e) => e.toNode === to_node && e.toPort === to_port,
			);
			const edges = graph.edges
				.filter((e) => !(e.toNode === to_node && e.toPort === to_port))
				.concat(newEdge);
			await b.applyGraph({ ...graph, edges });
			return {
				ok: true,
				...(replaced.length > 0 ? { replaced: replaced.map(edgeLabel) } : {}),
			};
		},
	});

	const disconnect = tool({
		description: "Remove the edge from_node.from_port -> to_node.to_port.",
		inputSchema: z.object({
			from_node: z.string(),
			from_port: z.string(),
			to_node: z.string(),
			to_port: z.string(),
		}),
		execute: async ({ from_node, from_port, to_node, to_port }) => {
			const graph = b.getGraph();
			const before = graph.edges.length;
			const edges = graph.edges.filter(
				(e) =>
					!(
						e.fromNode === from_node &&
						e.fromPort === from_port &&
						e.toNode === to_node &&
						e.toPort === to_port
					),
			);
			if (edges.length === before) return { ok: true, removed: false };
			await b.applyGraph({ ...graph, edges });
			return { ok: true, removed: true };
		},
	});

	const run = tool({
		description:
			"Compile and run the working graph. Returns either a compile error (bad/missing wiring, type errors, cycles) or a summary of each view node's output signal. Run this after edits to check correctness; it also updates the live visualizer and publishes the run to the `python` workspace (`luma.graph.run`).",
		inputSchema: z.object({}),
		execute: async () => {
			try {
				const result = await b.runGraph(b.getGraph());
				return { ok: true, ...summarizeRun(result) };
			} catch (err) {
				return { ok: false, error: String(err) };
			}
		},
	});

	const preview = tool({
		description:
			"Render the working graph to a space-time heatmap image and look at it. Rows = fixtures (sorted by activation time), cols = time across the span, pixel = dimmer × RGB. Use this to *see* the output — colour, motion, timing — alongside `python` for numbers. Selection args resolve to all fixtures.",
		inputSchema: z.object({}),
		execute: async () => {
			let img: AnnotationPreview;
			try {
				img = await b.previewImage(b.getGraph());
			} catch (err) {
				return { error: String(err) };
			}
			const base64 = await previewToPngBase64(img);
			return {
				width: img.width,
				height: img.height,
				dominantColor: img.dominantColor,
				base64,
			};
		},
		toModelOutput: ({ output }) => imageToolOutput(output),
	});

	const setArgs = tool({
		description: `Overwrite the pattern's args entirely (the pattern's interface). Pass the full new list — anything omitted is removed. Each arg: { id, name, argType, defaultValue }.

\`id\` and \`name\` MUST be snake_case (lowercase letters/digits/underscores, no leading/trailing/double underscores), e.g. \`base_intensity\` — same convention users get. Non-conforming names are rejected.

argType + defaultValue shapes:
  Color     -> { r, g, b, a }            (0-255, a 0-1)
  Scalar    -> a bare number
  Selection -> { expression }             — ALWAYS set expression to "all"; never bake a venue-specific selection into a pattern. Use set_preview_selection to preview on specific groups.
  Palette   -> { colors: ["#rrggbb", …] }
  Gradient  -> { stops: [{ color: "#rrggbb", t: 0..1 }, …] }

The pattern_args node's output ports update to match. Wire nodes from pattern_args.<arg_id> after.`,
		inputSchema: z.object({
			args: z.array(
				z.object({
					id: z.string(),
					name: z.string(),
					argType: z.enum([
						"Color",
						"Scalar",
						"Selection",
						"Palette",
						"Gradient",
					]),
					defaultValue: z.unknown(),
				}),
			),
		}),
		execute: async ({ args }) => {
			// Enforce the snake_case naming convention (the same one users get) —
			// error rather than silently normalize, so the agent learns the rule.
			for (const a of args) {
				for (const [field, value] of [
					["id", a.id],
					["name", a.name],
				] as const) {
					const canonical = toSnakeCase(value);
					if (canonical !== value || canonical.length === 0) {
						return {
							err: `arg ${field} '${value}' must be snake_case`,
							expected: canonical || "(must contain a letter or digit)",
						};
					}
				}
			}
			// Enforce the invariant: Selection args are always `all` in the saved
			// pattern; venue-specific previewing goes through set_preview_selection.
			const normalized = args.map((a) => {
				if (a.argType === "Selection") {
					const { spatialReference: _dead, ...dv } = (a.defaultValue ??
						{}) as Record<string, unknown>;
					return {
						...a,
						defaultValue: { ...dv, expression: "all" },
					};
				}
				return a;
			}) as PatternArgDef[];
			await b.setArgs(normalized);
			return { ok: true, args: normalized.map((a) => `${a.id}:${a.argType}`) };
		},
	});

	const setPreviewSelection = tool({
		description: `Set the PREVIEW-ONLY selection — which fixtures the preview/visualizer renders on. This does NOT change the saved pattern (its Selection arg stays \`all\`); it just lets you see the pattern on a subset of the rig. Pass a tag expression of venue group names (use ask_venue to find them), e.g. "front_wash" or "front_wash | left_movers". Pass null/empty to clear (back to all fixtures).`,
		inputSchema: z.object({
			expression: z
				.string()
				.nullable()
				.describe("Tag expression of group names, or null to clear."),
		}),
		execute: async ({ expression }) => {
			const expr = expression?.trim() ? expression.trim() : null;
			b.setPreviewSelection(expr);
			return { ok: true, preview_selection: expr ?? "all" };
		},
	});

	const askVenue = buildAskVenueTool({ getVenueId: b.getVenueId });

	const python = buildPythonTool({
		threadId: b.threadId,
		turnMessageId: b.turnMessageId,
		abortSignal: b.abortSignal,
		getScope: () => ({
			patternId: b.getPatternId(),
			implementationId: b.getImplementationId(),
			venueId: b.getVenueId(),
			trackId: b.getTrackId(),
			window: b.getSpan(),
			// The working canvas, so Python sees the graph as the agent has it now.
			graphDefinition: b.getGraph(),
		}),
	});

	return {
		graph_view: graphView,
		get_subgraph: getSubgraph,
		list_types: listTypes,
		add_node: addNode,
		remove_node: removeNode,
		set_params: setParams,
		replace_node: replaceNode,
		connect,
		disconnect,
		run_graph: run,
		python,
		preview,
		set_args: setArgs,
		set_preview_selection: setPreviewSelection,
		ask_venue: askVenue,
	};
}

type ImageOut =
	| { error: string }
	| {
			width: number;
			height: number;
			dominantColor: [number, number, number];
			base64: string;
	  };

function imageToolOutput(output: unknown) {
	const o = output as ImageOut;
	if ("error" in o) return { type: "error-text" as const, value: o.error };
	return {
		type: "content" as const,
		value: [
			{
				type: "text" as const,
				text: `Graph output heatmap (${o.width}×${o.height}). Rows = fixtures sorted by activation time, cols = time, brightness = dimmer × RGB.`,
			},
			{ type: "image-data" as const, data: o.base64, mediaType: "image/png" },
		],
	};
}

function edgeLabel(e: Edge): string {
	return `${e.fromNode}.${e.fromPort} -> ${e.toNode}.${e.toPort}`;
}

function stagger(index: number): { x: number; y: number } {
	return { x: (index % 5) * 220, y: Math.floor(index / 5) * 160 };
}

function summarizeRun(result: RunResult): {
	views: Array<Record<string, unknown>>;
	fixtures: number;
} {
	const views: Array<Record<string, unknown>> = [];
	for (const [id, sig] of Object.entries(result.views)) {
		if (!sig) continue;
		views.push({ id, ...summarizeSignal(sig) });
	}
	const fixtures = result.universeState
		? Object.keys(result.universeState.primitives).length
		: 0;
	return { views, fixtures };
}

function summarizeSignal(sig: Signal): Record<string, unknown> {
	// Per-channel min/max across all primitives + time, so the agent sees the
	// shape without the full array.
	const channels: Array<{ ch: number; min: number; max: number }> = [];
	for (let ch = 0; ch < sig.c; ch++) {
		let min = Infinity;
		let max = -Infinity;
		for (let prim = 0; prim < sig.n; prim++) {
			for (let time = 0; time < sig.t; time++) {
				const v = sig.data[prim * sig.t * sig.c + time * sig.c + ch] ?? 0;
				if (v < min) min = v;
				if (v > max) max = v;
			}
		}
		channels.push({
			ch,
			min: Number.isFinite(min) ? round(min) : 0,
			max: Number.isFinite(max) ? round(max) : 0,
		});
	}
	return { primitives: sig.n, timesteps: sig.t, channels };
}

function round(n: number): number {
	return Math.round(n * 1000) / 1000;
}
