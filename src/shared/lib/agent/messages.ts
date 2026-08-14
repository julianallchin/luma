import type { AgentEvent, AgentMessage } from "@earendil-works/pi-agent-core";
import type {
	Api,
	AssistantMessage,
	Model,
	ToolResultMessage,
} from "@earendil-works/pi-ai";
import { type ToolSet, toolResultContent } from "./agent-tool";

export type TextPart = { type: "text"; text: string };
export type ReasoningPart = {
	type: "reasoning";
	text: string;
	startedAt?: number;
	lastDeltaAt?: number;
};
export type ToolPart = {
	type: `tool-${string}` | "dynamic-tool";
	toolName?: string;
	toolCallId: string;
	state:
		| "input-streaming"
		| "input-available"
		| "approval-requested"
		| "approval-responded"
		| "output-available"
		| "output-error"
		| "output-denied";
	input?: unknown;
	output?: unknown;
	errorText?: string;
};
export type SubagentDataPart = {
	type: "data-subagent";
	data: import("./subagents/types").SubagentSnapshot;
};
export type PiMetadataPart = {
	type: "data-pi-message";
	data: Omit<AssistantMessage, "role" | "content">;
};
export type StepStartPart = { type: "step-start" };
export type AgentChatPart =
	| TextPart
	| ReasoningPart
	| ToolPart
	| SubagentDataPart
	| PiMetadataPart
	| StepStartPart;

export type AgentChatMessage = {
	id: string;
	role: "user" | "assistant";
	parts: AgentChatPart[];
};

type MessageWithId = AgentMessage & { id?: string };

export function messageId(message: AgentMessage): string {
	const identified = message as MessageWithId;
	identified.id ??= crypto.randomUUID();
	return identified.id;
}

export function toolName(part: ToolPart): string {
	return part.type === "dynamic-tool"
		? (part.toolName ?? "tool")
		: part.type.slice("tool-".length);
}

export function isTextPart(part: AgentChatPart): part is TextPart {
	return part.type === "text";
}

export function isReasoningPart(part: AgentChatPart): part is ReasoningPart {
	return part.type === "reasoning";
}

export function isToolPart(part: AgentChatPart): part is ToolPart {
	return part.type === "dynamic-tool" || part.type.startsWith("tool-");
}

export function userChatMessage(
	text: string,
	id: string = crypto.randomUUID(),
): AgentChatMessage {
	return { id, role: "user", parts: [{ type: "text", text }] };
}

export function userAgentMessage(message: AgentChatMessage): AgentMessage {
	return Object.assign(
		{
			role: "user" as const,
			content: message.parts
				.filter(isTextPart)
				.map((part) => part.text)
				.join("\n"),
			timestamp: Date.now(),
		},
		{ id: message.id },
	);
}

function metadataPart(message: AssistantMessage): PiMetadataPart {
	const { role: _role, content: _content, ...data } = message;
	return { type: "data-pi-message", data };
}

function partsOf(
	message: AssistantMessage,
	existing?: AgentChatMessage,
): AgentChatPart[] {
	const priorTools = new Map(
		existing?.parts.filter(isToolPart).map((part) => [part.toolCallId, part]) ??
			[],
	);
	return [
		...message.content.map((block): AgentChatPart => {
			if (block.type === "text") return { type: "text", text: block.text };
			if (block.type === "thinking") {
				const prior = existing?.parts.find(isReasoningPart);
				return {
					type: "reasoning",
					text: block.thinking,
					startedAt: prior?.startedAt ?? Date.now(),
					lastDeltaAt: Date.now(),
				};
			}
			const prior = priorTools.get(block.id);
			return {
				type: `tool-${block.name}`,
				toolCallId: block.id,
				state: prior?.state ?? "input-streaming",
				input: block.arguments,
				...(prior?.output !== undefined ? { output: prior.output } : {}),
				...(prior?.errorText !== undefined
					? { errorText: prior.errorText }
					: {}),
			};
		}),
		metadataPart(message),
	];
}

function replaceAt(
	messages: AgentChatMessage[],
	index: number,
	message: AgentChatMessage,
): AgentChatMessage[] {
	return messages.map((candidate, candidateIndex) =>
		candidateIndex === index ? message : candidate,
	);
}

/** Index of the step boundary the assistant message is currently writing into. */
function currentStepStart(parts: AgentChatPart[]): number {
	for (let index = parts.length - 1; index >= 0; index -= 1) {
		if (parts[index]?.type === "step-start") return index;
	}
	return -1;
}

