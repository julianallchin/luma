import type { Context } from "@earendil-works/pi-ai";
import { describe, expect, it, vi } from "vitest";
import { z } from "zod";
import { tool } from "@/shared/lib/agent/agent-tool";
import { userChatMessage } from "@/shared/lib/agent/messages";
import { TEST_PI_MODEL, testTextStream } from "@/test/pi-model";
import { AgentLoader } from "./agent-loader";
import { createPiSubagentRunner } from "./pi-runner";
import { SubagentManager } from "./subagent-manager";
import { createSubagentTools } from "./tools";
import type {
	AgentConfig,
	AgentToolOutput,
	PrepareSubagentSpawn,
	SubagentRunner,
	SubagentRunRequest,
} from "./types";

const domainTool = tool({
	description: "test tool",
	inputSchema: z.object({}),
	execute: async () => "ok",
});

const generalAgent: AgentConfig = {
	name: "worker",
	description: "test worker",
	systemPrompt: "Worker instructions.",
	promptMode: "append",
	toolNames: [],
};

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

function loader(agent: AgentConfig = generalAgent): AgentLoader {
	return new AgentLoader({ includeBundled: false, definitions: [agent] });
}

function prepare<Context>(
	override?: PrepareSubagentSpawn<Context>,
): PrepareSubagentSpawn<Context> {
	return (
		override ??
		(async () => ({
			tools: { python: domainTool },
		}))
	);
}

function managerWith(
	runner: SubagentRunner,
	options: {
		agent?: AgentConfig;
		prepareSpawn?: PrepareSubagentSpawn<unknown>;
		onEvent?: ConstructorParameters<typeof SubagentManager>[0]["onEvent"];
		createId?: () => string;
	} = {},
) {
	return new SubagentManager({
		runner,
		prepareSpawn: options.prepareSpawn ?? prepare(),
		agentLoader: loader(options.agent),
		onEvent: options.onEvent,
		createId: options.createId ?? (() => "child-1"),
	});
}

async function emitAnswer(request: SubagentRunRequest, text = "done") {
	await request.onMessages([
		userChatMessage(request.prompt, "prompt-1"),
		{
			id: "answer-1",
			role: "assistant",
			parts: [{ type: "text", text }],
		},
	]);
}

describe("AgentLoader", () => {
	it("loads the bundled generic agent", () => {
		const agents = new AgentLoader().list();
		expect(agents.map((agent) => agent.name)).toContain("general-purpose");
	});

	it("loads and overrides frontmatter definitions dynamically", () => {
		const agents = new AgentLoader({ includeBundled: false });
		agents.load(`---
name: scout
description: first
prompt_mode: replace
tools: read, preview
model: small
thinking: low
---
First prompt.`);
		agents.load({
			name: "scout",
			description: "replacement",
			systemPrompt: "Second prompt.",
			promptMode: "append",
			toolNames: [],
		});
		expect(agents.get("scout")).toMatchObject({
			description: "replacement",
			systemPrompt: "Second prompt.",
			promptMode: "append",
		});
	});

	it("inherits only the parent system prompt in append mode", () => {
		const agents = loader();
		const prompt = agents.buildSystemPrompt(generalAgent, {
			parentSystemPrompt: "Parent identity.",
			environment: "Subject: isolated child",
		});
		expect(prompt).toContain("Parent identity.");
		expect(prompt).toContain("Worker instructions.");
		expect(prompt).toContain("no parent conversation history");
		expect(prompt).toContain("Subject: isolated child");
	});

	it("replace mode ignores the parent system prompt", () => {
		const config = { ...generalAgent, promptMode: "replace" as const };
		const prompt = loader(config).buildSystemPrompt(config, {
			parentSystemPrompt: "Do not inherit this.",
		});
		expect(prompt).not.toContain("Do not inherit this.");
		expect(prompt).toContain("Worker instructions.");
	});
});

