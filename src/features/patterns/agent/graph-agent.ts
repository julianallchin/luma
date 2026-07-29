import { create } from "zustand";
import type {
	AnnotationPreview,
	Graph,
	NodeTypeDef,
	PatternArgDef,
	RunResult,
} from "@/bindings/schema";
import { createAgentChat } from "@/shared/components/agent-chat/create-agent-chat";
import type { ToolView, ToolVocab } from "@/shared/components/agent-chat/parts";
import { lumaOpenRouter } from "@/shared/lib/agent/openrouter";
import { buildGraphAgentTools } from "./graph-tools";

/** The graph agent's live handle on the pattern editor. Registered by
 * <PatternEditor> for its patternId; tools resolve it lazily so the long-lived
 * chat session always acts on the current canvas. */
export type GraphBridge = {
	serialize: () => Graph;
	apply: (graph: Graph) => void;
	run: (graph: Graph) => Promise<RunResult>;
	getNodeDefs: () => NodeTypeDef[];
	getSpan: () => [number, number];
	getLastRun: () => RunResult | null;
	setLastRun: (run: RunResult | null) => void;
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
		graph_view: { past: "Viewed graph", noun: null },
		get_subgraph: { past: "Viewed subgraph", noun: null },
		list_types: { past: "Listed types", noun: null },
		add_node: { past: "Added", noun: "node" },
		remove_node: { past: "Removed", noun: "node" },
		set_params: { past: "Set params on", noun: "node" },
		replace_node: { past: "Replaced", noun: "node" },
		connect: { past: "Connected", noun: "edge" },
		disconnect: { past: "Disconnected", noun: "edge" },
		run_graph: { past: "Ran graph", noun: null },
		inspect: { past: "Inspected signals", noun: null },
		preview: { past: "Previewed output", noun: null },
		set_args: { past: "Set", noun: "arg" },
		set_preview_selection: { past: "Set preview selection", noun: null },
		ask_venue: { past: "Asked venue", noun: null },
	},
	formatLabel: graphToolLabel,
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
		case "inspect":
			return { verb, detail: null };
		default:
			return { verb, detail: null };
	}
}

function str(v: unknown): string | null {
	return typeof v === "string" && v.length > 0 ? v : null;
}

const SYSTEM = `You are a lighting pattern engineer working inside a node-graph editor. A pattern is a graph of typed nodes wired together; it compiles to a signal that drives fixtures.

Workflow:
1. Call \`graph_view\` to see the current graph, and \`list_types\` to learn node types and their typed ports. Node ids (e.g. \`apply_color_1\`) are the handles you use everywhere.
2. Edit live with add_node / connect / set_params / replace_node / remove_node / disconnect. Ports only connect when their PortType matches EXACTLY — check list_types before wiring. Edits apply to the canvas immediately.
3. After edits, call \`run_graph\` to compile + run. It returns a compile error (fix it) or a summary of each view node's output signal, and updates the live preview.
4. To verify, use \`preview\` to *see* the output as a space-time heatmap (colour, motion, timing) and \`inspect\` to measure it precisely — write JavaScript against the run's view-node signals (you can't eyeball a 4096-float array) to find peaks, read colour channels, compare to expectations.

A view node (view_signal / view_uv) is what makes output visible and inspectable — make sure the graph terminates in one. \`pattern_args\` is a read-only node in the graph; its output ports are the pattern's args — wire FROM it. To change the args themselves, use \`set_args\` (overwrites the whole list), then wire from the new ports.

Selection & previewing: a pattern's Selection arg is ALWAYS \`all\` — patterns are venue-agnostic and select every fixture they're given. To preview on a specific part of the rig, use \`ask_venue\` to find group names and \`set_preview_selection\` with a tag expression (e.g. "front_wash | left_movers"). That only affects the preview/visualizer, never the saved pattern.

Be terse. Build, run, verify, then briefly report what you did.`;

// The graph agent runs its own model (independent of the track copilot).
const GRAPH_AGENT_MODEL = "x-ai/grok-4.5";

function createModel() {
	return lumaOpenRouter()?.(GRAPH_AGENT_MODEL) ?? null;
}