/** Rewrite only the newest step, leaving the turn's earlier steps untouched. */
function withCurrentStep(
	message: AgentChatMessage,
	update: AssistantMessage,
): AgentChatMessage {
	const boundary = currentStepStart(message.parts);
	const closed = message.parts.slice(0, boundary + 1);
	const step = { ...message, parts: message.parts.slice(boundary + 1) };
	return { ...message, parts: [...closed, ...partsOf(update, step)] };
}

/**
 * Start the round's step: a new one inside the turn's assistant message when a
 * response is already open, otherwise a new assistant message. Subagent
 * milestones are assistant-role bookkeeping and never a turn to continue.
 */
function openAssistantStep(
	messages: AgentChatMessage[],
	message: AssistantMessage,
): AgentChatMessage[] {
	const index = messages.length - 1;
	const open = messages[index];
	const step: AgentChatPart[] = [{ type: "step-start" }, ...partsOf(message)];
	if (
		open?.role === "assistant" &&
		!open.parts.every((part) => part.type === "data-subagent")
	) {
		return replaceAt(messages, index, {
			...open,
			parts: [...open.parts, ...step],
		});
	}
	return [
		...messages,
		{ id: messageId(message), role: "assistant", parts: step },
	];
}

function latestOpenAssistant(messages: AgentChatMessage[]): number {
	for (let index = messages.length - 1; index >= 0; index -= 1) {
		if (messages[index]?.role === "assistant") return index;
	}
	return -1;
}

function updateTool(
	messages: AgentChatMessage[],
	toolCallId: string,
	update: (part: ToolPart) => ToolPart,
): AgentChatMessage[] {
	for (let index = messages.length - 1; index >= 0; index -= 1) {
		const message = messages[index];
		if (!message || message.role !== "assistant") continue;
		const partIndex = message.parts.findIndex(
			(part) => isToolPart(part) && part.toolCallId === toolCallId,
		);
		if (partIndex < 0) continue;
		const part = message.parts[partIndex];
		if (!part || !isToolPart(part)) return messages;
		const nextMessage = {
			...message,
			parts: message.parts.map((candidate, index) =>
				index === partIndex ? update(part) : candidate,
			),
		};
		return messages.map((candidate, candidateIndex) =>
			candidateIndex === index ? nextMessage : candidate,
		);
	}
	return messages;
}

/** Pure-ish frontend fold over Pi's event vocabulary, matching Foam's UI model. */
export function applyAgentEvent(
	messages: AgentChatMessage[],
	event: AgentEvent,
): AgentChatMessage[] {
	switch (event.type) {
		case "message_start":
			if (event.message.role === "user") {
				const id = messageId(event.message);
				if (messages.some((message) => message.id === id)) return messages;
				const text =
					typeof event.message.content === "string"
						? event.message.content
						: event.message.content
								.filter((part) => part.type === "text")
								.map((part) => part.text)
								.join("");
				return [...messages, userChatMessage(text, id)];
			}
			if (event.message.role !== "assistant") return messages;
			// Pi emits one assistant message per model round, so a turn that calls
			// tools produces several. The transcript keeps them as a single
			// assistant message split by step boundaries: that is the shape
			// restoration splits back into Pi messages, and the shape the
			// authored turn reserves state for — one prepared turn per response.
			return openAssistantStep(messages, event.message);
		case "message_update": {
			const index = latestOpenAssistant(messages);
			if (index < 0 || event.message.role !== "assistant") return messages;
			const existing = messages[index];
			if (!existing) return messages;
			return replaceAt(
				messages,
				index,
				withCurrentStep(existing, event.message as AssistantMessage),
			);
		}
		case "message_end": {
			if (event.message.role !== "assistant") return messages;
			const index = latestOpenAssistant(messages);
			if (index < 0) return messages;
			const existing = messages[index];
			if (!existing) return messages;
			(event.message as MessageWithId).id = existing.id;
			return replaceAt(
				messages,
				index,
				withCurrentStep(existing, event.message as AssistantMessage),
			);
		}
		case "tool_execution_start":
			return updateTool(messages, event.toolCallId, (part) => ({
				...part,
				type: `tool-${event.toolName}`,
				state: "input-available",
				input: event.args,
			}));
		case "tool_execution_end":
			return updateTool(messages, event.toolCallId, (part) => {
				if (event.isError) {
					const errorText = event.result?.content
						?.filter((block: { type: string }) => block.type === "text")
						.map((block: { text?: string }) => block.text ?? "")
						.join("\n");
					return {
						...part,
						state: "output-error",
						errorText: errorText || "Tool execution failed.",
					};
				}
				return {
					...part,
					state: "output-available",
					output: event.result?.details,
				};
			});
		default:
			return messages;
	}
}