describe("Pi runner", () => {
	it("starts from a fresh one-message transcript and emits Pi events", async () => {
		const contexts: Context[] = [];
		const runner = createPiSubagentRunner({
			createModel: () => ({
				model: TEST_PI_MODEL,
				streamFn: (_model, context) => {
					contexts.push(context);
					return testTextStream("Done.");
				},
			}),
		});
		const eventTypes: string[] = [];
		const result = await runner({
			systemPrompt: "Child identity.",
			prompt: "Only this child task.",
			tools: {},
			abortSignal: new AbortController().signal,
			drainSteering: () => [],
			subscribeSteering: () => () => undefined,
			onMessages: () => undefined,
			onAgentEvent: (event) => {
				eventTypes.push(event.type);
			},
		});
		expect(result).toBe("Done.");
		expect(eventTypes).toContain("message_update");
		expect(contexts).toHaveLength(1);
		expect(JSON.stringify(contexts[0]?.messages)).toContain(
			"Only this child task.",
		);
		expect(JSON.stringify(contexts[0]?.messages)).not.toContain(
			"parent transcript secret",
		);
	});

	it("continues with queued steering and preserves each assistant response", async () => {
		const firstText = deferred<void>();
		const releaseFirst = deferred<void>();
		const contexts: Context[] = [];
		const runner = createPiSubagentRunner({
			createModel: () => ({
				model: TEST_PI_MODEL,
				streamFn: (_model, context) => {
					contexts.push({ ...context, messages: [...context.messages] });
					return contexts.length === 1
						? testTextStream("Initial answer.", {
								afterText: async () => {
									firstText.resolve();
									await releaseFirst.promise;
								},
							})
						: testTextStream("Revised answer.");
				},
			}),
		});
		const manager = managerWith(runner);
		const record = await manager.spawn("worker", "Original task.");
		await firstText.promise;
		manager.steer(record.id, "Please revise the conclusion.");
		releaseFirst.resolve();

		await expect(manager.getResult(record.id)).resolves.toBe(
			"Initial answer.\n\nRevised answer.",
		);
		expect(contexts).toHaveLength(2);
		const followUpPrompt = JSON.stringify(contexts[1]?.messages);
		expect(followUpPrompt).toContain("Initial answer.");
		expect(followUpPrompt).toContain("Please revise the conclusion.");
		expect(manager.get(record.id)?.messages).toMatchObject([
			{ role: "user" },
			{ role: "assistant" },
			{ role: "user" },
			{ role: "assistant" },
		]);
	});
});

