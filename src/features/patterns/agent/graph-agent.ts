import type {
	AnnotationPreview,
	Graph,
	NodeTypeDef,
	PatternArgDef,
	RunResult,
} from "@/bindings/schema";
import { createAgentChat } from "@/shared/components/agent-chat/create-agent-chat";
import type { ToolView, ToolVocab } from "@/shared/components/agent-chat/parts";
import { renderPythonToolDetail } from "@/shared/components/agent-chat/python-tool-detail";
import { lumaOpenRouter } from "@/shared/lib/agent/openrouter";
import { pythonToolLabel } from "@/shared/lib/agent/python-tool";
import { invoke } from "@/shared/lib/tauri";
import { buildGraphAgentTools } from "./graph-tools";

/** The graph agent's live handle on the pattern editor. Registered by
 * <PatternEditor> for its patternId; tools resolve it lazily so the long-lived
 * chat session always acts on the current canvas. */
export type GraphBridge = {
	/** The pattern being edited — also the Python scope's pattern id. */
	patternId: string;
	/** Immutable authored identity beneath the pattern catalog entry. */
	implementationId: string;
	serialize: () => Graph;
	apply: (graph: Graph) => void;
	/** Apply an authoritative restored graph without auto-layout or migration. */
	restore: (graph: Graph, revision: string) => void;
	/** Persist the exact live canvas before a restore can replace it. */
	checkpoint: () => Promise<void>;
	/** Run the graph. `agentThreadId` publishes the run to that thread's Python
	 * workspace, so the next cell sees it under `luma.graph.run`. */
	run: (graph: Graph, opts?: { agentThreadId?: string }) => Promise<RunResult>;
	getNodeDefs: () => NodeTypeDef[];
	/** Track id of the current preview context (null when none is selected). */
	getTrackId: () => string | null;
	getSpan: () => [number, number];
	/** One-line context (pattern name + args) for the system prompt. */
	describe: () => string;
	/** Re-seed the in-memory working graph from the live canvas (turn start). */
	syncFromEditor: () => void;
	previewImage: (graph: Graph) => Promise<AnnotationPreview>;
	/** Overwrite the pattern's args entirely. */
	setArgs: (args: PatternArgDef[]) => void;
	/** Set the preview-only selection expression (null → use the args' value). */
	setPreviewSelection: (expression: string | null) => void;
	getVenueId: () => string | null;
};

const EMPTY_GRAPH: Graph = { nodes: [], edges: [], args: [] };

const VOCAB: ToolVocab = {
	verbs: {
		graph_view: { running: "Viewing", past: "Viewed", noun: "graph" },
		get_subgraph: { running: "Viewing", past: "Viewed", noun: "subgraph" },
		list_types: { running: "Listing", past: "Listed", noun: "type" },
		add_node: { running: "Adding", past: "Added", noun: "node" },
		remove_node: { running: "Removing", past: "Removed", noun: "node" },
		set_params: { running: "Setting", past: "Set", noun: "node parameters" },
		replace_node: { running: "Replacing", past: "Replaced", noun: "node" },
		connect: { running: "Connecting", past: "Connected", noun: "edge" },
		disconnect: {
			running: "Disconnecting",
			past: "Disconnected",
			noun: "edge",
		},
		run_graph: { running: "Running", past: "Ran", noun: "graph" },
		python: { running: "Running", past: "Ran", noun: "python cell" },
		preview: { running: "Previewing", past: "Previewed", noun: "output" },
		set_args: { running: "Setting", past: "Set", noun: "argument" },
		set_preview_selection: {
			running: "Selecting",
			past: "Selected",
			noun: "preview group",
		},
		ask_venue: {
			running: "Asking",
			past: "Asked",
			noun: "question",
			object: "venue",
		},
	},
	formatLabel: graphToolLabel,
	renderers: { python: renderPythonToolDetail },
};

function graphToolLabel(tool: ToolView): {
	verb: string;
	detail: string | null;
} {
	const meta = VOCAB.verbs[tool.name];
	const verb = meta?.past ?? tool.name;
	const input = (tool.input ?? {}) as Record<string, unknown>;
	switch (tool.name) {
		case "add_node":
		case "replace_node":
			return { verb, detail: str(input.id) ?? str(input.type) ?? null };
		case "remove_node":
		case "set_params":
			return { verb, detail: str(input.id) ?? null };
		case "connect":
		case "disconnect": {
			const from = `${str(input.from_node) ?? "?"}.${str(input.from_port) ?? "?"}`;
			const to = `${str(input.to_node) ?? "?"}.${str(input.to_port) ?? "?"}`;
			return { verb, detail: `${from} → ${to}` };
		}
		case "get_subgraph":
			return { verb, detail: str(input.id) ?? null };
		case "python":
			return pythonToolLabel(tool);
		default:
			return { verb, detail: null };
	}
}

function str(v: unknown): string | null {
	return typeof v === "string" && v.length > 0 ? v : null;
}

