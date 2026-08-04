import { afterEach, describe, expect, it, vi } from "vitest";
import type { AgentThread } from "@/bindings/schema";
import type { AgentChatMessage } from "@/shared/lib/agent/messages";
import {
	appendThreadMessages,
	createScopedThread,
	listScopedThreads,
	loadThreadMessages,
	newestThread,
	normalizeThreadScope,
	type PersistedMessage,
	planThreadAppend,
	resolveThread,
	threadMatchesScope,
} from "@/shared/lib/agent/threads";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

type Call = { command: string; args: Record<string, unknown> };

function mockInvoke(handlers: Record<string, (args: never) => unknown>) {
	const calls: Call[] = [];
	setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
		calls.push({ command, args: args ?? {} });
		const handler = handlers[command];
		if (!handler) throw new Error(`unexpected command: ${command}`);
		return (await handler((args ?? {}) as never)) as T;
	});
	return calls;
}

function userMessage(id: string, text: string): AgentChatMessage {
	return { id, role: "user", parts: [{ type: "text", text }] };
}

function assistantMessage(id: string, text: string): AgentChatMessage {
	return { id, role: "assistant", parts: [{ type: "text", text }] };
}

function baselineOf(
	messages: AgentChatMessage[],
	startSeq = 1,
): PersistedMessage[] {
	return messages.map((m, i) => ({
		id: m.id,
		seq: startSeq + i,
		role: m.role,
		partsJson: JSON.stringify(m.parts),
	}));
}

function thread(id: string, updatedAt: string, createdAt = updatedAt) {
	return {
		id,
		ownerUserId: "user-1",
		agentKind: "track_copilot",
		subjectKind: "track",
		subjectId: "track-1",
		implementationId: null,
		venueId: null,
		scoreId: null,
		forkedFromThreadId: null,
		forkedAtMessageId: null,
		title: null,
		createdAt,
		updatedAt,
	} satisfies AgentThread;
}

function scopedThread(
	id: string,
	updatedAt: string,
	venueId: string | null,
	scoreId: string | null,
) {
	return { ...thread(id, updatedAt), venueId, scoreId } satisfies AgentThread;
}

const TRACK_SCOPE = normalizeThreadScope("track_copilot", "track", "track-1", {
	principalId: "user-1",
	venueId: "v-1",
	scoreId: "s-1",
});

afterEach(() => {
	resetInvoke();
	vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// planThreadAppend — exact-prefix validation and the new tail
// ---------------------------------------------------------------------------

describe("planThreadAppend", () => {
	it("appends everything when nothing is persisted yet", () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		expect(planThreadAppend([], current)).toEqual({ append: current });
	});

	it("is a no-op when the persisted history already matches", () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		expect(planThreadAppend(baselineOf(current), current)).toEqual({
			append: [],
		});
	});

	it("appends only the new tail of a finished turn", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
		];
		const current = [...persisted, userMessage("u2", "again")];
		expect(planThreadAppend(baselineOf(persisted), current)).toEqual({
			append: [current[2]],
		});
	});

	it("rejects changed persisted parts", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "draft"),
		];
		const current = [persisted[0], assistantMessage("a1", "final")];
		expect(() => planThreadAppend(baselineOf(persisted), current)).toThrow(
			"diverged",
		);
	});

	it("rejects a changed persisted id", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
			userMessage("u2", "typo"),
		];
		const current = [persisted[0], persisted[1], userMessage("u3", "fixed")];
		expect(() => planThreadAppend(baselineOf(persisted), current)).toThrow(
			"diverged",
		);
	});

	it("rejects removed persisted messages", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
		];
		expect(() =>
			planThreadAppend(baselineOf(persisted), [persisted[0]]),
		).toThrow("not an exact prefix");
	});

	it("rejects a role change on the same id", () => {
		const persisted = [userMessage("u1", "hi")];
		const current = [assistantMessage("u1", "hi")];
		expect(() => planThreadAppend(baselineOf(persisted), current)).toThrow(
			"diverged",
		);
	});
});

// ---------------------------------------------------------------------------
// appendThreadMessages — exact-prefix validation wired to the command
// ---------------------------------------------------------------------------