describe("SubagentManager", () => {
	it("inherits child-bound domain and recursive tools without file tools", async () => {
		let captured: SubagentRunRequest | undefined;
		const events: string[] = [];
		const manager = managerWith(
			async (request) => {
				captured = request;
				await emitAnswer(request, "finished");
				return "finished";
			},
			{ onEvent: ({ event }) => events.push(event.type) },
		);

		const record = await manager.spawn("worker", "Do the isolated task.", {
			parentSystemPrompt: "Parent identity.",
			parentToolCallId: "parent-call",
		});
		await expect(manager.getResult(record.id)).resolves.toBe("finished");
		expect(captured?.prompt).toBe("Do the isolated task.");
		expect(Object.keys(captured?.tools ?? {})).toEqual([
			"python",
			"Agent",
			"get_subagent_result",
			"steer_subagent",
		]);
		const childToolNames = Object.keys(captured?.tools ?? {});
		for (const fileTool of ["ls", "find", "read", "grep", "write", "edit"]) {
			expect(childToolNames).not.toContain(fileTool);
		}
		expect(captured?.systemPrompt).toContain("Parent identity.");
		expect(events[0]).toBe("start");
		expect(events.at(-1)).toBe("end");

		const snapshot = manager.get(record.id);
		expect(snapshot).toMatchObject({
			status: "completed",
			result: "finished",
			parentToolCallId: "parent-call",
		});
		expect(snapshot?.messages).toHaveLength(2);
		expect(snapshot?.messages[0]).toMatchObject({ role: "user" });
		expect(snapshot?.messages[1]).toMatchObject({
			role: "assistant",
			parts: expect.arrayContaining([
				expect.objectContaining({ type: "text", text: "finished" }),
			]),
		});
	});

	it("filters the complete inherited pool through an agent Markdown allowlist", async () => {
		let names: string[] = [];
		const agentLoader = new AgentLoader({
			includeBundled: false,
			definitions: [
				`---
name: worker
description: filtered worker
prompt_mode: append
tools: python, Agent, unavailable
---
Only use the selected tools.`,
			],
		});
		const manager = new SubagentManager({
			runner: async (request) => {
				names = Object.keys(request.tools);
				return "ok";
			},
			prepareSpawn: async () => ({
				tools: { python: domainTool, preview: domainTool },
			}),
			agentLoader,
			createId: () => "child-filtered",
		});
		const record = await manager.spawn("worker", "inspect");
		await manager.getResult(record.id);
		expect(names).toEqual(["python", "Agent"]);
	});

	it("awaits supervisor finalization before resolving", async () => {
		const release = deferred<void>();
		const finalize = vi.fn(async () => {
			await release.promise;
			return undefined;
		});
		const cleanup = vi.fn(async () => undefined);
		const manager = managerWith(async () => "candidate", {
			prepareSpawn: async () => ({
				tools: {},
				context: { workspaceId: "workspace-1" },
				finalize,
				cleanup,
			}),
		});
		const record = await manager.spawn("worker", "change it");
		let settled = false;
		void manager.getResult(record.id).finally(() => {
			settled = true;
		});
		await vi.waitFor(() => expect(finalize).toHaveBeenCalledOnce());
		expect(settled).toBe(false);
		expect(manager.get(record.id)?.status).toBe("running");
		release.resolve();
		await expect(manager.getResult(record.id)).resolves.toBe("candidate");
		expect(cleanup).toHaveBeenCalledOnce();
	});

	it("uses supervisor finalization text as the parent-visible result", async () => {
		const manager = managerWith(async () => "child prose", {
			prepareSpawn: async () => ({
				tools: {},
				finalize: async ({ outcome }) =>
					outcome.status === "completed"
						? `${outcome.result}\n<authored_merge status="merged"/>`
						: undefined,
			}),
		});
		const record = await manager.spawn("worker", "change it");
		await expect(manager.getResult(record.id)).resolves.toContain(
			'<authored_merge status="merged"/>',
		);
		expect(manager.get(record.id)?.result).toContain("child prose");
	});

	it("runs cleanup when setup finishes after cancellation", async () => {
		const setup = deferred<void>();
		const cleanup = vi.fn(async () => undefined);
		const controller = new AbortController();
		const manager = managerWith(async () => "unused", {
			prepareSpawn: async () => {
				await setup.promise;
				return { tools: {}, cleanup };
			},
		});
		const spawning = manager.spawn("worker", "task", {
			setupSignal: controller.signal,
		});
		controller.abort();
		setup.resolve();
		await expect(spawning).rejects.toMatchObject({ name: "AbortError" });
		expect(cleanup).toHaveBeenCalledWith(
			expect.objectContaining({ outcome: { status: "aborted" } }),
		);
		expect(manager.list()).toEqual([]);
	});

	it("queues steering for the child's next model step", async () => {
		const started = deferred<SubagentRunRequest>();
		const release = deferred<void>();
		const manager = managerWith(async (request) => {
			started.resolve(request);
			await release.promise;
			return request.drainSteering().join("|");
		});
		const record = await manager.spawn("worker", "task");
		await started.promise;
		manager.steer(record.id, "Focus on the chorus.");
		release.resolve();
		await expect(manager.getResult(record.id)).resolves.toBe(
			"Focus on the chorus.",
		);
	});

	it("passes the owning run id into recursive workspace setup", async () => {
		const rootStarted = deferred<SubagentRunRequest>();
		const releaseRoot = deferred<void>();
		const prepared: Array<{
			id: string;
			parentSubagentId?: string;
			turnMessageId?: string;
		}> = [];
		const childToolNames = new Map<string, string[]>();
		let nextId = 0;
		const manager = managerWith(
			async (request) => {
				childToolNames.set(request.prompt, Object.keys(request.tools));
				if (request.prompt === "root") {
					rootStarted.resolve(request);
					await releaseRoot.promise;
				}
				return "done";
			},
			{
				createId: () => `run-${++nextId}`,
				prepareSpawn: async ({ id, parentSubagentId, turnMessageId }) => {
					prepared.push({ id, parentSubagentId, turnMessageId });
					return { tools: { python: domainTool } };
				},
			},
		);
		const rootTools = createSubagentTools(manager, {
			getParentSystemPrompt: () => "root system",
			turnMessageId: "durable-root-turn",
		});
		const root = (await rootTools.Agent.execute?.(
			{
				subagent_type: "worker",
				prompt: "root",
				description: "root task",
				run_in_background: true,
			},
			{ toolCallId: "root-call", messages: [] },
		)) as AgentToolOutput;
		const request = await rootStarted.promise;
		const nested = (await request.tools.Agent?.execute?.(
			{
				subagent_type: "worker",
				prompt: "nested",
				description: "nested task",
				run_in_background: true,
			},
			{ toolCallId: "nested-call", messages: [] },
		)) as AgentToolOutput;
		expect(prepared).toEqual([
			{
				id: root.agent_id,
				parentSubagentId: undefined,
				turnMessageId: "durable-root-turn",
			},
			{
				id: nested.agent_id,
				parentSubagentId: root.agent_id,
				turnMessageId: "durable-root-turn",
			},
		]);
		await vi.waitFor(() => expect(childToolNames.has("nested")).toBe(true));
		expect(childToolNames.get("root")).toEqual([
			"python",
			"Agent",
			"get_subagent_result",
			"steer_subagent",
		]);
		expect(childToolNames.get("nested")).toEqual([
			"python",
			"Agent",
			"get_subagent_result",
			"steer_subagent",
		]);
		releaseRoot.resolve();
		await Promise.all([
			manager.getResult(root.agent_id),
			manager.getResult(nested.agent_id),
		]);
	});

	it("dispose waits for aborted runs to finalize and clean up", async () => {
		const releaseCleanup = deferred<void>();
		const cleanup = vi.fn(async () => releaseCleanup.promise);
		const manager = managerWith(
			(request) =>
				new Promise((_resolve, reject) => {
					request.abortSignal.addEventListener("abort", () => {
						const error = new Error("stopped");
						error.name = "AbortError";
						reject(error);
					});
				}),
			{ prepareSpawn: async () => ({ tools: {}, cleanup }) },
		);
		await manager.spawn("worker", "task");
		let disposed = false;
		const disposing = manager.dispose().then(() => {
			disposed = true;
		});
		await vi.waitFor(() => expect(cleanup).toHaveBeenCalledOnce());
		expect(disposed).toBe(false);
		releaseCleanup.resolve();
		await disposing;
		expect(manager.list()[0]?.status).toBe("aborted");
	});
});

