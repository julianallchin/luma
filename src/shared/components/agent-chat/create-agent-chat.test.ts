import { afterEach, describe, expect, it, vi } from "vitest";
import type {
	AgentThread,
	AgentThreadDetail,
	AgentThreadMessage,
	AppendAgentThreadMessagesInput,
	CreateAgentThreadInput,
} from "@/bindings/schema";
import type { PiAgentModel } from "@/shared/lib/agent/pi-agent-loop";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import { TEST_PI_MODEL, testTextStream } from "@/test/pi-model";
import {
	AuthoredTurnConflictError,
	createAgentChat,
} from "./create-agent-chat";

function chat() {
	return createAgentChat<{ name: string }>({
		agentKind: "track_copilot",
		subjectKind: "track",
		createModel: () => null,
		buildTools: () => ({}),
		buildSystem: () => "",
		vocab: {
			verbs: {},
			formatLabel: () => ({ verb: "", detail: null }),
		},
		applyAuthoredState: () => undefined,
		refreshAuthoredState: () => undefined,
	});
}

function successfulModel(): PiAgentModel {
	return { model: TEST_PI_MODEL, streamFn: () => testTextStream("Done.") };
}

function failingModel(): PiAgentModel {
	return {
		model: TEST_PI_MODEL,
		streamFn: () => testTextStream("", { error: "model failed" }),
	};
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

function installImmediateStreamEnvironment(): void {
	vi.stubGlobal("document", {
		visibilityState: "hidden",
		addEventListener: () => undefined,
		removeEventListener: () => undefined,
	});
	vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
		callback(performance.now());
		return 1;
	});
	vi.stubGlobal("cancelAnimationFrame", () => undefined);
}

type FakeThread = AgentThreadDetail;

type ThreadBackendOptions = {
	prepare?: (args: Record<string, unknown>) => unknown | Promise<unknown>;
	finalize?: (args: Record<string, unknown>) => unknown | Promise<unknown>;
	recover?: (args: Record<string, unknown>) => unknown | Promise<unknown>;
	restore?: (args: Record<string, unknown>) => unknown | Promise<unknown>;
};