describe("appendThreadMessages", () => {
	it("issues no commands when nothing changed", async () => {
		const current = [userMessage("u1", "hi")];
		const calls = mockInvoke({});
		const next = await appendThreadMessages("t1", baselineOf(current), current);
		expect(calls).toEqual([]);
		expect(next).toEqual(baselineOf(current));
	});

	it("appends only the new tail", async () => {
		const persisted = [userMessage("u1", "hi")];
		const current = [...persisted, assistantMessage("a1", "hello")];
		const calls = mockInvoke({
			agent_thread_append_messages: () => [
				{ id: "a1", threadId: "t1", seq: 2, role: "assistant" },
			],
		});
		const next = await appendThreadMessages(
			"t1",
			baselineOf(persisted),
			current,
		);
		expect(calls.map((c) => c.command)).toEqual([
			"agent_thread_append_messages",
		]);
		expect(calls[0].args.input).toEqual({
			operationId: expect.any(String),
			expectedHeadMessageId: "u1",
			messages: [{ id: "a1", role: "assistant", parts: current[1].parts }],
		});
		expect(next.map((m) => m.seq)).toEqual([1, 2]);
	});

	it.each([
		[
			"edited",
			(persisted: AgentChatMessage[]) => [
				persisted[0],
				assistantMessage("a1", "changed"),
				persisted[2],
			],
		],
		["removed", (persisted: AgentChatMessage[]) => persisted.slice(0, 2)],
		[
			"reordered",
			(persisted: AgentChatMessage[]) => [
				persisted[1],
				persisted[0],
				persisted[2],
			],
		],
	] as const)(
		"fails closed with zero IPC when history was %s",
		async (_label, mutate) => {
			const persisted = [
				userMessage("u1", "hi"),
				assistantMessage("a1", "hello"),
				userMessage("u2", "again"),
			];
			const calls = mockInvoke({
				agent_thread_append_messages: () => {
					throw new Error("must not execute");
				},
			});
			await expect(
				appendThreadMessages("t1", baselineOf(persisted), mutate(persisted)),
			).rejects.toThrow();
			expect(calls).toEqual([]);
		},
	);

	it("keeps a baseline that makes the next append a no-op", async () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		mockInvoke({
			// The backend echoes parts through serde_json, which may reorder keys;
			// the baseline must still match the in-memory messages next time.
			agent_thread_append_messages: () => [
				{ id: "u1", threadId: "t1", seq: 0, role: "user" },
				{ id: "a1", threadId: "t1", seq: 1, role: "assistant" },
			],
		});
		const baseline = await appendThreadMessages("t1", [], current);
		expect(planThreadAppend(baseline, current)).toEqual({ append: [] });
	});

	it("reuses the operation id after an IPC response is lost", async () => {
		const current = [userMessage("u1", "hi")];
		const operationIds: string[] = [];
		let attempts = 0;
		mockInvoke({
			agent_thread_append_messages: (raw) => {
				const args = raw as {
					input: { operationId: string };
				};
				operationIds.push(args.input.operationId);
				attempts += 1;
				if (attempts === 1) throw new Error("IPC response lost");
				return [{ id: "u1", threadId: "t1", seq: 0, role: "user" }];
			},
		});

		await expect(appendThreadMessages("t1", [], current)).rejects.toThrow(
			"IPC response lost",
		);
		await appendThreadMessages("t1", [], current);

		expect(operationIds).toHaveLength(2);
		expect(operationIds[1]).toBe(operationIds[0]);
	});

	it("retires the operation id after a successful response", async () => {
		const firstTurn = [userMessage("u1", "hi")];
		const operationIds: string[] = [];
		mockInvoke({
			agent_thread_append_messages: (raw) => {
				const args = raw as {
					threadId: string;
					input: {
						operationId: string;
						messages: Array<{ id: string; role: string }>;
					};
				};
				operationIds.push(args.input.operationId);
				return args.input.messages.map((message) => ({
					id: message.id,
					threadId: args.threadId,
					seq: operationIds.length - 1,
					role: message.role,
				}));
			},
		});

		const baseline = await appendThreadMessages("t1", [], firstTurn);
		await appendThreadMessages("t1", baseline, [
			...firstTurn,
			assistantMessage("a1", "hello"),
		]);

		expect(operationIds).toHaveLength(2);
		expect(operationIds[1]).not.toBe(operationIds[0]);
	});

	it("rejects a short append response instead of advancing a partial baseline", async () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		mockInvoke({
			agent_thread_append_messages: () => [
				{ id: "u1", threadId: "short", seq: 0, role: "user" },
			],
		});
		await expect(appendThreadMessages("short", [], current)).rejects.toThrow(
			"returned 1 rows for 2 messages",
		);
	});

	it("blocks different content while an append response is unresolved", async () => {
		const original = [userMessage("u1", "hi")];
		let attempts = 0;
		const calls = mockInvoke({
			agent_thread_append_messages: () => {
				attempts += 1;
				if (attempts === 1) throw new Error("IPC response lost");
				return [{ id: "u1", threadId: "pending", seq: 0, role: "user" }];
			},
		});
		await expect(appendThreadMessages("pending", [], original)).rejects.toThrow(
			"IPC response lost",
		);
		await expect(
			appendThreadMessages("pending", [], [userMessage("u2", "different")]),
		).rejects.toThrow("unresolved");
		expect(calls).toHaveLength(1);

		// Exact retry is the only safe way to resolve and retire the operation.
		await appendThreadMessages("pending", [], original);
	});
});

