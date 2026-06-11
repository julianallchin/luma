import { tool } from "ai";
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
import { buildAskVenueTool } from "@/shared/lib/agent/ask-venue-tool";
import {
	type ProbeResult,
	runProbe,
	runResultToProbeViews,
} from "./graph-probe";
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
 * errors for review; `getLastRun`/`setLastRun` cache the latest results so the
 * `inspect` probe has data to chew on. */
export type GraphAgentBindings = {
	getGraph: () => Graph;
	applyGraph: (graph: Graph) => void;
	runGraph: (graph: Graph) => Promise<RunResult>;
	getNodeDefs: () => NodeTypeDef[];
	/** Preview span [startSec, endSec] — used to map probe sample indices to time. */
	getSpan: () => [number, number];
	getLastRun: () => RunResult | null;
	setLastRun: (run: RunResult | null) => void;
	/** Render the graph to a space-time heatmap (rows=fixtures, cols=time). */
	previewImage: (graph: Graph) => Promise<AnnotationPreview>;
	/** Overwrite the pattern's args entirely. */
	setArgs: (args: PatternArgDef[]) => void;
	/** Set the preview-only selection (null → revert to the pattern's `all`). */
	setPreviewSelection: (expression: string | null) => void;
	getVenueId: () => string | null;
};

/** pattern_args is synthetic (its ports mirror the args panel) — the agent may
 * wire FROM it but must not delete, replace, or reparametrize it. */
