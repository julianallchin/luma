import type { UIMessage } from "ai";
import { describe, expect, it } from "vitest";
import {
	collectSubagentEntries,
	lastSubagentText,
	mergeSubagentStates,
	type SubagentState,
	subagentAction,
	subagentStatesFromMessages,
} from "./subagent-state";

function assistant(id: string, parts: unknown[]): UIMessage {
	return { id, role: "assistant", parts } as UIMessage;
}

function agentCall(
	callId: string,
	description: string,
): Record<string, unknown> {
	return {
		type: "dynamic-tool",
		toolName: "Agent",
		toolCallId: callId,
		state: "input-available",
		input: { subagent_type: "general-purpose", description },
	};
}

function state(
	id: string,
	parentToolCallId: string,
	messages: UIMessage[] = [],
): SubagentState {
	return {
		id,
		type: "general-purpose",
		parentToolCallId,
		messages,
		status: "running",
		startedAt: 100,
	};
}

describe("subagent UI state", () => {
	it("collects nested children in visual spawn order", () => {
		const grandchild = state("grandchild", "call-grandchild");
		const child = state("child", "call-child", [
			assistant("child-message", [
				agentCall("call-grandchild", "Inspect nested state"),
			]),
		]);
		const sibling = state("sibling", "call-sibling");
		const root = [
			assistant("root-message", [
				agentCall("call-child", "Implement editor"),
				agentCall("call-sibling", "Review changes"),
			]),
		];

		const entries = collectSubagentEntries(root, [sibling, grandchild, child]);

		expect(entries.map((entry) => entry.id)).toEqual([
			"child",
			"grandchild",
			"sibling",
		]);
		expect(entries.map((entry) => entry.name)).toEqual([
			"Implement editor",
			"Inspect nested state",
			"Review changes",
		]);
	});

	it("omits unattached states instead of guessing their parent", () => {
		const root = [assistant("root", [agentCall("known", "Known child")])];
		const entries = collectSubagentEntries(root, [state("orphan", "missing")]);
		expect(entries).toEqual([]);
	});

	it("reads latest prose and terminal action labels", () => {
		const subagent = state("child", "call", [
			assistant("answer", [{ type: "text", text: "Done with\nall edits." }]),
		]);
		expect(lastSubagentText(subagent)).toBe("Done with all edits.");
		expect(subagentAction({ ...subagent, status: "completed" })).toBe(
			"finished working",
		);
		expect(subagentAction({ ...subagent, status: "error" })).toBe("failed");
		expect(subagentAction({ ...subagent, status: "aborted" })).toBe("stopped");
	});

	it("keeps the latest persisted data part for each child", () => {
		const running = state("child", "call");
		const completed = { ...running, status: "completed" as const };
		const messages = [
			assistant("one", [{ type: "data-subagent", data: running }]),
			assistant("two", [{ type: "data-subagent", data: completed }]),
		];

		expect(subagentStatesFromMessages(messages)).toEqual([completed]);
	});

	it("marks unmatched replayed running children as interrupted", () => {
		const replayed = state("child", "call");
		const messages = [
			assistant("one", [{ type: "data-subagent", data: replayed }]),
		];

		expect(mergeSubagentStates(messages, [replayed])).toEqual([replayed]);
		expect(mergeSubagentStates(messages, [])).toEqual([
			{
				...replayed,
				status: "aborted",
				error: "Subagent session ended.",
			},
		]);
	});

	it("recovers terminal children and grandchildren from tool outputs", () => {
		const grandchild = {
			...state("grandchild", "grandchild-call"),
			status: "completed" as const,
		};
		const child = {
			...state("child", "child-call", [
				assistant("nested", [
					{
						...agentCall("grandchild-call", "Nested work"),
						state: "output-available",
						output: { subagent: grandchild },
					},
				]),
			]),
			status: "completed" as const,
		};
		const messages = [
			assistant("root", [
				{
					...agentCall("child-call", "Root work"),
					state: "output-available",
					output: { subagent: child },
				},
			]),
		];

		expect(subagentStatesFromMessages(messages)).toEqual([child, grandchild]);
	});
});