// ---------------------------------------------------------------------------
// resolveThread — newest-wins, create-if-missing
// ---------------------------------------------------------------------------

describe("exact thread scopes", () => {
	it("normalizes omitted nullable fields instead of treating them as wildcards", () => {
		expect(
			normalizeThreadScope("pattern_graph", "pattern", "pattern-1", {
				principalId: null,
				implementationId: "implementation-1",
				title: "Display only",
			}),
		).toEqual({
			principalId: null,
			agentKind: "pattern_graph",
			subjectKind: "pattern",
			subjectId: "pattern-1",
			implementationId: "implementation-1",
			venueId: null,
			scoreId: null,
		});
	});

	it.each([
		["owner", { ownerUserId: "other-user" }],
		["agent", { agentKind: "pattern_graph" }],
		["subject kind", { subjectKind: "pattern" }],
		["subject id", { subjectId: "track-2" }],
		["implementation", { implementationId: "implementation-2" }],
		["venue", { venueId: "v-2" }],
		["score", { scoreId: "s-2" }],
	] as const)("rejects a thread with a different %s", (_label, mismatch) => {
		const matching = scopedThread(
			"matching",
			"2026-08-01T00:00:00Z",
			"v-1",
			"s-1",
		);
		expect(threadMatchesScope({ ...matching, ...mismatch }, TRACK_SCOPE)).toBe(
			false,
		);
	});

	it("matches every field of an exact scope", () => {
		expect(
			threadMatchesScope(
				scopedThread("matching", "2026-08-01T00:00:00Z", "v-1", "s-1"),
				TRACK_SCOPE,
			),
		).toBe(true);
	});

	it("lists only exact-scope rows while preserving backend order", async () => {
		const matching = scopedThread(
			"matching",
			"2026-08-01T00:00:00Z",
			"v-1",
			"s-1",
		);
		const older = scopedThread("older", "2026-07-01T00:00:00Z", "v-1", "s-1");
		const calls = mockInvoke({
			agent_thread_list: () => [
				matching,
				{ ...matching, id: "wrong-owner", ownerUserId: "user-2" },
				{ ...matching, id: "wrong-agent", agentKind: "pattern_graph" },
				{ ...matching, id: "wrong-subject", subjectId: "track-2" },
				{ ...matching, id: "wrong-venue", venueId: "v-2" },
				{ ...matching, id: "wrong-score", scoreId: "s-2" },
				older,
			],
		});

		const listed = await listScopedThreads(TRACK_SCOPE);
		expect(listed.map((candidate) => candidate.id)).toEqual([
			"matching",
			"older",
		]);
		expect(calls[0]).toEqual({
			command: "agent_thread_list",
			args: {
				agentKind: "track_copilot",
				subjectKind: "track",
				subjectId: "track-1",
			},
		});
	});

	it("keeps signed-out rows separate from signed-in rows", async () => {
		const signedOutScope = normalizeThreadScope(
			"track_copilot",
			"track",
			"track-1",
			{ principalId: null },
		);
		const row = thread("local", "2026-08-01T00:00:00Z");
		mockInvoke({
			agent_thread_list: () => [
				{ ...row, ownerUserId: null },
				{ ...row, id: "signed-in" },
			],
		});

		expect(
			(await listScopedThreads(signedOutScope)).map(
				(candidate) => candidate.id,
			),
		).toEqual(["local"]);
	});

	it("creates from exact scope without sending the frontend principal", async () => {
		const created = scopedThread(
			"created",
			"2026-08-01T00:00:00Z",
			"v-1",
			"s-1",
		);
		const calls = mockInvoke({ agent_thread_create: () => created });

		await expect(
			createScopedThread(
				TRACK_SCOPE,
				"Main",
				"40000000-0000-4000-8000-000000000001",
			),
		).resolves.toBe(created);
		expect(calls[0]).toEqual({
			command: "agent_thread_create",
			args: {
				input: {
					requestId: "40000000-0000-4000-8000-000000000001",
					agentKind: "track_copilot",
					subjectKind: "track",
					subjectId: "track-1",
					implementationId: null,
					venueId: "v-1",
					scoreId: "s-1",
					title: "Main",
				},
			},
		});
	});

	it("reuses one request id when thread creation loses its first response", async () => {
		const created = scopedThread(
			"created",
			"2026-08-01T00:00:00Z",
			"v-1",
			"s-1",
		);
		let attempts = 0;
		const calls = mockInvoke({
			agent_thread_create: () => {
				attempts += 1;
				if (attempts === 1) throw new Error("response lost");
				return created;
			},
		});

		await expect(
			createScopedThread(
				TRACK_SCOPE,
				null,
				"40000000-0000-4000-8000-000000000002",
			),
		).resolves.toBe(created);
		expect(calls).toHaveLength(2);
		expect(calls[0]?.args).toEqual(calls[1]?.args);
	});

	it("rejects a created row that does not match the requested scope", async () => {
		mockInvoke({
			agent_thread_create: () => ({
				...scopedThread("wrong-owner", "2026-08-01T00:00:00Z", "v-1", "s-1"),
				ownerUserId: "user-2",
			}),
		});

		await expect(createScopedThread(TRACK_SCOPE)).rejects.toThrow(
			"does not match the requested scope",
		);
	});
});