describe("subagent tools", () => {
	it("foreground cancellation aborts its child", async () => {
		const manager = managerWith(
			(request) =>
				new Promise((_resolve, reject) => {
					request.abortSignal.addEventListener("abort", () => {
						const error = new Error("stopped");
						error.name = "AbortError";
						reject(error);
					});
				}),
		);
		const tools = createSubagentTools(manager, {
			getParentSystemPrompt: () => "parent",
		});
		const controller = new AbortController();
		const execution = Promise.resolve(
			tools.Agent.execute?.(
				{
					subagent_type: "worker",
					prompt: "task",
					description: "test task",
				},
				{
					toolCallId: "parent-call",
					messages: [],
					abortSignal: controller.signal,
				},
			),
		) as Promise<AgentToolOutput>;
		await vi.waitFor(() => expect(manager.list()).toHaveLength(1));
		controller.abort();
		await expect(execution).rejects.toMatchObject({ name: "AbortError" });
		await vi.waitFor(() => expect(manager.list()[0]?.status).toBe("aborted"));
	});

	it("foreground cancellation waits once authored finalization begins", async () => {
		const finalizing = deferred<void>();
		const releaseFinalization = deferred<void>();
		const manager = managerWith(async () => "candidate", {
			prepareSpawn: async () => ({
				tools: {},
				finalize: async () => {
					finalizing.resolve();
					await releaseFinalization.promise;
					return "merged";
				},
			}),
		});
		const tools = createSubagentTools(manager, {
			getParentSystemPrompt: () => "parent",
		});
		const controller = new AbortController();
		let settled = false;
		const execution = (
			Promise.resolve(
				tools.Agent.execute?.(
					{
						subagent_type: "worker",
						prompt: "task",
						description: "test task",
					},
					{
						toolCallId: "parent-call",
						messages: [],
						abortSignal: controller.signal,
					},
				),
			) as Promise<AgentToolOutput>
		).finally(() => {
			settled = true;
		});
		await finalizing.promise;
		controller.abort();
		await Promise.resolve();
		expect(settled).toBe(false);
		expect(manager.list()[0]?.status).toBe("running");
		releaseFinalization.resolve();
		await expect(execution).resolves.toMatchObject({
			status: "completed",
			result: "merged",
		});
		expect(manager.list()[0]?.status).toBe("completed");
	});

	it("join cancellation leaves a background child running", async () => {
		const manager = managerWith(() => new Promise(() => undefined));
		const tools = createSubagentTools(manager, {
			getParentSystemPrompt: () => "parent",
		});
		const background = (await tools.Agent.execute?.(
			{
				subagent_type: "worker",
				prompt: "task",
				description: "test task",
				run_in_background: true,
			},
			{
				toolCallId: "parent-call",
				messages: [],
			},
		)) as AgentToolOutput;
		const controller = new AbortController();
		const joining = Promise.resolve(
			tools.get_subagent_result.execute?.(
				{ agent_id: background.agent_id },
				{
					toolCallId: "join-call",
					messages: [],
					abortSignal: controller.signal,
				},
			),
		) as Promise<AgentToolOutput>;
		controller.abort();
		await expect(joining).rejects.toMatchObject({ name: "AbortError" });
		expect(manager.get(background.agent_id)?.status).toBe("running");
		manager.abort();
	});

	it("does not return a background id before workspace setup", async () => {
		const setup = deferred<void>();
		const manager = managerWith(async () => "done", {
			prepareSpawn: async () => {
				await setup.promise;
				return { tools: {} };
			},
		});
		const tools = createSubagentTools(manager, {
			getParentSystemPrompt: () => undefined,
		});
		let settled = false;
		const spawning = (
			Promise.resolve(
				tools.Agent.execute?.(
					{
						subagent_type: "worker",
						prompt: "task",
						description: "test task",
						run_in_background: true,
					},
					{
						toolCallId: "parent-call",
						messages: [],
					},
				),
			) as Promise<AgentToolOutput>
		).finally(() => {
			settled = true;
		});
		await Promise.resolve();
		expect(settled).toBe(false);
		setup.resolve();
		await expect(spawning).resolves.toMatchObject({
			agent_id: "child-1",
			status: "running",
			subagent: { id: "child-1" },
		});
	});
});