const SYSTEM = `You are a creative lighting collaborator working inside a node-graph editor. Behind the scenes, a pattern is a graph of typed nodes wired together; it compiles to a signal that drives fixtures.

Workflow:
1. Call \`graph_view\` to see the current graph, and \`list_types\` to learn node types and their typed ports. Node ids (e.g. \`apply_color_1\`) are the handles you use everywhere.
2. Edit live with add_node / connect / set_params / replace_node / remove_node / disconnect. Ports only connect when their PortType matches EXACTLY — check list_types before wiring. Edits apply to the canvas immediately.
3. After edits, call \`run_graph\` to compile + run. It returns a compile error (fix it) or a summary of each view node's output signal, and updates the live preview.
4. To verify, use \`preview\` to *see* the output as a space-time heatmap (colour, motion, timing) and \`python\` to measure it precisely — you can't eyeball a 4096-float array, so compute instead.

The \`python\` tool is a persistent Python namespace, refreshed before every call. After \`run_graph\`, the run's view-node output is bound at \`luma.graph.run.views\` (dict of view-node id -> tensor; \`.values\` is a numpy array, \`.times_s\` its time axis, \`.channels\` its channel labels). The track under the preview context is bound too, so you can correlate lighting against the music — e.g. line \`luma.graph.run.views["view_signal_1"]\` up with \`luma.features.drum_onsets["kick"]\` to measure how tightly a strobe tracks the kick. Call \`luma.catalog()\` to see everything that's bound. Variables persist between cells, and matplotlib figures come back as images you can actually look at — plot the dimmer curve against onset times when timing is the question. Python is read-only: change the graph with the edit tools, re-run, then re-measure.

A view node (view_signal / view_uv) is what makes output visible and measurable — make sure the graph terminates in one. \`pattern_args\` is a read-only node in the graph; its output ports are the pattern's args — wire FROM it. To change the args themselves, use \`set_args\` (overwrites the whole list), then wire from the new ports.

Selection & previewing: a pattern's Selection arg is ALWAYS \`all\` — patterns are venue-agnostic and select every fixture they're given. To preview on a specific part of the rig, use \`ask_venue\` to find group names and \`set_preview_selection\` with a tag expression (e.g. "front_wash | left_movers"). That only affects the preview/visualizer, never the saved pattern.

Run \`run_graph\` before \`python\` when you want to measure the latest edits — Python sees the run that last executed.

## Voice
Keep the user-facing conversation extremely concise, creative, and nontechnical. Default to one or two short sentences. Use one sentence after a straightforward action. Do not add a preamble, recap, heading, or list unless the user asks for one. Never use em dashes.

Speak like a lighting artist, not a graph engineer. Describe the visible result in terms of color, rhythm, motion, shape, atmosphere, tension, and release.

Maintain that artistic front while using technical terminology and logic privately. Do not narrate nodes, ports, signals, arrays, tools, schemas, compilation, or measurement details unless the user explicitly asks. Translate the machinery into plain visual language: say what changed and how it will feel.

Be decisive and tasteful. Use the fewest vivid words that carry the idea. Build, run, and verify quietly, then state only the artistic result.`;

// The graph agent runs its own model (independent of the track assistant).
const GRAPH_AGENT_MODEL = "x-ai/grok-4.5";

function createModel(modelId = GRAPH_AGENT_MODEL) {
	return lumaOpenRouter()?.(modelId) ?? null;
}

export const graphAgent = createAgentChat<GraphBridge>({
	agentKind: "pattern_graph",
	subjectKind: "pattern",
	createModel,
	notConfiguredMessage: "OpenRouter API key is not set.",
	vocab: VOCAB,
	reasoningEffort: "high",
	onTurnStart: (bridge) => bridge.syncFromEditor(),
	buildSystem: (bridge) => `${SYSTEM}\n\n## This pattern\n${bridge.describe()}`,
	buildTools: ({ getBridge, threadId, turnMessageId, abortSignal }) =>
		buildGraphAgentTools({
			threadId,
			turnMessageId,
			abortSignal,
			getGraph: () => getBridge()?.serialize() ?? EMPTY_GRAPH,
			applyGraph: (graph) => getBridge()?.apply(graph),
			runGraph: (graph) => {
				const b = getBridge();
				if (!b) throw new Error("Editor not ready.");
				// Publish the run to this thread's Python workspace.
				return b.run(graph, { agentThreadId: threadId });
			},
			getNodeDefs: () => getBridge()?.getNodeDefs() ?? [],
			getSpan: () => getBridge()?.getSpan() ?? [0, 1],
			getPatternId: () => getBridge()?.patternId ?? null,
			getImplementationId: () => getBridge()?.implementationId ?? null,
			getTrackId: () => getBridge()?.getTrackId() ?? null,
			previewImage: (graph) => {
				const b = getBridge();
				if (!b) throw new Error("Editor not ready.");
				return b.previewImage(graph);
			},
			setArgs: (args) => getBridge()?.setArgs(args),
			setPreviewSelection: (expr) => getBridge()?.setPreviewSelection(expr),
			getVenueId: () => getBridge()?.getVenueId() ?? null,
		}),
	captureAuthoredState: ({ bridge }) => ({
		graph: structuredClone(bridge.serialize()),
	}),
	checkpointAuthoredState: ({ bridge }) => bridge.checkpoint(),
	applyAuthoredState: ({ result, bridge }) => {
		if (result.document.kind !== "pattern_graph") {
			throw new Error("The authored revision is not a pattern graph.");
		}
		if (result.document.implementationId !== bridge.implementationId) {
			throw new Error(
				"The authored revision belongs to another implementation.",
			);
		}
		bridge.restore(result.document.graph, result.document.revision);
	},
	refreshAuthoredState: async ({ bridge }) => {
		const document = await invoke<{
			implementationId: string;
			revision: string;
			graph: Graph;
		}>("get_pattern_graph_document", {
			id: bridge.patternId,
			implementationId: bridge.implementationId,
		});
		if (document.implementationId !== bridge.implementationId) {
			throw new Error("Graph refresh resolved another implementation.");
		}
		bridge.restore(document.graph, document.revision);
	},
});
