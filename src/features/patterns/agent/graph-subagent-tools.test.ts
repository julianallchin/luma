import { afterEach, describe, expect, it, vi } from "vitest";
import type {
	AuthoredProjectedDocument,
	Graph,
	RunResult,
} from "@/bindings/schema";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import { buildGraphSubagentTools, type GraphBridge } from "./graph-agent";
import { withPatternArgsNode } from "./graph-tools";

const initialGraph: Graph = {
	nodes: [
		{
			id: "child_view",
			typeId: "view_signal",
			params: {},
			positionX: 10,
			positionY: 20,
		},
	],
	edges: [],
	args: [],
};

const execution = { toolCallId: "call-1", messages: [] };

afterEach(() => {
	resetInvoke();
	vi.restoreAllMocks();
});

describe("pattern subagent tools", () => {
	it("exposes the parent graph surface and writes only its structured workspace", async () => {
		const parentGraph = structuredClone(initialGraph);
		const writes: Graph[] = [];
		setInvoke(async <T>(command: string, args = {}) => {
			expect(command).toBe("authored_state_write_workspace_graph");
			const graph = structuredClone(
				(args as { input: { graph: Graph } }).input.graph,
			);
			writes.push(graph);
			return graph as T;
		});
		const bridge = bridgeFor(parentGraph);
		const tools = buildGraphSubagentTools({
			getBridge: () => bridge,
			threadId: "thread-1",
			turnMessageId: "user-1",
			workspaceId: "workspace-1",
			bindWorkspaceDocument: () => undefined,
			initialDocument: {
				kind: "pattern_graph",
				implementationId: "implementation-1",
				revision: "base",
				graph: initialGraph,
			},
		});

		expect(Object.keys(tools).sort()).toEqual(
			[
				"add_node",
				"ask_venue",
				"connect",
				"disconnect",
				"get_subgraph",
				"graph_view",
				"list_types",
				"preview",
				"python",
				"remove_node",
				"replace_node",
				"run_graph",
				"set_args",
				"set_params",
				"set_preview_selection",
			].sort(),
		);
		for (const fileTool of ["ls", "find", "read", "grep", "write", "edit"]) {
			expect(Object.keys(tools)).not.toContain(fileTool);
		}

		await tools.remove_node.execute?.({ id: "child_view" }, execution);

		expect(writes).toHaveLength(1);
		expect(writes[0].nodes).toEqual([]);
		expect(parentGraph).toEqual(initialGraph);
		const view = (await tools.graph_view.execute?.({}, execution)) as {
			view: string;
		};
		expect(view.view).not.toContain("child_view");
	});

	it("keeps selection local and runs in the child execution namespace", async () => {
		setInvoke(async <T>(_command: string, args = {}) => {
			return structuredClone(
				(args as { input: { graph: Graph } }).input.graph,
			) as T;
		});
		const bridge = bridgeFor(structuredClone(initialGraph));
		const tools = buildGraphSubagentTools({
			getBridge: () => bridge,
			threadId: "thread-1",
			turnMessageId: "user-1",
			workspaceId: "workspace-1",
			bindWorkspaceDocument: () => undefined,
			initialDocument: {
				kind: "pattern_graph",
				implementationId: "implementation-1",
				revision: "base",
				graph: initialGraph,
			},
		});

		await tools.set_preview_selection.execute?.(
			{ expression: "front_wash" },
			execution,
		);
		await tools.run_graph.execute?.({}, execution);

		expect(bridge.setPreviewSelection).not.toHaveBeenCalled();
		expect(bridge.run).toHaveBeenCalledWith(expect.anything(), {
			agentThreadId: "thread-1",
			agentExecutionId: "workspace-1",
			driveLivePreview: false,
			previewSelection: "front_wash",
		});
	});

	it("serializes sibling tool calls so complete graph writes cannot lose edits", async () => {
		const graph: Graph = {
			...structuredClone(initialGraph),
			nodes: [
				...structuredClone(initialGraph.nodes),
				{
					id: "second_view",
					typeId: "view_signal",
					params: {},
					positionX: 30,
					positionY: 20,
				},
			],
		};
		const writes: Graph[] = [];
		setInvoke(async <T>(_command: string, args = {}) => {
			await Promise.resolve();
			const written = structuredClone(
				(args as { input: { graph: Graph } }).input.graph,
			);
			writes.push(written);
			return written as T;
		});
		const tools = buildGraphSubagentTools({
			getBridge: () => bridgeFor(structuredClone(graph)),
			threadId: "thread-1",
			turnMessageId: "user-1",
			workspaceId: "workspace-1",
			bindWorkspaceDocument: () => undefined,
			initialDocument: {
				kind: "pattern_graph",
				implementationId: "implementation-1",
				revision: "base",
				graph,
			},
		});

		await Promise.all([
			tools.remove_node.execute?.({ id: "child_view" }, execution),
			tools.remove_node.execute?.({ id: "second_view" }, execution),
		]);

		expect(writes).toHaveLength(2);
		expect(writes[1].nodes).toEqual([]);
	});

	it("updates the parent tool view after a recursive workspace merge", async () => {
		let receiveMergedDocument:
			| ((document: AuthoredProjectedDocument) => void | Promise<void>)
			| undefined;
		const tools = buildGraphSubagentTools({
			getBridge: () => bridgeFor(structuredClone(initialGraph)),
			threadId: "thread-1",
			turnMessageId: "user-1",
			workspaceId: "workspace-1",
			bindWorkspaceDocument: (sink) => {
				receiveMergedDocument = sink;
			},
			initialDocument: {
				kind: "pattern_graph",
				implementationId: "implementation-1",
				revision: "base",
				graph: initialGraph,
			},
		});
		const mergedGraph: Graph = {
			...structuredClone(initialGraph),
			nodes: [
				{
					id: "nested_result",
					typeId: "view_signal",
					params: {},
					positionX: 50,
					positionY: 60,
				},
			],
		};

		await receiveMergedDocument?.({
			kind: "pattern_graph",
			implementationId: "implementation-1",
			revision: "nested-revision",
			graph: mergedGraph,
		});
		const view = (await tools.graph_view.execute?.({}, execution)) as {
			view: string;
		};

		expect(view.view).toContain("nested_result");
		expect(view.view).not.toContain("child_view");
	});
});

