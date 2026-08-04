import type { ReactNode } from "react";
import {
	type AgentChatMessage,
	isReasoningPart,
	isTextPart,
	isToolPart,
	toolName,
} from "@/shared/lib/agent/messages";

/** Normalized, render-friendly view of a tool invocation. */
export type ToolView = {
	name: string;
	callId: string;
	state: "pending" | "running" | "done" | "error";
	input: unknown;
	output: unknown;
	error?: string;
};

export type ToolLabel = { verb: string; detail: string | null };

export type ToolVerb = {
	/** Present participle shown while the call is live. */
	running: string;
	/** Past-tense verb shown once the call finishes. */
	past: string;
	/** Singular noun used when counting calls in an activity summary. */
	noun: string;
	/** Optional natural object form: "Asked venue" instead of "Asked 2 questions". */
	object?: string;
};

/** Per-feature display vocabulary so the shared renderer can label tool runs
 * without knowing any specific tool set. */
export type ToolVocab = {
	/** Status-aware verb + count noun per tool name. */
	verbs: Record<string, ToolVerb>;
	/** Rich single-tool label for the expanded tool line. */
	formatLabel: (tool: ToolView) => ToolLabel;
	/** Optional tool-specific detail bodies. Unknown tools use the compact
	 * generic input/result renderer. */
	renderers?: Record<string, (tool: ToolView) => ReactNode>;
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
		case "output-denied":
			return "error";
		default:
			return "running";
	}
}

/** Flatten an assistant transcript message's parts into render parts, dropping
 * step-start / source / file parts the chat UI doesn't show. */
export function toRenderParts(message: AgentChatMessage): RenderPart[] {
	const out: RenderPart[] = [];
	let i = 0;
	for (const part of message.parts) {
		i += 1;
		if (isTextPart(part)) {
			out.push({ kind: "text", id: `t-${i}`, text: part.text });
		} else if (isReasoningPart(part)) {
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
		} else if (isToolPart(part)) {
			const name = toolName(part);
			const anyPart = part as {
				toolCallId: string;
				state: string;
				input?: unknown;
				output?: unknown;
				errorText?: string;
			};
			// Derive completion from output/error presence as well as state so a
			// no-argument tool cannot remain visually pending after its result lands.
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