// ---------------------------------------------------------------------------
// Per-turn graph snapshots, coupled to conversation history → revertible.
//
// Deliberately ephemeral: these are process-memory undo points for the canvas,
// not part of the durable thread. They're keyed by patternId (the subject), not
// by thread id, because reverting is about the *canvas*, and they're cleared on
// reset so a fresh conversation never offers checkpoints from the old one.
// ---------------------------------------------------------------------------

export type GraphCheckpoint = {
	/** Assistant message id this snapshot was taken after; "baseline" before turn 1. */
	id: string;
	label: string;
	graph: Graph;
};

type SnapshotStore = {
	/** patternId -> ordered checkpoints (oldest first). */
	byPattern: Record<string, GraphCheckpoint[]>;
	record: (patternId: string, checkpoint: GraphCheckpoint) => void;
	/** Capture the pre-turn baseline once, before the agent's first edit. */
	ensureBaseline: (patternId: string, graph: Graph) => void;
	clear: (patternId: string) => void;
};

export const useGraphSnapshots = create<SnapshotStore>((set, get) => ({
	byPattern: {},
	record: (patternId, checkpoint) => {
		set((s) => {
			const list = s.byPattern[patternId] ?? [];
			// Replace if same id already recorded (idempotent on re-run).
			const next = list
				.filter((c) => c.id !== checkpoint.id)
				.concat(checkpoint);
			return { byPattern: { ...s.byPattern, [patternId]: next } };
		});
	},
	ensureBaseline: (patternId, graph) => {
		const list = get().byPattern[patternId];
		if (list && list.length > 0) return;
		set((s) => ({
			byPattern: {
				...s.byPattern,
				[patternId]: [{ id: "baseline", label: "Before agent", graph }],
			},
		}));
	},
	clear: (patternId) => {
		set((s) => ({ byPattern: { ...s.byPattern, [patternId]: [] } }));
	},
}));

// ---------------------------------------------------------------------------
// The agent instance.
// ---------------------------------------------------------------------------

export const graphAgent = createAgentChat<GraphBridge>({
	agentKind: "pattern_graph",
	subjectKind: "pattern",
	createModel,
	notConfiguredMessage: "OpenRouter API key is not set.",
	vocab: VOCAB,
	reasoningEffort: "high",
	onTurnStart: (bridge) => bridge.syncFromEditor(),
	buildSystem: (bridge) => `${SYSTEM}\n\n## This pattern\n${bridge.describe()}`,
	buildTools: ({ getBridge }) =>
		buildGraphAgentTools({
			getGraph: () => getBridge()?.serialize() ?? EMPTY_GRAPH,
			applyGraph: (graph) => getBridge()?.apply(graph),
			runGraph: (graph) => {
				const b = getBridge();
				if (!b) throw new Error("Editor not ready.");
				return b.run(graph);
			},
			getNodeDefs: () => getBridge()?.getNodeDefs() ?? [],
			getSpan: () => getBridge()?.getSpan() ?? [0, 1],
			getLastRun: () => getBridge()?.getLastRun() ?? null,
			setLastRun: (r) => getBridge()?.setLastRun(r),
			previewImage: (graph) => {
				const b = getBridge();
				if (!b) throw new Error("Editor not ready.");
				return b.previewImage(graph);
			},
			setArgs: (args) => getBridge()?.setArgs(args),
			setPreviewSelection: (expr) => getBridge()?.setPreviewSelection(expr),
			getVenueId: () => getBridge()?.getVenueId() ?? null,
		}),
	onTurnFinish: ({ subjectKey, message, bridge }) => {
		// Snapshot the graph as it stands after this turn, keyed to the message.
		useGraphSnapshots.getState().record(subjectKey, {
			id: message.id,
			label: `Turn ${turnLabel(subjectKey)}`,
			graph: bridge.serialize(),
		});
	},
	onReset: (subjectKey) => useGraphSnapshots.getState().clear(subjectKey),
});

function turnLabel(patternId: string): number {
	const list = useGraphSnapshots.getState().byPattern[patternId] ?? [];
	// baseline + N turns → next turn number is count of non-baseline + 1.
	return list.filter((c) => c.id !== "baseline").length + 1;
}
