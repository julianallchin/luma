import type { UIMessage } from "ai";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentThread } from "@/bindings/schema";
import {
	loadThreadMessages,
	newestThread,
	type PersistedMessage,
	planThreadSync,
	resolveThread,
	syncThreadMessages,
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

function userMessage(id: string, text: string): UIMessage {
	return { id, role: "user", parts: [{ type: "text", text }] };
}

function assistantMessage(id: string, text: string): UIMessage {
	return { id, role: "assistant", parts: [{ type: "text", text }] };
}

function baselineOf(messages: UIMessage[], startSeq = 1): PersistedMessage[] {
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
		agentKind: "track_copilot",
		subjectKind: "track",
		subjectId: "track-1",
		venueId: null,
		scoreId: null,
		title: null,
		createdAt,
		updatedAt,
	} satisfies AgentThread;
}

afterEach(() => {
	resetInvoke();
	vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// planThreadSync — the persistence differ
// ---------------------------------------------------------------------------

describe("planThreadSync", () => {
	it("appends everything when nothing is persisted yet", () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		expect(planThreadSync([], current)).toEqual({
			truncateFromSeq: null,
			append: current,
		});
	});

	it("is a no-op when the persisted history already matches", () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		expect(planThreadSync(baselineOf(current), current)).toEqual({
			truncateFromSeq: null,
			append: [],
		});
	});

	it("appends only the new tail of a finished turn", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
		];
		const current = [...persisted, userMessage("u2", "again")];
		expect(planThreadSync(baselineOf(persisted), current)).toEqual({
			truncateFromSeq: null,
			append: [current[2]],
		});
	});

	it("truncates and re-appends when a persisted message's parts changed", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "draft"),
		];
		const current = [persisted[0], assistantMessage("a1", "final")];
		expect(planThreadSync(baselineOf(persisted), current)).toEqual({
			truncateFromSeq: 2,
			append: [current[1]],
		});
	});

	it("truncates from the first diverging id when history was edited", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
			userMessage("u2", "typo"),
		];
		const current = [persisted[0], persisted[1], userMessage("u3", "fixed")];
		expect(planThreadSync(baselineOf(persisted), current)).toEqual({
			truncateFromSeq: 3,
			append: [current[2]],
		});
	});

	it("truncates the tail when messages were removed", () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
		];
		expect(planThreadSync(baselineOf(persisted), [persisted[0]])).toEqual({
			truncateFromSeq: 2,
			append: [],
		});
	});

	it("notices a role change on the same id", () => {
		const persisted = [userMessage("u1", "hi")];
		const current = [assistantMessage("u1", "hi")];
		expect(planThreadSync(baselineOf(persisted), current).truncateFromSeq).toBe(
			1,
		);
	});
});

// ---------------------------------------------------------------------------
// syncThreadMessages — the differ wired to the commands
// ---------------------------------------------------------------------------