function installThreadBackend(
	seed: FakeThread[] = [],
	options: ThreadBackendOptions = {},
) {
	const threads = new Map(seed.map((detail) => [detail.thread.id, detail]));
	const appends = new Map<
		string,
		{ requestJson: string; result: AgentThreadMessage[] }
	>();
	const commands: string[] = [];
	const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
	let nextId = seed.length + 1;
	setInvoke(async <T>(command: string, args = {}) => {
		commands.push(command);
		calls.push({ command, args });
		switch (command) {
			case "agent_thread_list": {
				const filter = args as {
					agentKind: string | null;
					subjectKind: string | null;
					subjectId: string | null;
				};
				return [...threads.values()]
					.map((detail) => detail.thread)
					.filter(
						(thread) =>
							(!filter.agentKind || thread.agentKind === filter.agentKind) &&
							(!filter.subjectKind ||
								thread.subjectKind === filter.subjectKind) &&
							(!filter.subjectId || thread.subjectId === filter.subjectId),
					) as T;
			}
			case "agent_thread_create": {
				const input = (args as { input: CreateAgentThreadInput }).input;
				const id = `thread-${nextId++}`;
				const now = `2026-08-01T00:00:0${nextId}Z`;
				const thread: AgentThread = {
					id,
					ownerUserId: null,
					agentKind: input.agentKind,
					subjectKind: input.subjectKind,
					subjectId: input.subjectId,
					implementationId: input.implementationId,
					venueId: input.venueId,
					scoreId: input.scoreId,
					forkedFromThreadId: null,
					forkedAtMessageId: null,
					title: input.title,
					createdAt: now,
					updatedAt: now,
				};
				threads.set(id, { thread, messages: [] });
				return thread as T;
			}
			case "agent_thread_get": {
				const id = (args as { threadId: string }).threadId;
				const detail = threads.get(id);
				if (!detail) throw new Error(`missing thread ${id}`);
				return detail as T;
			}
			case "agent_thread_append_messages": {
				const input = args as {
					threadId: string;
					input: AppendAgentThreadMessagesInput;
				};
				const detail = threads.get(input.threadId);
				if (!detail) throw new Error(`missing thread ${input.threadId}`);
				const appendKey = `${input.threadId}:${input.input.operationId}`;
				const requestJson = JSON.stringify(input.input.messages);
				const prior = appends.get(appendKey);
				if (prior) {
					if (prior.requestJson !== requestJson) {
						throw new Error("append operation rebound to different content");
					}
					return prior.result as T;
				}
				const currentHead = detail.messages.at(-1)?.id ?? null;
				if (input.input.expectedHeadMessageId !== currentHead) {
					throw new Error(
						"Agent transcript changed before append; reload the conversation before retrying",
					);
				}
				const messageIds = new Set<string>();
				for (const message of input.input.messages) {
					if (
						(message.id !== null && !messageIds.add(message.id)) ||
						(message.id !== null &&
							detail.messages.some((existing) => existing.id === message.id))
					) {
						throw new Error("message id already exists");
					}
				}
				const nextSeq =
					detail.messages.reduce(
						(max, message) => Math.max(max, message.seq),
						-1,
					) + 1;
				const appended = input.input.messages.map(
					(message, index): AgentThreadMessage => ({
						id: message.id ?? `message-${detail.messages.length + index}`,
						threadId: input.threadId,
						parentMessageId:
							index === 0
								? currentHead
								: (input.input.messages[index - 1].id ??
									`message-${detail.messages.length + index - 1}`),
						seq: nextSeq + index,
						role: message.role,
						parts: message.parts,
						createdAt: "2026-08-01T00:00:00Z",
					}),
				);
				detail.messages = [...detail.messages, ...appended];
				appends.set(appendKey, { requestJson, result: appended });
				return appended as T;
			}
			case "authored_state_prepare_turn": {
				if (options.prepare) return (await options.prepare(args)) as T;
				const input = (args as { input: { assistantMessageId: string } }).input;
				return {
					documentId: "repo-1",
					preparedRevisionId: `branch-${input.assistantMessageId}`,
					document: { kind: "track_score", revision: "prepared-revision" },
				} as T;
			}
			case "authored_state_finalize_turn": {
				if (options.finalize) return (await options.finalize(args)) as T;
				return {
					status: "committed",
					documentId: "repo-1",
					revisionId: "turn-commit",
					appliedToCurrentProjection: true,
					changed: true,
					document: { kind: "track_score", revision: "turn-revision" },
				} as T;
			}
			case "authored_state_recover_turns": {
				if (options.recover) return (await options.recover(args)) as T;
				return [] as T;
			}
			case "authored_state_restore": {
				if (options.restore) return (await options.restore(args)) as T;
				return {
					documentId: "repo-1",
					revisionId: "restore-commit",
					appliedToCurrentProjection: true,
					document: { kind: "track_score", revision: "score-revision" },
					forkedThreadId: null,
				} as T;
			}
			default:
				throw new Error(`unexpected command ${command}`);
		}
	});
	return { threads, commands, calls };
}

function persistedThread(
	id: string,
	overrides: Partial<AgentThread> = {},
): FakeThread {
	return {
		thread: {
			id,
			ownerUserId: null,
			agentKind: "track_copilot",
			subjectKind: "track",
			subjectId: "track-1",
			implementationId: null,
			venueId: "venue-a",
			scoreId: "score-a",
			forkedFromThreadId: null,
			forkedAtMessageId: null,
			title: "Existing",
			createdAt: "2026-08-01T00:00:00Z",
			updatedAt: "2026-08-01T00:00:00Z",
			...overrides,
		},
		messages: [],
	};
}

