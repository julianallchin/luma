import { createOpenRouter } from "@openrouter/ai-sdk-provider";
import type { ModelMessage } from "ai";
import { stepCountIs, streamText } from "ai";
import { useCallback, useRef, useState } from "react";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import {
	type BarClassificationsPayload,
	buildSystemPrompt,
	formatBarTags,
	formatScore,
} from "./build-context";
import { getOpenRouterKey, OPENROUTER_MODEL } from "./openrouter-key";
import { buildAgentTools } from "./tools";

export type ToolPart = {
	id: string; // toolCallId
	name: string;
	input: unknown;
	output?: unknown;
	error?: string;
	state: "input-streaming" | "executing" | "done" | "error";
};

export type ChatTextPart = { kind: "text"; id: string; text: string };
export type ChatReasoningPart = {
	kind: "reasoning";
	id: string;
	text: string;
	startedAt: number;
	lastDeltaAt: number;
};
export type ChatToolPart = { kind: "tool"; tool: ToolPart };
export type ChatPart = ChatTextPart | ChatReasoningPart | ChatToolPart;

export type ChatMessage =
	| { id: string; role: "user"; text: string }
	| { id: string; role: "assistant"; parts: ChatPart[] };

type SendArgs = {
	prompt: string;
	venueName: string | null;
	barClassifications: BarClassificationsPayload | null;
	tagThresholds: Record<string, number>;
};

export function useChatAgent() {
	const [messages, setMessages] = useState<ChatMessage[]>([]);
	const [streaming, setStreaming] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const abortRef = useRef<AbortController | null>(null);

	const reset = useCallback(() => {
		setMessages([]);
		setError(null);
	}, []);

	const abort = useCallback(() => {
		abortRef.current?.abort();
	}, []);

	const send = useCallback(
		async ({
			prompt,
			venueName,
			barClassifications,
			tagThresholds,
		}: SendArgs) => {
			const apiKey = getOpenRouterKey();
			if (!apiKey) {
				setError("OpenRouter API key is not set.");
				return;
			}

			const text = prompt.trim();
			if (!text) return;

			setError(null);

			const userId = crypto.randomUUID();
			const assistantId = crypto.randomUUID();

			// Snapshot history *as it will be sent* (current messages + this user turn)
			const priorMessages = [...messages];
			const userMessage: ChatMessage = {
				id: userId,
				role: "user",
				text,
			};
			const assistantSeed: ChatMessage = {
				id: assistantId,
				role: "assistant",
				parts: [],
			};
			setMessages([...priorMessages, userMessage, assistantSeed]);

			const state = useTrackEditorStore.getState();
			const tools = buildAgentTools(useTrackEditorStore);

			const system = `${buildSystemPrompt({
				trackName: state.trackName,
				durationSeconds: state.durationSeconds,
				beatGrid: state.beatGrid,
				patternsCount: state.patterns.length,
				venueName,
				annotationsCount: state.annotations.length,
			})}

## Bar-by-bar tags
${formatBarTags(barClassifications, tagThresholds)}

## Current score (annotations)
${formatScore(state.annotations, state.beatGrid)}`;

			const modelMessages: ModelMessage[] = toModelMessages(priorMessages);
			modelMessages.push({ role: "user", content: text });

			const abortController = new AbortController();
			abortRef.current = abortController;
			setStreaming(true);

			try {
				const openrouter = createOpenRouter({
					apiKey,
					appName: "Luma",
					appUrl: "https://luma.show",
				});
				const result = streamText({
					model: openrouter(OPENROUTER_MODEL),
					system,
					messages: modelMessages,
					tools,
					stopWhen: stepCountIs(8),
					abortSignal: abortController.signal,
					providerOptions: {
						openrouter: {
							reasoning: { enabled: true, effort: "medium" },
						},
					},
				});

				for await (const part of result.fullStream) {
					if (abortController.signal.aborted) break;
					applyStreamPart(setMessages, assistantId, part);
				}
			} catch (err: unknown) {
				if (
					err instanceof Error &&
					(err.name === "AbortError" || abortController.signal.aborted)
				) {
					// fine
				} else {
					setError(err instanceof Error ? err.message : String(err));
				}
			} finally {
				setStreaming(false);
				abortRef.current = null;
				if (DEBUG_CHAT) {
					setMessages((prev) => {
						const m = prev.find((x) => x.id === assistantId);
						if (m && m.role === "assistant") {
							console.log("[chat] final assistant parts", {
								count: m.parts.length,
								kinds: m.parts.map((p) =>
									p.kind === "tool"
										? `tool:${p.tool.name}:${p.tool.state}`
										: p.kind === "text"
											? `text(${p.text.length})`
											: `r(${p.text.length})`,
								),
							});
						}
						return prev;
					});
				}
			}
		},
		[messages],
	);

	return { messages, streaming, error, send, abort, reset };
}

/** Convert our local UI messages into the AI SDK's ModelMessage format. */
function toModelMessages(uiMessages: ChatMessage[]): ModelMessage[] {
	const out: ModelMessage[] = [];
	for (const m of uiMessages) {
		if (m.role === "user") {
			out.push({ role: "user", content: m.text });
			continue;
		}
		// Assistant: emit a single text concatenation. We deliberately drop tool
		// trace history when round-tripping — the model has the latest score in
		// the system prompt and tool calls within a single send roundtrip are
		// handled by streamText's multi-step loop.
		const text = m.parts
			.filter((p): p is ChatTextPart => p.kind === "text")
			.map((p) => p.text)
			.join("");
		if (text.trim().length > 0) {
			out.push({ role: "assistant", content: text });
		}
	}
	return out;
}