describe("newestThread", () => {
	it("returns null for an empty list", () => {
		expect(newestThread([])).toBeNull();
	});

	it("picks the most recently updated thread regardless of list order", () => {
		const picked = newestThread([
			thread("old", "2026-01-01T00:00:00Z"),
			thread("new", "2026-07-01T00:00:00Z"),
			thread("mid", "2026-03-01T00:00:00Z"),
		]);
		expect(picked?.id).toBe("new");
	});

	it("breaks updatedAt ties on createdAt", () => {
		const picked = newestThread([
			thread("a", "2026-07-01T00:00:00Z", "2026-01-01T00:00:00Z"),
			thread("b", "2026-07-01T00:00:00Z", "2026-02-01T00:00:00Z"),
		]);
		expect(picked?.id).toBe("b");
	});
});

describe("resolveThread", () => {
	it("reuses the newest existing thread for the subject", async () => {
		const calls = mockInvoke({
			agent_thread_list: () => [
				thread("old", "2026-01-01T00:00:00Z"),
				thread("new", "2026-07-01T00:00:00Z"),
			],
		});
		const resolved = await resolveThread("track_copilot", "track", "track-1", {
			principalId: "user-1",
		});
		expect(resolved.id).toBe("new");
		expect(calls.map((c) => c.command)).toEqual(["agent_thread_list"]);
		expect(calls[0].args).toEqual({
			agentKind: "track_copilot",
			subjectKind: "track",
			subjectId: "track-1",
		});
	});

	it("reuses only a thread pinned to the requested venue and track state", async () => {
		const calls = mockInvoke({
			agent_thread_list: () => [
				scopedThread("wrong-score", "2026-08-01T00:00:00Z", "v-1", "s-2"),
				scopedThread("wrong-venue", "2026-07-01T00:00:00Z", "v-2", "s-1"),
				scopedThread("matching", "2026-06-01T00:00:00Z", "v-1", "s-1"),
			],
		});
		const resolved = await resolveThread("track_copilot", "track", "track-1", {
			principalId: "user-1",
			venueId: "v-1",
			scoreId: "s-1",
		});
		expect(resolved.id).toBe("matching");
		expect(calls.map((call) => call.command)).toEqual(["agent_thread_list"]);
	});

	it("creates a new thread when only differently scoped threads exist", async () => {
		const fresh = scopedThread("fresh", "2026-08-01T00:00:00Z", "v-1", "s-1");
		const calls = mockInvoke({
			agent_thread_list: () => [
				scopedThread("other", "2026-07-01T00:00:00Z", "v-1", "s-2"),
			],
			agent_thread_create: () => fresh,
		});
		const resolved = await resolveThread("track_copilot", "track", "track-1", {
			principalId: "user-1",
			venueId: "v-1",
			scoreId: "s-1",
			title: "Main",
		});
		expect(resolved.id).toBe("fresh");
		expect(calls[1].args.input).toEqual({
			requestId: expect.any(String),
			agentKind: "track_copilot",
			subjectKind: "track",
			subjectId: "track-1",
			implementationId: null,
			venueId: "v-1",
			scoreId: "s-1",
			title: "Main",
		});
	});

	it("creates a thread when the subject has none, stamping the init metadata", async () => {
		const fresh = {
			...thread("fresh", "2026-07-01T00:00:00Z"),
			agentKind: "pattern_graph",
			subjectKind: "pattern",
			subjectId: "p-1",
			implementationId: "implementation-1",
			venueId: "v-1",
		} satisfies AgentThread;
		const calls = mockInvoke({
			agent_thread_list: () => [],
			agent_thread_create: () => fresh,
		});
		const resolved = await resolveThread("pattern_graph", "pattern", "p-1", {
			principalId: "user-1",
			implementationId: "implementation-1",
			venueId: "v-1",
			title: "Wash",
		});
		expect(resolved.id).toBe("fresh");
		expect(calls.map((c) => c.command)).toEqual([
			"agent_thread_list",
			"agent_thread_create",
		]);
		expect(calls[1].args.input).toEqual({
			requestId: expect.any(String),
			agentKind: "pattern_graph",
			subjectKind: "pattern",
			subjectId: "p-1",
			implementationId: "implementation-1",
			venueId: "v-1",
			scoreId: null,
			title: "Wash",
		});
	});
});

