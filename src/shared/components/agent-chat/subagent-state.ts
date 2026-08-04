import {
	type AgentChatMessage,
	isToolPart,
	toolName,
} from "@/shared/lib/agent/messages";
import type {
	SubagentSnapshot,
	SubagentStatus,
} from "@/shared/lib/agent/subagents/types";

export type { SubagentStatus };

/** Live child state supplied by the agent runtime. Child messages use the same
 * durable transcript format as the parent, so the regular conversation renderer
 * can render drill-in feeds without a second display protocol. */
export type SubagentState = Omit<SubagentSnapshot, "lastActivityAt"> & {
	messages: AgentChatMessage[];
	lastActivityAt?: number;
};

export type SubagentEntry = {
	id: string;
	name?: string;
	type?: string;
	subagent: SubagentState;
};

export function isSubagentDone(subagent: SubagentState): boolean {
	return subagent.status !== "running";
}

function nonemptyString(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function descriptionOf(input: unknown): string | undefined {
	if (!input || typeof input !== "object") return undefined;
	return nonemptyString((input as Record<string, unknown>).description);
}

function toolParts(messages: AgentChatMessage[]) {
	return messages.flatMap((message) =>
		message.parts.flatMap((part) => {
			if (!isToolPart(part)) return [];
			const value = part as typeof part & {
				toolCallId: string;
				input?: unknown;
				output?: unknown;
				errorText?: string;
			};
			return [
				{
					name: toolName(part),
					callId: value.toolCallId,
					input: value.input,
					state:
						value.errorText !== undefined
							? "error"
							: value.output !== undefined
								? "done"
								: "running",
				},
			];
		}),
	);
}

/** Every child reachable from an Agent tool, recursively, in spawn order.
 * The runtime may keep child state in a flat map; UI nesting is determined by
 * parentToolCallId so grandchildren naturally appear immediately after their
 * parent. Orphan states are deliberately omitted instead of being guessed onto
 * the wrong Agent call. */
export function collectSubagentEntries(
	messages: AgentChatMessage[],
	subagents: readonly SubagentState[],
): SubagentEntry[] {
	const byParentCall = new Map<string, SubagentState>();
	for (const subagent of subagents) {
		if (subagent.parentToolCallId) {
			byParentCall.set(subagent.parentToolCallId, subagent);
		}
	}
	const visited = new Set<string>();
	const entries: SubagentEntry[] = [];

	const walk = (scope: AgentChatMessage[]) => {
		for (const tool of toolParts(scope)) {
			if (tool.name !== "Agent") continue;
			const subagent = byParentCall.get(tool.callId);
			if (!subagent || visited.has(subagent.id)) continue;
			visited.add(subagent.id);
			entries.push({
				id: subagent.id,
				name: descriptionOf(tool.input),
				type: subagent.type,
				subagent,
			});
			walk(subagent.messages);
		}
	};

	walk(messages);
	return entries;
}

/** Latest assistant prose, squashed to one line. Used for both a live child's
 * current commentary and a completed child's final-answer preview. */
export function lastSubagentText(subagent: SubagentState): string | undefined {
	for (let i = subagent.messages.length - 1; i >= 0; i -= 1) {
		const message = subagent.messages[i];
		if (!message || message.role !== "assistant") continue;
		for (let j = message.parts.length - 1; j >= 0; j -= 1) {
			const part = message.parts[j];
			if (part?.type !== "text" || !part.text.trim()) continue;
			return part.text.replace(/\s+/g, " ").trim();
		}
	}
	return undefined;
}

export function subagentAction(subagent: SubagentState): string {
	if (isSubagentDone(subagent)) {
		if (subagent.status === "error") return "failed";
		if (subagent.status === "aborted") return "stopped";
		return "finished working";
	}
	const tools = toolParts(subagent.messages);
	const last = tools.at(-1);
	if (!last) return "starting…";
	const object = descriptionOf(last.input);
	const action = last.state === "running" ? "using" : "used";
	const suffix = last.state === "running" ? "…" : "";
	return `${action} ${last.name}${object ? ` — ${object}` : ""}${suffix}`;
}

/** Custom data parts are the persistence-friendly fallback integration point:
 * a runtime can append/update `data-subagent` parts in the parent transcript,
 * or expose the same states separately on AgentSession. */
export function subagentStatesFromMessages(
	messages: AgentChatMessage[],
): SubagentState[] {
	const byId = new Map<string, SubagentState>();
	const visit = (scope: AgentChatMessage[], ancestors: ReadonlySet<string>) => {
		for (const message of scope) {
			for (const part of message.parts) {
				let candidate: unknown;
				if (part.type === "data-subagent") {
					candidate = (part as { data?: unknown }).data;
				} else if (isToolPart(part)) {
					const output = (part as { output?: unknown }).output;
					candidate =
						output && typeof output === "object"
							? (output as { subagent?: unknown }).subagent
							: undefined;
				}
				if (!isSubagentState(candidate)) continue;
				byId.set(candidate.id, candidate);
				if (ancestors.has(candidate.id)) continue;
				visit(candidate.messages, new Set([...ancestors, candidate.id]));
			}
		}
	};
	visit(messages, new Set());
	return [...byId.values()];
}

/** Merge durable replay state with this process's live manager. A persisted
 * running snapshot without a matching live child can only be an interrupted
 * prior session, so never resurrect it in the Active list. */
export function mergeSubagentStates(
	messages: AgentChatMessage[],
	liveSubagents: readonly SubagentState[],
): SubagentState[] {
	const liveIds = new Set(liveSubagents.map((subagent) => subagent.id));
	const merged = new Map<string, SubagentState>();
	for (const subagent of subagentStatesFromMessages(messages)) {
		merged.set(
			subagent.id,
			subagent.status === "running" && !liveIds.has(subagent.id)
				? {
						...subagent,
						status: "aborted",
						error: subagent.error ?? "Subagent session ended.",
					}
				: subagent,
		);
	}
	for (const subagent of liveSubagents) merged.set(subagent.id, subagent);
	return [...merged.values()];
}

function isSubagentState(value: unknown): value is SubagentState {
	if (!value || typeof value !== "object") return false;
	const state = value as Partial<SubagentState>;
	return (
		typeof state.id === "string" &&
		typeof state.type === "string" &&
		(state.parentToolCallId === undefined ||
			typeof state.parentToolCallId === "string") &&
		Array.isArray(state.messages) &&
		typeof state.startedAt === "number" &&
		(state.status === "running" ||
			state.status === "completed" ||
			state.status === "error" ||
			state.status === "aborted")
	);
}