type StreamPart = {
	type: string;
	id?: string;
	text?: string;
	toolName?: string;
	toolCallId?: string;
	input?: unknown;
	output?: unknown;
	error?: unknown;
	delta?: string;
};

const DEBUG_CHAT = true;

function applyStreamPart(
	setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
	assistantId: string,
	part: StreamPart,
) {
	if (
		DEBUG_CHAT &&
		(part.type === "tool-call" ||
			part.type === "tool-input-start" ||
			part.type === "tool-result" ||
			part.type === "tool-error")
	) {
		console.log("[chat]", part.type, {
			tool: part.toolName,
			id: part.toolCallId?.slice(-8),
		});
	}
	if (part.type === "text-delta") {
		const id = part.id ?? "default";
		const delta = part.text ?? "";
		setMessages((prev) =>
			updateAssistant(prev, assistantId, (parts) =>
				appendTextDelta(parts, id, delta),
			),
		);
		return;
	}

	if (part.type === "reasoning-delta") {
		const id = part.id ?? "default";
		const delta = part.text ?? "";
		setMessages((prev) =>
			updateAssistant(prev, assistantId, (parts) =>
				appendReasoningDelta(parts, id, delta),
			),
		);
		return;
	}

	if (part.type === "tool-input-start") {
		const id = part.toolCallId ?? part.id ?? "";
		const name = part.toolName ?? "tool";
		setMessages((prev) =>
			updateAssistant(prev, assistantId, (parts) =>
				upsertToolPart(parts, {
					id,
					name,
					input: undefined,
					state: "input-streaming",
				}),
			),
		);
		return;
	}

	if (part.type === "tool-call") {
		const id = part.toolCallId ?? "";
		const name = part.toolName ?? "tool";
		setMessages((prev) =>
			updateAssistant(prev, assistantId, (parts) =>
				upsertToolPart(parts, {
					id,
					name,
					input: part.input,
					state: "executing",
				}),
			),
		);
		return;
	}

	if (part.type === "tool-result") {
		const id = part.toolCallId ?? "";
		const name = part.toolName ?? "tool";
		setMessages((prev) =>
			updateAssistant(prev, assistantId, (parts) =>
				upsertToolPart(parts, {
					id,
					name,
					input: part.input,
					output: part.output,
					state: "done",
				}),
			),
		);
		return;
	}

	if (part.type === "tool-error") {
		const id = part.toolCallId ?? "";
		const name = part.toolName ?? "tool";
		setMessages((prev) =>
			updateAssistant(prev, assistantId, (parts) =>
				upsertToolPart(parts, {
					id,
					name,
					input: part.input,
					error:
						part.error instanceof Error
							? part.error.message
							: String(part.error ?? "tool error"),
					state: "error",
				}),
			),
		);
	}
}

function updateAssistant(
	messages: ChatMessage[],
	assistantId: string,
	mutate: (parts: ChatPart[]) => ChatPart[],
): ChatMessage[] {
	return messages.map((m) => {
		if (m.role !== "assistant" || m.id !== assistantId) return m;
		return { ...m, parts: mutate(m.parts) };
	});
}

function appendTextDelta(
	parts: ChatPart[],
	id: string,
	delta: string,
): ChatPart[] {
	const last = parts[parts.length - 1];
	if (last && last.kind === "text" && last.id === id) {
		return [...parts.slice(0, -1), { ...last, text: last.text + delta }];
	}
	return [...parts, { kind: "text", id, text: delta }];
}

function appendReasoningDelta(
	parts: ChatPart[],
	id: string,
	delta: string,
): ChatPart[] {
	const now = Date.now();
	const last = parts[parts.length - 1];
	if (last && last.kind === "reasoning" && last.id === id) {
		return [
			...parts.slice(0, -1),
			{ ...last, text: last.text + delta, lastDeltaAt: now },
		];
	}
	return [
		...parts,
		{ kind: "reasoning", id, text: delta, startedAt: now, lastDeltaAt: now },
	];
}

function upsertToolPart(
	parts: ChatPart[],
	tool: Pick<ToolPart, "id" | "name" | "input" | "output" | "error" | "state">,
): ChatPart[] {
	// Some providers reuse toolCallId across distinct calls (e.g. OpenRouter
	// emits "search_patterns:0" for every call to that tool). Match only an
	// open (not yet finalized) part with the same base id; otherwise append a
	// new part with a uniqued internal id so React keys stay stable.
	let idx = -1;
	for (let i = parts.length - 1; i >= 0; i--) {
		const p = parts[i];
		if (
			p.kind === "tool" &&
			stripIdSuffix(p.tool.id) === tool.id &&
			p.tool.state !== "done" &&
			p.tool.state !== "error"
		) {
			idx = i;
			break;
		}
	}
	if (idx === -1) {
		const dupCount = parts.filter(
			(p) => p.kind === "tool" && stripIdSuffix(p.tool.id) === tool.id,
		).length;
		const uniqueId = dupCount === 0 ? tool.id : `${tool.id}#${dupCount}`;
		return [...parts, { kind: "tool", tool: { ...tool, id: uniqueId } }];
	}
	const existing = parts[idx] as ChatToolPart;
	const merged: ChatToolPart = {
		kind: "tool",
		tool: {
			...existing.tool,
			...tool,
			id: existing.tool.id,
			input: tool.input ?? existing.tool.input,
			output: tool.output ?? existing.tool.output,
		},
	};
	const out = parts.slice();
	out[idx] = merged;
	return out;
}

function stripIdSuffix(id: string): string {
	const i = id.lastIndexOf("#");
	return i === -1 ? id : id.slice(0, i);
}
