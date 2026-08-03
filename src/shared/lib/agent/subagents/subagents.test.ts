import { tool } from "ai";
import { MockLanguageModelV3, simulateReadableStream } from "ai/test";
import { describe, expect, it, vi } from "vitest";
import { z } from "zod";
import { AgentLoader } from "./agent-loader";
import { createAiSdkSubagentRunner } from "./ai-sdk-runner";
import { SubagentManager } from "./subagent-manager";
import { createSubagentTools } from "./tools";
import type {
	AgentConfig,
	AgentToolOutput,
	PrepareSubagentSpawn,
	SubagentRunner,
	SubagentRunRequest,
} from "./types";

const writableTool = tool({
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
			tools: { write: writableTool },
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
	await request.onUIMessageChunk({ type: "start", messageId: "answer-1" });
	await request.onUIMessageChunk({ type: "start-step" });
	await request.onUIMessageChunk({ type: "text-start", id: "text-1" });
	await request.onUIMessageChunk({
		type: "text-delta",
		id: "text-1",
		delta: text,
	});
	await request.onUIMessageChunk({ type: "text-end", id: "text-1" });
	await request.onUIMessageChunk({ type: "finish-step" });
	await request.onUIMessageChunk({ type: "finish", finishReason: "stop" });
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

describe("AI SDK runner", () => {
	it("starts from a fresh one-message transcript and streams UI chunks", async () => {
		const model = new MockLanguageModelV3({
			doStream: {
				stream: simulateReadableStream({
					chunks: [
						{ type: "stream-start", warnings: [] },
						{ type: "text-start", id: "text-1" },
						{ type: "text-delta", id: "text-1", delta: "Done." },
						{ type: "text-end", id: "text-1" },
						{
							type: "finish",
							finishReason: { unified: "stop", raw: undefined },
							usage: {
								inputTokens: {
									total: 1,
									noCache: 1,
									cacheRead: 0,
									cacheWrite: 0,
								},
								outputTokens: { total: 1, text: 1, reasoning: 0 },
							},
						},
					],
					initialDelayInMs: null,
					chunkDelayInMs: null,
				}),
			},
		});
		const runner = createAiSdkSubagentRunner({ createModel: () => model });
		const chunkTypes: string[] = [];
		const result = await runner({
			systemPrompt: "Child identity.",
			prompt: "Only this child task.",
			tools: {},
			abortSignal: new AbortController().signal,
			drainSteering: () => [],
			onUIMessageChunk: (chunk) => {
				chunkTypes.push(chunk.type);
			},
		});
		expect(result).toBe("Done.");
		expect(chunkTypes).toContain("text-delta");
		expect(model.doStreamCalls).toHaveLength(1);
		expect(JSON.stringify(model.doStreamCalls[0]?.prompt)).toContain(
			"Only this child task.",
		);
		expect(JSON.stringify(model.doStreamCalls[0]?.prompt)).not.toContain(
			"parent transcript secret",
		);
	});

	it("continues with queued steering and preserves each assistant response", async () => {
		const firstText = deferred<void>();
		const releaseFirst = deferred<void>();
		let call = 0;
		const finish = {
			type: "finish" as const,
			finishReason: { unified: "stop" as const, raw: undefined },
			usage: {
				inputTokens: { total: 1, noCache: 1, cacheRead: 0, cacheWrite: 0 },
				outputTokens: { total: 1, text: 1, reasoning: 0 },
			},
		};
		const model = new MockLanguageModelV3({
			doStream: async () => {
				call += 1;
				if (call === 1) {
					return {
						stream: new ReadableStream({
							async start(controller) {
								controller.enqueue({ type: "stream-start", warnings: [] });
								controller.enqueue({ type: "text-start", id: "first" });
								controller.enqueue({
									type: "text-delta",
									id: "first",
									delta: "Initial answer.",
								});
								firstText.resolve();
								await releaseFirst.promise;
								controller.enqueue({ type: "text-end", id: "first" });
								controller.enqueue(finish);
								controller.close();
							},
						}),
					};
				}
				return {
					stream: simulateReadableStream({
						chunks: [
							{ type: "stream-start", warnings: [] },
							{ type: "text-start", id: "follow-up" },
							{
								type: "text-delta",
								id: "follow-up",
								delta: "Revised answer.",
							},
							{ type: "text-end", id: "follow-up" },
							finish,
						],
						initialDelayInMs: null,
						chunkDelayInMs: null,
					}),
				};
			},
		});
		const runner = createAiSdkSubagentRunner({ createModel: () => model });
		const manager = managerWith(runner);
		const record = await manager.spawn("worker", "Original task.");
		await firstText.promise;
		manager.steer(record.id, "Please revise the conclusion.");
		releaseFirst.resolve();

		await expect(manager.getResult(record.id)).resolves.toBe(
			"Initial answer.\n\nRevised answer.",
		);
		expect(model.doStreamCalls).toHaveLength(2);
		const followUpPrompt = JSON.stringify(model.doStreamCalls[1]?.prompt);
		expect(followUpPrompt).toContain("Initial answer.");
		expect(followUpPrompt).toContain("Please revise the conclusion.");
		expect(manager.get(record.id)?.messages).toMatchObject([
			{ role: "user" },
			{ role: "assistant" },
			{ role: "assistant" },
		]);
	});
});

describe("SubagentManager", () => {
	it("runs with a fresh prompt and child-scoped tools", async () => {
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
		expect(captured?.tools).toMatchObject({ write: writableTool });
		expect(captured?.tools).toHaveProperty("Agent");
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

	it("filters prepared tools through the agent allowlist", async () => {
		let names: string[] = [];
		const agent = { ...generalAgent, toolNames: ["read"] };
		const manager = managerWith(
			async (request) => {
				names = Object.keys(request.tools);
				return "ok";
			},
			{
				agent,
				prepareSpawn: async () => ({
					tools: { read: writableTool, write: writableTool },
				}),
			},
		);
		const record = await manager.spawn("worker", "inspect");
		await manager.getResult(record.id);
		expect(names).toEqual(["read"]);
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
		const prepared: Array<{ id: string; parentSubagentId?: string }> = [];
		let nextId = 0;
		const manager = managerWith(
			async (request) => {
				if (request.prompt === "root") {
					rootStarted.resolve(request);
					await releaseRoot.promise;
				}
				return "done";
			},
			{
				createId: () => `run-${++nextId}`,
				prepareSpawn: async ({ id, parentSubagentId }) => {
					prepared.push({ id, parentSubagentId });
					return { tools: {} };
				},
			},
		);
		const root = await manager.spawn("worker", "root");
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
			{ id: root.id, parentSubagentId: undefined },
			{ id: nested.agent_id, parentSubagentId: root.id },
		]);
		releaseRoot.resolve();
		await Promise.all([
			manager.getResult(root.id),
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
