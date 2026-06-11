import {
	getToolName,
	isReasoningUIPart,
	isTextUIPart,
	isToolUIPart,
	type UIMessage,
} from "ai";

/** Normalized, render-friendly view of a tool invocation, derived from the
 * SDK's ToolUIPart / DynamicToolUIPart (whose states and field names vary). */
export type ToolView = {
	name: string;
	callId: string;
	state: "pending" | "running" | "done" | "error";
	input: unknown;
	output: unknown;
	error?: string;
};

export type ToolLabel = { verb: string; detail: string | null };

/** Per-feature display vocabulary so the shared renderer can label tool runs
 * without knowing any specific tool set. */
export type ToolVocab = {
	/** Past-tense verb + object noun per tool name, for run summaries
	 * ("Placed 2 clips"). noun=null when the verb already implies its object. */
	verbs: Record<string, { past: string; noun: string | null }>;
	/** Rich single-tool label for the expanded tool line. */
	formatLabel: (tool: ToolView) => ToolLabel;
};

/** A flattened assistant part for rendering. Reasoning parts may carry optional
 * timing (stamped by the streaming store) so we can show "Thought for Ns". */
export type RenderPart =
	| { kind: "text"; id: string; text: string }
	| {
			kind: "reasoning";
			id: string;
			text: string;
			startedAt?: number;
			lastDeltaAt?: number;
	  }
	| { kind: "tool"; id: string; tool: ToolView };

function normalizeToolState(state: string): ToolView["state"] {
	switch (state) {
		case "input-streaming":
			return "pending";
		case "input-available":
		case "approval-requested":
		case "approval-responded":
			return "running";
		case "output-available":
			return "done";
		case "output-error":
			return "error";
		default:
			return "running";
	}
}

/** Flatten an assistant UIMessage's parts into render parts, dropping
 * step-start / source / file parts the chat UI doesn't show. */
export function toRenderParts(message: UIMessage): RenderPart[] {
	const out: RenderPart[] = [];
	let i = 0;
	for (const part of message.parts) {
		i += 1;
		if (isTextUIPart(part)) {
			out.push({ kind: "text", id: `t-${i}`, text: part.text });
		} else if (isReasoningUIPart(part)) {
			const p = part as typeof part & {
				startedAt?: number;
				lastDeltaAt?: number;
			};
			out.push({
				kind: "reasoning",
				id: `r-${i}`,
				text: part.text,
				startedAt: p.startedAt,
				lastDeltaAt: p.lastDeltaAt,
			});
		} else if (isToolUIPart(part)) {
			const name = getToolName(part);
			const anyPart = part as {
				toolCallId: string;
				state: string;
				input?: unknown;
				output?: unknown;
				errorText?: string;
			};
			// Derive completion from the presence of output/error, not just the
			// state string — the SDK's tool-part state can lag (a no-arg tool can
			// linger in input-streaming) even after the result has landed.
			const state =
				anyPart.errorText !== undefined
					? "error"
					: anyPart.output !== undefined
						? "done"
						: normalizeToolState(anyPart.state);
			out.push({
				kind: "tool",
				id: anyPart.toolCallId,
				tool: {
					name,
					callId: anyPart.toolCallId,
					state,
					input: anyPart.input,
					output: anyPart.output,
					error: anyPart.errorText,
				},
			});
		}
	}
	return out;
}