describe("syncThreadMessages", () => {
	it("issues no commands when nothing changed", async () => {
		const current = [userMessage("u1", "hi")];
		const calls = mockInvoke({});
		const next = await syncThreadMessages("t1", baselineOf(current), current);
		expect(calls).toEqual([]);
		expect(next).toEqual(baselineOf(current));
	});

	it("appends without truncating in the common case", async () => {
		const persisted = [userMessage("u1", "hi")];
		const current = [...persisted, assistantMessage("a1", "hello")];
		const calls = mockInvoke({
			agent_thread_append_messages: () => [{ seq: 2 }],
		});
		const next = await syncThreadMessages("t1", baselineOf(persisted), current);
		expect(calls.map((c) => c.command)).toEqual([
			"agent_thread_append_messages",
		]);
		expect(calls[0].args.messages).toEqual([
			{ id: "a1", role: "assistant", parts: current[1].parts },
		]);
		expect(next.map((m) => m.seq)).toEqual([1, 2]);
	});

	it("truncates before re-appending an edited tail", async () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "draft"),
		];
		const current = [persisted[0], assistantMessage("a1", "final")];
		const calls = mockInvoke({
			agent_thread_truncate_from: () => 1,
			agent_thread_append_messages: () => [{ seq: 5 }],
		});
		const next = await syncThreadMessages("t1", baselineOf(persisted), current);
		expect(calls.map((c) => c.command)).toEqual([
			"agent_thread_truncate_from",
			"agent_thread_append_messages",
		]);
		expect(calls[0].args).toEqual({ threadId: "t1", seq: 2 });
		expect(next.map((m) => m.seq)).toEqual([1, 5]);
	});

	it("truncates with no append when messages were removed", async () => {
		const persisted = [
			userMessage("u1", "hi"),
			assistantMessage("a1", "hello"),
		];
		const calls = mockInvoke({ agent_thread_truncate_from: () => 1 });
		const next = await syncThreadMessages("t1", baselineOf(persisted), [
			persisted[0],
		]);
		expect(calls.map((c) => c.command)).toEqual(["agent_thread_truncate_from"]);
		expect(next).toEqual(baselineOf([persisted[0]]));
	});

	it("keeps a baseline that makes the next sync a no-op", async () => {
		const current = [userMessage("u1", "hi"), assistantMessage("a1", "hello")];
		mockInvoke({
			// The backend echoes parts through serde_json, which may reorder keys;
			// the baseline must still match the in-memory messages next time.
			agent_thread_append_messages: () => [{ seq: 1 }, { seq: 2 }],
		});
		const baseline = await syncThreadMessages("t1", [], current);
		expect(planThreadSync(baseline, current)).toEqual({
			truncateFromSeq: null,
			append: [],
		});
	});
});

// ---------------------------------------------------------------------------
// resolveThread — newest-wins, create-if-missing
// ---------------------------------------------------------------------------

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
		const resolved = await resolveThread("track_copilot", "track", "track-1");
		expect(resolved.id).toBe("new");
		expect(calls.map((c) => c.command)).toEqual(["agent_thread_list"]);
		expect(calls[0].args).toEqual({
			agentKind: "track_copilot",
			subjectKind: "track",
			subjectId: "track-1",
		});
	});

	it("creates a thread when the subject has none, stamping the init metadata", async () => {
		const calls = mockInvoke({
			agent_thread_list: () => [],
			agent_thread_create: () => thread("fresh", "2026-07-01T00:00:00Z"),
		});
		const resolved = await resolveThread("pattern_graph", "pattern", "p-1", {
			venueId: "v-1",
			title: "Wash",
		});
		expect(resolved.id).toBe("fresh");
		expect(calls.map((c) => c.command)).toEqual([
			"agent_thread_list",
			"agent_thread_create",
		]);
		expect(calls[1].args.input).toEqual({
			agentKind: "pattern_graph",
			subjectKind: "pattern",
			subjectId: "p-1",
			venueId: "v-1",
			scoreId: null,
			title: "Wash",
		});
	});
});

// ---------------------------------------------------------------------------
// loadThreadMessages — validation fallback
// ---------------------------------------------------------------------------

describe("loadThreadMessages", () => {
	beforeEach(() => {
		vi.spyOn(console, "warn").mockImplementation(() => {});
	});

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
		// A freshly loaded thread must not immediately rewrite itself.
		expect(planThreadSync(loaded.baseline, loaded.messages)).toEqual({
			truncateFromSeq: null,
			append: [],
		});
	});

	it("falls back to the longest valid prefix when a message is corrupt", async () => {
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
					{ id: "x1", seq: 2, role: "assistant", parts: [{ type: "nope" }] },
				],
			}),
		});
		const loaded = await loadThreadMessages("t1");
		expect(loaded.messages.map((m) => m.id)).toEqual(["u1"]);
		expect(loaded.baseline.map((m) => m.seq)).toEqual([1]);
		expect(console.warn).toHaveBeenCalled();
	});

	it("starts empty rather than throwing when nothing validates", async () => {
		mockInvoke({
			agent_thread_get: () => ({
				thread: thread("t1", "2026-07-01T00:00:00Z"),
				messages: [
					{ id: "x1", seq: 1, role: "assistant", parts: [{ type: "nope" }] },
					{ id: "x2", seq: 2, role: "assistant", parts: [{ type: "nope" }] },
				],
			}),
		});
		const loaded = await loadThreadMessages("t1");
		expect(loaded.messages).toEqual([]);
		expect(loaded.baseline).toEqual([]);
	});
});