const PROTECTED_TYPES = new Set(["pattern_args"]);

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
			b.applyGraph({ ...graph, nodes: [...graph.nodes, node] });
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
			b.applyGraph({
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
			}
			b.applyGraph({
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
			b.applyGraph({
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
			b.applyGraph({ ...graph, edges });
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
			b.applyGraph({ ...graph, edges });
			return { ok: true, removed: true };
		},
	});

	const run = tool({
		description:
			"Compile and run the working graph. Returns either a compile error (bad/missing wiring, type errors, cycles) or a summary of each view node's output signal. Run this after edits to check correctness; it also updates the live visualizer and caches results for `inspect`.",
		inputSchema: z.object({}),
		execute: async () => {
			try {
				const result = await b.runGraph(b.getGraph());
				b.setLastRun(result);
				return { ok: true, ...summarizeRun(result) };
			} catch (err) {
				b.setLastRun(null);
				return { ok: false, error: String(err) };
			}
		},
	});

	const inspect = tool({
		description: `Run JavaScript against the latest run's view-node signals to inspect them precisely — you can't eyeball a 4096-float array, so write code instead.

Auto-runs the graph first if needed. Every result includes \`views_available\` (the view ids + their {n,t,c} shapes) so you can see what you have; on the first call just \`return Object.keys(views)\` or log a shape if unsure. Your code is an async function body; \`return\` a value and/or \`console.log\`. Bound in scope:
  views   — object mapping view-node id -> Signal { n, t, c, data }
            data is flat [primitive][time][channel]: data[prim*t*c + time*c + ch]
            (n = primitives, t = timesteps over the span, c = channels; for a
             color signal c=4 → [r,g,b,a]; for a scalar c=1)
  at(sig, prim, time, ch)        -> one sample
  series(sig, prim=0, ch=0)      -> number[] over time
  tOf(sig, i)                    -> sample index i as seconds in the span
  stats(arr)                     -> { len, min, max, mean, rms }
  peaks(arr, {threshold, minGap})-> indices of local maxima

Example — when does view_signal_1's dimmer pulse?
  const s = views.view_signal_1;
  const dim = series(s, 0, 3);
  return peaks(dim, {threshold:0.5, minGap:5}).map(i => +tOf(s,i).toFixed(2));`,
		inputSchema: z.object({
			code: z
				.string()
				.describe("Async-function body. Return and/or console.log."),
		}),
		execute: async ({ code }) => {
			let run = b.getLastRun();
			if (!run) {
				try {
					run = await b.runGraph(b.getGraph());
					b.setLastRun(run);
				} catch (err) {
					return { ok: false, error: `graph failed to run: ${err}` };
				}
			}
			const views = runResultToProbeViews(run);
			// Manifest of what's bound, returned on every call so the agent can
			// "see what it has" without spending a probe just to discover shapes.
			const available = Object.entries(views).map(([id, sig]) => ({
				id,
				n: sig.n,
				t: sig.t,
				c: sig.c,
			}));
			if (available.length === 0) {
				return {
					ok: false,
					error:
						"No view-node signals to inspect. Add a view_signal/view_uv node and connect it, then run.",
					views_available: available,
				};
			}
			const probe: ProbeResult = await runProbe({
				code,
				views,
				span: b.getSpan(),
			});
			// Guard the model from raw signal dumps: the agent's code can `return`
			// or log a 4096-float array, which would flood context. Clamp both —
			// the probe is for *summaries* (peaks, stats), not raw data.
			return {
				ok: probe.ok,
				error: probe.error,
				result: clampForModel(probe.result),
				logs: clampLogs(probe.logs),
				views_available: available,
			};
		},
	});

	const preview = tool({
		description:
			"Render the working graph to a space-time heatmap image and look at it. Rows = fixtures (sorted by activation time), cols = time across the span, pixel = dimmer × RGB. Use this to *see* the output — colour, motion, timing — alongside `inspect` for numbers. Selection args resolve to all fixtures.",
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
		description: `Overwrite the pattern's args entirely (the pattern's interface). Pass the full new list — anything omitted is removed. Each arg: { id (snake_case), name, argType, defaultValue }.

argType + defaultValue shapes:
  Color     -> { r, g, b, a }            (0-255, a 0-1)
  Scalar    -> a bare number
  Selection -> { expression, spatialReference }  — ALWAYS set expression to "all"; never bake a venue-specific selection into a pattern. Use set_preview_selection to preview on specific groups.
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
			// Enforce the invariant: Selection args are always `all` in the saved
			// pattern; venue-specific previewing goes through set_preview_selection.
			const normalized = args.map((a) => {
				if (a.argType === "Selection") {
					const dv = (a.defaultValue ?? {}) as Record<string, unknown>;
					return {
						...a,
						defaultValue: {
							...dv,
							expression: "all",
							spatialReference: dv.spatialReference ?? "global",
						},
					};
				}
				return a;
			}) as PatternArgDef[];
			b.setArgs(normalized);
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
		inspect,
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

const PROBE_RESULT_MAX = 1500; // chars of JSON returned to the model
const PROBE_LOG_MAX = 1500;
const PROBE_LOG_LINES = 40;

/** Clamp the probe's return value so a raw-array dump can't flood context. */
function clampForModel(value: unknown): unknown {
	if (value === undefined) return undefined;
	// Long arrays are the main offender — summarize instead of sending them.
	if (Array.isArray(value) && value.length > 64) {
		return `[array of ${value.length} items — too large to return; summarize it in your code (peaks/stats) instead of returning raw data]`;
	}
	let json: string;
	try {
		json = JSON.stringify(value);
	} catch {
		json = String(value);
	}
	if (json.length > PROBE_RESULT_MAX) {
		return `${json.slice(0, PROBE_RESULT_MAX)}… [truncated ${json.length - PROBE_RESULT_MAX} chars — return a summary, not raw data]`;
	}
	return value;
}

/** Cap log volume (count + total length). */
function clampLogs(logs: string[]): string[] {
	if (logs.length === 0) return logs;
	let out = logs;
	let dropped = 0;
	if (out.length > PROBE_LOG_LINES) {
		dropped = out.length - PROBE_LOG_LINES;
		out = out.slice(0, PROBE_LOG_LINES);
	}
	let total = 0;
	const capped: string[] = [];
	for (const line of out) {
		if (total >= PROBE_LOG_MAX) {
			dropped += out.length - capped.length;
			break;
		}
		const remaining = PROBE_LOG_MAX - total;
		capped.push(
			line.length > remaining ? `${line.slice(0, remaining)}…` : line,
		);
		total += line.length;
	}
	if (dropped > 0) capped.push(`… [${dropped} more log line(s) dropped]`);
	return capped;
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