describe("withPatternArgsNode", () => {
	it("adds the synthetic node and drops edges for removed arguments", () => {
		const withArgs = withPatternArgsNode(initialGraph, [
			{
				id: "level",
				name: "level",
				argType: "Scalar",
				defaultValue: { value: 0.5 },
			},
		]);
		expect(withArgs.nodes).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ id: "pattern_args", typeId: "pattern_args" }),
			]),
		);

		const removed = withPatternArgsNode(
			{
				...withArgs,
				edges: [
					{
						id: "args-edge",
						fromNode: "pattern_args",
						fromPort: "level",
						toNode: "child_view",
						toPort: "signal",
					},
				],
			},
			[],
		);
		expect(removed.args).toEqual([]);
		expect(removed.nodes.some((node) => node.typeId === "pattern_args")).toBe(
			false,
		);
		expect(removed.edges).toEqual([]);
	});
});

function bridgeFor(graph: Graph): GraphBridge {
	return {
		patternId: "pattern-1",
		implementationId: "implementation-1",
		serialize: () => graph,
		apply: vi.fn(),
		restore: vi.fn(),
		checkpoint: vi.fn(async () => undefined),
		run: vi.fn(async () => ({ views: {} }) as RunResult),
		getNodeDefs: () => [],
		getTrackId: () => "track-1",
		getSpan: () => [0, 10],
		describe: () => "Pattern: test",
		syncFromEditor: vi.fn(),
		previewImage: vi.fn(),
		setArgs: vi.fn(),
		setPreviewSelection: vi.fn(),
		getVenueId: () => "venue-1",
	};
}