function restoredAssistant(
	message: AgentChatMessage,
	model: Model<Api>,
	parts: AgentChatPart[] = message.parts,
	id = message.id,
): AssistantMessage {
	const metadata = parts.find(
		(part): part is PiMetadataPart => part.type === "data-pi-message",
	)?.data;
	const content: AssistantMessage["content"] = [];
	for (const part of parts) {
		if (isTextPart(part)) {
			content.push({ type: "text", text: part.text });
			continue;
		}
		if (isReasoningPart(part)) {
			content.push({ type: "thinking", thinking: part.text });
			continue;
		}
		if (isToolPart(part)) {
			content.push({
				type: "toolCall",
				id: part.toolCallId,
				name: toolName(part),
				arguments: (part.input ?? {}) as Record<string, unknown>,
			});
		}
	}
	return Object.assign(
		{
			role: "assistant" as const,
			content,
			api: metadata?.api ?? model.api,
			provider: metadata?.provider ?? model.provider,
			model: metadata?.model ?? model.id,
			usage: metadata?.usage ?? {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			stopReason:
				metadata?.stopReason ??
				(content.some((part) => part.type === "toolCall") ? "toolUse" : "stop"),
			timestamp: metadata?.timestamp ?? Date.now(),
			...(metadata?.errorMessage
				? { errorMessage: metadata.errorMessage }
				: {}),
		},
		{ id },
	);
}

function assistantSteps(message: AgentChatMessage): AgentChatPart[][] {
	if (!message.parts.some((part) => part.type === "step-start")) {
		return [message.parts];
	}
	const steps: AgentChatPart[][] = [];
	let current: AgentChatPart[] = [];
	for (const part of message.parts) {
		if (part.type === "step-start") {
			if (current.length > 0) steps.push(current);
			current = [];
		} else {
			current.push(part);
		}
	}
	if (current.length > 0) steps.push(current);
	return steps;
}

async function restoredToolResult(
	part: ToolPart,
	tools: ToolSet,
): Promise<ToolResultMessage | null> {
	if (
		part.state !== "output-available" &&
		part.state !== "output-error" &&
		part.state !== "output-denied"
	) {
		return null;
	}
	const isError =
		part.state === "output-error" || part.state === "output-denied";
	const output = isError
		? (part.errorText ?? "Tool execution was denied.")
		: part.output;
	return {
		role: "toolResult",
		toolCallId: part.toolCallId,
		toolName: toolName(part),
		content: await toolResultContent(tools[toolName(part)], {
			toolCallId: part.toolCallId,
			input: part.input,
			output,
		}),
		details: output,
		isError,
		timestamp: Date.now(),
	};
}

export async function chatMessagesToAgentMessages(
	messages: AgentChatMessage[],
	model: Model<Api>,
	tools: ToolSet,
): Promise<AgentMessage[]> {
	const out: AgentMessage[] = [];
	for (const message of messages) {
		if (message.role === "user") {
			out.push(userAgentMessage(message));
			continue;
		}
		if (message.parts.every((part) => part.type === "data-subagent")) continue;
		for (const [stepIndex, parts] of assistantSteps(message).entries()) {
			if (
				!parts.some(
					(part) =>
						isTextPart(part) || isReasoningPart(part) || isToolPart(part),
				)
			) {
				continue;
			}
			out.push(
				restoredAssistant(
					message,
					model,
					parts,
					`${message.id}:step:${stepIndex}`,
				),
			);
			for (const part of parts) {
				if (!isToolPart(part)) continue;
				const result = await restoredToolResult(part, tools);
				if (result) out.push(result);
			}
		}
	}
	return out;
}

export function isAgentChatMessage(value: unknown): value is AgentChatMessage {
	if (!value || typeof value !== "object") return false;
	const message = value as Record<string, unknown>;
	return (
		typeof message.id === "string" &&
		(message.role === "user" || message.role === "assistant") &&
		Array.isArray(message.parts) &&
		message.parts.every(isAgentChatPart)
	);
}

function isAgentChatPart(value: unknown): value is AgentChatPart {
	if (!value || typeof value !== "object") return false;
	const part = value as Record<string, unknown>;
	if (part.type === "text" || part.type === "reasoning") {
		return typeof part.text === "string";
	}
	if (part.type === "step-start") return true;
	if (part.type === "data-subagent" || part.type === "data-pi-message") {
		return typeof part.data === "object" && part.data !== null;
	}
	if (
		part.type === "dynamic-tool" ||
		(typeof part.type === "string" && part.type.startsWith("tool-"))
	) {
		return (
			typeof part.toolCallId === "string" &&
			typeof part.state === "string" &&
			[
				"input-streaming",
				"input-available",
				"approval-requested",
				"approval-responded",
				"output-available",
				"output-error",
				"output-denied",
			].includes(part.state)
		);
	}
	return false;
}
