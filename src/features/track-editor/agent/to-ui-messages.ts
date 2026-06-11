import type { UIMessage } from "ai";
import type { ChatMessage } from "./use-chat-agent";

/**
 * Adapt the track agent's hand-rolled `ChatMessage`s into native SDK
 * `UIMessage`s so it can render through the shared `AgentConversation` — the
 * same component the graph agent uses. Behavior is identical; only the message
 * plumbing differs (the track store predates the SDK streaming primitives and
 * still maintains its own reducer).
 *
 * Reasoning timing (startedAt/lastDeltaAt) is attached onto reasoning parts so
 * the shared renderer can still show "Thought for Ns".
 */
export function toUIMessages(messages: ChatMessage[]): UIMessage[] {
	return messages.map((m): UIMessage => {
		if (m.role === "user") {
			return {
				id: m.id,
				role: "user",
				parts: [{ type: "text", text: m.text }],
			};
		}
		return {
			id: m.id,
			role: "assistant",
			parts: m.parts.map((p) => {
				if (p.kind === "text") {
					return { type: "text", text: p.text };
				}
				if (p.kind === "reasoning") {
					return {
						type: "reasoning",
						text: p.text,
						startedAt: p.startedAt,
						lastDeltaAt: p.lastDeltaAt,
					} as UIMessage["parts"][number];
				}
				const t = p.tool;
				return {
					type: `tool-${t.name}`,
					toolCallId: t.id,
					state: toolState(t.state),
					input: t.input,
					output: t.output,
					errorText: t.error,
				} as UIMessage["parts"][number];
			}),
		};
	});
}

function toolState(
	state: "input-streaming" | "executing" | "done" | "error",
): "input-streaming" | "input-available" | "output-available" | "output-error" {
	switch (state) {
		case "input-streaming":
			return "input-streaming";
		case "executing":
			return "input-available";
		case "done":
			return "output-available";
		case "error":
			return "output-error";
	}
}