afterEach(() => {
	resetInvoke();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe("createAgentChat bridge scopes", () => {
	it("keeps bridges for the same subject isolated by venue and score", () => {
		const agent = chat();
		const a = { name: "venue A / score A" };
		const b = { name: "venue B / score B" };

		agent.registerBridge("track-1", a, {
			principalId: "user-a",
			venueId: "venue-a",
			scoreId: "score-a",
		});
		agent.registerBridge("track-1", b, {
			principalId: "user-a",
			venueId: "venue-b",
			scoreId: "score-b",
		});

		expect(
			agent.getBridge("track-1", {
				principalId: "user-a",
				venueId: "venue-a",
				scoreId: "score-a",
			}),
		).toBe(a);
		expect(
			agent.getBridge("track-1", {
				principalId: "user-a",
				venueId: "venue-b",
				scoreId: "score-b",
			}),
		).toBe(b);
		expect(agent.getBridge("track-1")).toBeNull();
	});

	it("does not let stale cleanup remove a newer bridge", () => {
		const agent = chat();
		const init = {
			principalId: "user-a",
			venueId: "venue-a",
			scoreId: "score-a",
		};
		const first = { name: "first" };
		const second = { name: "second" };
		const unregisterFirst = agent.registerBridge("track-1", first, init);
		const unregisterSecond = agent.registerBridge("track-1", second, init);

		unregisterFirst();
		expect(agent.getBridge("track-1", init)).toBe(second);

		unregisterSecond();
		expect(agent.getBridge("track-1", init)).toBeNull();
	});

	it("restores an older live registration when a newer one unmounts", () => {
		const agent = chat();
		const init = {
			principalId: "user-a",
			venueId: "venue-a",
			scoreId: "score-a",
		};
		const first = { name: "first" };
		const second = { name: "second" };
		const unregisterFirst = agent.registerBridge("track-1", first, init);
		const unregisterSecond = agent.registerBridge("track-1", second, init);

		unregisterSecond();
		expect(agent.getBridge("track-1", init)).toBe(first);

		unregisterFirst();
		expect(agent.getBridge("track-1", init)).toBeNull();
	});

	it("never reuses an in-memory scope across account principals", () => {
		const agent = chat();
		const alice = { name: "alice" };
		const bob = { name: "bob" };
		const shared = { venueId: "venue-a", scoreId: "score-a" };

		agent.registerBridge("track-1", alice, {
			...shared,
			principalId: "alice",
		});
		agent.registerBridge("track-1", bob, {
			...shared,
			principalId: "bob",
		});

		expect(
			agent.getBridge("track-1", { ...shared, principalId: "alice" }),
		).toBe(alice);
		expect(agent.getBridge("track-1", { ...shared, principalId: "bob" })).toBe(
			bob,
		);
	});

	it("rejects a bridge that disagrees with its immutable scope", () => {
		const agent = createAgentChat<{ venueId: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			validateThreadScope: ({ init, bridge }) => {
				if (bridge && bridge.venueId !== init.venueId) {
					throw new Error("bridge scope mismatch");
				}
			},
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});
		const init = {
			principalId: null,
			venueId: "venue-a",
			scoreId: "score-a",
		};

		expect(() =>
			agent.registerBridge("track-1", { venueId: "venue-b" }, init),
		).toThrow("bridge scope mismatch");
		expect(agent.getBridge("track-1", init)).toBeNull();
	});
});

describe("createAgentChat conversation lifecycle", () => {
	const init = {
		principalId: null,
		venueId: "venue-a",
		scoreId: "score-a",
	};

	it("rejects an incomplete immutable scope before thread lookup", async () => {
		const backend = installThreadBackend();
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			validateThreadScope: ({ init: candidate }) => {
				if (!candidate.venueId || !candidate.scoreId) {
					throw new Error("missing immutable scope");
				}
			},
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});

		await expect(
			agent.resolveThreadFor("track-1", { principalId: null }),
		).rejects.toThrow("missing immutable scope");
		expect(backend.commands).toEqual([]);
	});

	it("starts a new durable thread without clearing or deleting the old one", async () => {
		const existing = persistedThread("thread-1");
		const backend = installThreadBackend([existing]);
		const agent = chat();

		expect(await agent.resolveThreadFor("track-1", init)).toBe("thread-1");
		const nextId = await agent.newThreadFor("track-1", init);

		expect(nextId).toBe("thread-2");
		expect(backend.threads.has("thread-1")).toBe(true);
		expect(backend.threads.has("thread-2")).toBe(true);
		expect(backend.commands).not.toContain("agent_thread_delete");
	});

	it("lists only conversations in the exact venue and score scope", async () => {
		installThreadBackend([
			persistedThread("exact"),
			persistedThread("other-venue", { venueId: "venue-b" }),
			persistedThread("other-score", { scoreId: "score-b" }),
		]);
		const agent = chat();

		const result = await agent.listThreadsFor("track-1", init);

		expect(result.map((thread) => thread.id)).toEqual(["exact"]);
	});

	it("refuses to activate a thread from another exact scope", async () => {
		installThreadBackend([
			persistedThread("exact"),
			persistedThread("wrong", { scoreId: "score-b" }),
		]);
		const agent = chat();
		await agent.resolveThreadFor("track-1", init);

		await expect(agent.openThreadFor("track-1", "wrong", init)).rejects.toThrow(
			"does not belong to this conversation scope",
		);
		expect(await agent.resolveThreadFor("track-1", init)).toBe("exact");
	});

	it("recovers durable turns and applies only the last projection before ready", async () => {
		const existing = persistedThread("thread-1");
		existing.messages = [
			{
				id: "user-1",
				threadId: "thread-1",
				parentMessageId: null,
				seq: 0,
				role: "user",
				parts: [{ type: "text", text: "Make it blue" }],
				createdAt: "2026-08-01T00:00:00Z",
			},
			{
				id: "assistant-1",
				threadId: "thread-1",
				parentMessageId: "user-1",
				seq: 1,
				role: "assistant",
				parts: [{ type: "text", text: "Done." }],
				createdAt: "2026-08-01T00:00:01Z",
			},
		];
		const backend = installThreadBackend([existing], {
			recover: () => [
				{
					status: "committed",
					documentId: "repo-1",
					revisionId: "recovered-1",
					appliedToCurrentProjection: false,
					changed: true,
					document: { kind: "track_score", revision: "revision-1" },
				},
				{
					status: "conflicted",
					documentId: "repo-1",
					preparedRevisionId: "conflicted-branch",
					conflicts: [{}],
				},
				{
					status: "committed",
					documentId: "repo-1",
					revisionId: "recovered-2",
					appliedToCurrentProjection: true,
					changed: true,
					document: { kind: "track_score", revision: "revision-2" },
				},
			],
		});
		const applied: string[] = [];
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: async ({ source, result }) => {
				await Promise.resolve();
				applied.push(`${source}:${result.revisionId}`);
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		expect(await agent.resolveThreadFor("track-1", init)).toBe("thread-1");

		expect(applied).toEqual(["recovery:recovered-2"]);
		expect(
			backend.calls.filter(
				(call) => call.command === "authored_state_recover_turns",
			),
		).toEqual([
			{
				command: "authored_state_recover_turns",
				args: { threadId: "thread-1" },
			},
		]);
	});

	it("restores state through the active thread without changing conversations", async () => {
		const order: string[] = [];
		const backend = installThreadBackend([persistedThread("thread-1")], {
			restore: () => {
				order.push("restore");
				return {
					documentId: "repo-1",
					revisionId: "restore-commit",
					appliedToCurrentProjection: true,
					document: { kind: "track_score", revision: "score-revision" },
					forkedThreadId: null,
				};
			},
		});
		const restored: Array<{ threadId: string; bridge: string }> = [];
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			checkpointAuthoredState: ({ threadId, bridge }) => {
				order.push(`checkpoint:${threadId}:${bridge.name}`);
			},
			applyAuthoredState: ({ threadId, bridge }) => {
				order.push("apply");
				restored.push({ threadId, bridge: bridge.name });
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		await agent.restoreStateFor("track-1", "old-commit", "state_only", init);

		const call = backend.calls.find(
			(candidate) => candidate.command === "authored_state_restore",
		);
		expect(call?.args).toEqual({
			input: {
				threadId: "thread-1",
				targetRevisionId: "old-commit",
				operationId: expect.any(String),
				mode: "state_only",
			},
		});
		expect(restored).toEqual([{ threadId: "thread-1", bridge: "mounted" }]);
		expect(order).toEqual(["checkpoint:thread-1:mounted", "restore", "apply"]);
		expect(await agent.resolveThreadFor("track-1", init)).toBe("thread-1");
	});

	it("opens the immutable transcript fork returned by restore-and-rewind", async () => {
		const backend = installThreadBackend(
			[
				persistedThread("thread-1"),
				persistedThread("thread-fork", {
					forkedFromThreadId: "thread-1",
					forkedAtMessageId: "assistant-1",
				}),
			],
			{
				restore: () => ({
					documentId: "document-1",
					revisionId: "restore-revision",
					appliedToCurrentProjection: true,
					document: { kind: "track_score", revision: "score-revision" },
					forkedThreadId: "thread-fork",
				}),
			},
		);
		const appliedThrough: string[] = [];
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: { verbs: {}, formatLabel: () => ({ verb: "", detail: null }) },
			applyAuthoredState: ({ threadId }) => {
				appliedThrough.push(threadId);
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		await agent.restoreStateFor(
			"track-1",
			"old-revision",
			"state_and_conversation",
			init,
		);

		expect(appliedThrough).toEqual(["thread-1"]);
		expect(await agent.resolveThreadFor("track-1", init)).toBe("thread-fork");
		expect(
			backend.calls.find((call) => call.command === "authored_state_restore")
				?.args,
		).toMatchObject({
			input: {
				threadId: "thread-1",
				targetRevisionId: "old-revision",
				mode: "state_and_conversation",
			},
		});
	});

	it("does not restore until the live editor checkpoint succeeds", async () => {
		const backend = installThreadBackend([persistedThread("thread-1")]);
		let checkpointAttempts = 0;
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			checkpointAuthoredState: () => {
				checkpointAttempts += 1;
				if (checkpointAttempts === 1) {
					throw new Error("checkpoint failed");
				}
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		await expect(
			agent.restoreStateFor("track-1", "old-commit", "state_only", init),
		).rejects.toThrow("checkpoint failed");
		expect(
			backend.calls.filter((call) => call.command === "authored_state_restore"),
		).toHaveLength(0);

		await agent.restoreStateFor("track-1", "old-commit", "state_only", init);

		expect(checkpointAttempts).toBe(2);
		expect(
			backend.calls.filter((call) => call.command === "authored_state_restore"),
		).toHaveLength(1);
	});

	it("reuses a restore operation id after response loss", async () => {
		const operationIds: string[] = [];
		let attempts = 0;
		installThreadBackend([persistedThread("thread-1")], {
			restore: (args) => {
				const input = (
					args as {
						input: { operationId: string };
					}
				).input;
				operationIds.push(input.operationId);
				attempts += 1;
				if (attempts === 1) throw new Error("response lost");
				return {
					documentId: "repo-1",
					revisionId: "same-restore-commit",
					appliedToCurrentProjection: true,
					document: { kind: "track_score", revision: "score-revision" },
					forkedThreadId: null,
				};
			},
		});
		const applied: string[] = [];
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: ({ result }) => {
				applied.push(result.revisionId);
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		await expect(
			agent.restoreStateFor("track-1", "old-commit", "state_only", init),
		).rejects.toThrow("response lost");
		await agent.restoreStateFor("track-1", "old-commit", "state_only", init);

		expect(operationIds).toHaveLength(2);
		expect(operationIds[0]).toBe(operationIds[1]);
		expect(applied).toEqual(["same-restore-commit"]);
	});

	it("reapplies a returned restore without creating another commit", async () => {
		const backend = installThreadBackend([persistedThread("thread-1")]);
		let applyAttempts = 0;
		let checkpointAttempts = 0;
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => null,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			checkpointAuthoredState: () => {
				checkpointAttempts += 1;
			},
			applyAuthoredState: () => {
				applyAttempts += 1;
				if (applyAttempts === 1) throw new Error("editor refresh failed");
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		await expect(
			agent.restoreStateFor("track-1", "old-commit", "state_only", init),
		).rejects.toThrow("editor refresh failed");
		await agent.restoreStateFor("track-1", "old-commit", "state_only", init);

		expect(applyAttempts).toBe(2);
		expect(checkpointAttempts).toBe(1);
		expect(
			backend.calls.filter((call) => call.command === "authored_state_restore"),
		).toHaveLength(1);
	});

	it("prepares before persisting the assistant and finalizes afterward", async () => {
		installImmediateStreamEnvironment();
		const backend = installThreadBackend([persistedThread("thread-1")]);
		let toolIdentity:
			| { threadId: string; turnMessageId: string; alreadyDurable: boolean }
			| undefined;
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: successfulModel,
			buildTools: ({ threadId, turnMessageId }) => {
				toolIdentity = {
					threadId,
					turnMessageId,
					alreadyDurable:
						backend.threads
							.get(threadId)
							?.messages.some(
								(message) =>
									message.id === turnMessageId && message.role === "user",
							) ?? false,
				};
				return {};
			},
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		await agent.send("track-1", "Make it blue", init);
		expect(toolIdentity).toEqual({
			threadId: "thread-1",
			turnMessageId: backend.threads.get("thread-1")?.messages[0].id,
			alreadyDurable: true,
		});

		expect(
			backend.commands.filter((command) =>
				[
					"authored_state_recover_turns",
					"agent_thread_append_messages",
					"authored_state_prepare_turn",
					"authored_state_finalize_turn",
				].includes(command),
			),
		).toEqual([
			"authored_state_recover_turns",
			"agent_thread_append_messages",
			"authored_state_prepare_turn",
			"agent_thread_append_messages",
			"authored_state_finalize_turn",
		]);
	});

	it("injects steering into the active Pi run and persists the event order", async () => {
		installImmediateStreamEnvironment();
		const backend = installThreadBackend([persistedThread("thread-1")]);
		const firstText = deferred<void>();
		const releaseFirst = deferred<void>();
		const prompts: string[] = [];
		let call = 0;
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: () => ({
				model: TEST_PI_MODEL,
				streamFn: (_model, context) => {
					prompts.push(JSON.stringify(context.messages));
					call += 1;
					return call === 1
						? testTextStream("Initial answer.", {
								afterText: async () => {
									firstText.resolve();
									await releaseFirst.promise;
								},
							})
						: testTextStream("Revised answer.");
				},
			}),
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		const running = agent.send("track-1", "Start here", init);
		await firstText.promise;
		agent.steer("track-1", "Change direction", init);
		releaseFirst.resolve();
		await running;

		expect(prompts).toHaveLength(2);
		expect(prompts[1]).toContain("Initial answer.");
		expect(prompts[1]).toContain("Change direction");
		expect(
			backend.threads.get("thread-1")?.messages.map((message) => message.role),
		).toEqual(["user", "assistant", "user", "assistant"]);
	});

	it("retries prepare response loss with the original captured state", async () => {
		installImmediateStreamEnvironment();
		const graphInit = {
			...init,
			implementationId: "implementation-1",
		};
		const prepared: Array<{ messageId: string; nodeId: string }> = [];
		let prepareAttempts = 0;
		const backend = installThreadBackend(
			[
				persistedThread("thread-1", {
					agentKind: "pattern_graph",
					subjectKind: "pattern",
					subjectId: "pattern-1",
					implementationId: "implementation-1",
				}),
			],
			{
				prepare: (args) => {
					const input = (
						args as {
							input: {
								assistantMessageId: string;
								graph: { nodes: Array<{ id: string }> };
							};
						}
					).input;
					prepared.push({
						messageId: input.assistantMessageId,
						nodeId: input.graph.nodes[0].id,
					});
					prepareAttempts += 1;
					if (prepareAttempts === 1) throw new Error("prepare response lost");
					return {
						documentId: "repo-1",
						preparedRevisionId: "captured-branch",
						document: {
							kind: "pattern_graph",
							revision: "prepared-revision",
							graph: input.graph,
						},
					};
				},
			},
		);
		const bridge = { name: "state-at-finish" };
		let captureCount = 0;
		const agent = createAgentChat<{ name: string }>({
			agentKind: "pattern_graph",
			subjectKind: "pattern",
			createModel: successfulModel,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			captureAuthoredState: ({ bridge: finishedBridge }) => {
				captureCount += 1;
				return {
					graph: {
						nodes: [
							{
								id: finishedBridge.name,
								typeId: "constant",
								params: {},
								positionX: null,
								positionY: null,
							},
						],
						edges: [],
						args: [],
					},
				};
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("pattern-1", bridge, graphInit);
		const finished: unknown[] = [];
		agent.onSessionFinished((event) => finished.push(event));

		await expect(
			agent.send("pattern-1", "Make it blue", graphInit),
		).rejects.toThrow("prepare response lost");
		expect(finished).toEqual([]);
		expect(
			backend.threads.get("thread-1")?.messages.map((m) => m.role),
		).toEqual(["user"]);
		bridge.name = "state-after-failure";

		const nextThread = await agent.newThreadFor("pattern-1", graphInit);

		expect(nextThread).toBe("thread-2");
		expect(captureCount).toBe(1);
		expect(prepared).toEqual([
			{ messageId: prepared[0].messageId, nodeId: "state-at-finish" },
			{ messageId: prepared[0].messageId, nodeId: "state-at-finish" },
		]);
		expect(finished).toHaveLength(1);
		expect(
			backend.threads.get("thread-1")?.messages.map((m) => m.role),
		).toEqual(["user", "assistant"]);
	});

	it("reuses the prepared branch and applies current state after finalize response loss", async () => {
		installImmediateStreamEnvironment();
		let prepareAttempts = 0;
		const finalizedBranches: string[] = [];
		let finalizeAttempts = 0;
		const backend = installThreadBackend([persistedThread("thread-1")], {
			prepare: (args) => {
				prepareAttempts += 1;
				const input = (args as { input: { assistantMessageId: string } }).input;
				return {
					documentId: "repo-1",
					preparedRevisionId: `stable-${input.assistantMessageId}`,
					document: { kind: "track_score", revision: "prepared" },
				};
			},
			finalize: (args) => {
				const input = (args as { input: { preparedRevisionId: string } }).input;
				finalizedBranches.push(input.preparedRevisionId);
				finalizeAttempts += 1;
				if (finalizeAttempts === 1) throw new Error("finalize response lost");
				return {
					status: "committed",
					documentId: "repo-1",
					revisionId: "same-main-commit",
					appliedToCurrentProjection: false,
					changed: false,
					document: { kind: "track_score", revision: "final" },
				};
			},
		});
		const applied: string[] = [];
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: successfulModel,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: ({ result }) => {
				applied.push(result.document.revision);
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);
		const finished: unknown[] = [];
		agent.onSessionFinished((event) => finished.push(event));

		await expect(agent.send("track-1", "Make it blue", init)).rejects.toThrow(
			"finalize response lost",
		);
		expect(
			backend.threads.get("thread-1")?.messages.map((m) => m.role),
		).toEqual(["user", "assistant"]);
		expect(applied).toEqual([]);
		expect(finished).toEqual([]);

		await agent.newThreadFor("track-1", init);

		expect(prepareAttempts).toBe(1);
		expect(finalizedBranches).toEqual([
			finalizedBranches[0],
			finalizedBranches[0],
		]);
		expect(applied).toEqual(["final"]);
		expect(finished).toHaveLength(1);
	});

	it("keeps a conflicted branch without blocking the next turn", async () => {
		installImmediateStreamEnvironment();
		let finalizeAttempts = 0;
		installThreadBackend([persistedThread("thread-1")], {
			finalize: () => {
				finalizeAttempts += 1;
				if (finalizeAttempts === 1) {
					return {
						status: "conflicted",
						documentId: "repo-1",
						preparedRevisionId: "conflicted-branch",
						conflicts: [{ detail: "same node changed twice" }],
					};
				}
				return {
					status: "committed",
					documentId: "repo-1",
					revisionId: "next-turn-commit",
					appliedToCurrentProjection: true,
					changed: true,
					document: { kind: "track_score", revision: "next-revision" },
				};
			},
		});
		let applyCount = 0;
		let refreshCount = 0;
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: successfulModel,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: () => {
				applyCount += 1;
			},
			refreshAuthoredState: () => {
				refreshCount += 1;
			},
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);
		const finished: Array<string | null> = [];
		agent.onSessionFinished((event) => finished.push(event.error));

		let conflict: unknown;
		try {
			await agent.send("track-1", "First edit", init);
		} catch (error) {
			conflict = error;
		}

		expect(conflict).toBeInstanceOf(AuthoredTurnConflictError);
		expect(conflict).toMatchObject({
			preparedRevisionId: "conflicted-branch",
			conflicts: [{ detail: "same node changed twice" }],
		});
		expect(applyCount).toBe(0);
		expect(refreshCount).toBe(1);
		expect(finished).toHaveLength(1);
		expect(finished[0]).toContain("conflicted with newer edits");

		await agent.send("track-1", "Second edit", init);

		expect(applyCount).toBe(1);
		expect(refreshCount).toBe(1);
		expect(finished).toEqual([finished[0], null]);
	});

	it("finalizes authored state even when the model turn ends in error", async () => {
		vi.spyOn(console, "error").mockImplementation(() => undefined);
		installImmediateStreamEnvironment();
		const backend = installThreadBackend([persistedThread("thread-1")]);
		const finalized: string[] = [];
		const finished: Array<{ error: string | null }> = [];
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: failingModel,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: ({ result }) => {
				finalized.push(result.revisionId);
			},
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);
		agent.onSessionFinished((event) => finished.push({ error: event.error }));

		await agent.send("track-1", "Try something", init);

		expect(finalized).toHaveLength(1);
		expect(backend.commands).toContain("authored_state_prepare_turn");
		expect(backend.commands).toContain("authored_state_finalize_turn");
		expect(finished).toEqual([{ error: "model failed" }]);
		// The real backend refuses to finalize a turn whose assistant message
		// row was never persisted — an errored turn must still append it.
		const appended = backend.calls
			.filter((call) => call.command === "agent_thread_append_messages")
			.flatMap(
				(call) =>
					(call.args as { input: AppendAgentThreadMessagesInput }).input
						.messages,
			);
		expect(appended.some((message) => message.role === "assistant")).toBe(true);
		// A zero-part assistant message would fail validation on reload — the
		// recovered message must carry at least one part.
		for (const message of appended) {
			expect((message.parts as unknown[]).length).toBeGreaterThan(0);
		}
	});

	it("rejects a concurrent programmatic send before it can alter history", async () => {
		installImmediateStreamEnvironment();
		const backend = installThreadBackend([persistedThread("thread-1")]);
		const agent = createAgentChat<{ name: string }>({
			agentKind: "track_copilot",
			subjectKind: "track",
			createModel: successfulModel,
			buildTools: () => ({}),
			buildSystem: () => "",
			vocab: {
				verbs: {},
				formatLabel: () => ({ verb: "", detail: null }),
			},
			applyAuthoredState: () => undefined,
			refreshAuthoredState: () => undefined,
		});
		agent.registerBridge("track-1", { name: "mounted" }, init);

		const first = agent.send("track-1", "First prompt", init);
		await expect(agent.send("track-1", "Second prompt", init)).rejects.toThrow(
			"Wait for the current turn",
		);
		await first;

		expect(
			backend.threads.get("thread-1")?.messages.map((message) => message.role),
		).toEqual(["user", "assistant"]);
	});
});