// ---------------------------------------------------------------------------
// loadThreadMessages — fail-closed validation
// ---------------------------------------------------------------------------

describe("loadThreadMessages", () => {
	it("returns validated messages and a baseline aligned to their seqs", async () => {
		mockInvoke({
			agent_thread_get: () => ({
				thread: thread("t1", "2026-07-01T00:00:00Z"),
				messages: [
					{
						id: "u1",
						seq: 1,
						role: "user",
						parts: [{ type: "text", text: "hi" }],
					},
					{
						id: "a1",
						seq: 2,
						role: "assistant",
						parts: [{ type: "text", text: "hello" }],
					},
				],
			}),
		});
		const loaded = await loadThreadMessages("t1");
		expect(loaded.messages.map((m) => m.id)).toEqual(["u1", "a1"]);
		expect(loaded.baseline.map((m) => m.seq)).toEqual([1, 2]);
		// A freshly loaded thread must not immediately append itself again.
		expect(planThreadAppend(loaded.baseline, loaded.messages)).toEqual({
			append: [],
		});
	});

	it("loads a brand-new thread without warning", async () => {
		mockInvoke({
			agent_thread_get: () => ({
				thread: thread("t1", "2026-07-01T00:00:00Z"),
				messages: [],
			}),
		});
		const loaded = await loadThreadMessages("t1");
		expect(loaded).toEqual({ messages: [], baseline: [] });
		// An empty thread is new, not corrupt — validating it would fail
		// because the SDK rejects an empty messages array.
	});

	it("fails closed without modifying a transcript with an invalid tail", async () => {
		const calls = mockInvoke({
			agent_thread_get: () => ({
				thread: thread("t1", "2026-07-01T00:00:00Z"),
				messages: [
					{
						id: "u1",
						seq: 1,
						role: "user",
						parts: [{ type: "text", text: "hi" }],
					},
					{ id: "x1", seq: 2, role: "assistant", parts: [{ type: "nope" }] },
				],
			}),
		});
		await expect(loadThreadMessages("t1")).rejects.toThrow(
			"durable transcript was left unchanged",
		);
		expect(calls.map((call) => call.command)).toEqual(["agent_thread_get"]);
	});

	it("fails closed when no persisted messages validate", async () => {
		mockInvoke({
			agent_thread_get: () => ({
				thread: thread("t1", "2026-07-01T00:00:00Z"),
				messages: [
					{ id: "x1", seq: 1, role: "assistant", parts: [{ type: "nope" }] },
					{ id: "x2", seq: 2, role: "assistant", parts: [{ type: "nope" }] },
				],
			}),
		});
		await expect(loadThreadMessages("t1")).rejects.toThrow(
			"durable transcript was left unchanged",
		);
	});
});
