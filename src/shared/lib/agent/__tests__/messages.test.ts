import type { AgentEvent } from "@earendil-works/pi-agent-core";
import type { AssistantMessage } from "@earendil-works/pi-ai";
import { describe, expect, it } from "vitest";
import { z } from "zod";
import { tool } from "@/shared/lib/agent/agent-tool";
import {
	type AgentChatMessage,
	applyAgentEvent,
	chatMessagesToAgentMessages,
	userChatMessage,
} from "@/shared/lib/agent/messages";
import { TEST_PI_MODEL, testAssistantMessage } from "@/test/pi-model";

describe("Pi transcript restoration", () => {
	it("preserves legacy step boundaries around tool results", async () => {
		const legacy: AgentChatMessage = {
			id: "assistant-1",
			role: "assistant",
			parts: [
				{ type: "step-start" },
				{
					type: "tool-inspect",
					toolCallId: "call-1",
					state: "output-available",
					input: { target: "track" },
					output: { found: true },
				},
				{ type: "step-start" },
				{ type: "text", text: "Inspection complete." },
			],
		};
		const inspect = tool({
			description: "Inspect",
			inputSchema: z.object({ target: z.string() }),
			execute: async () => ({ found: true }),
		});

		const restored = await chatMessagesToAgentMessages(
			[legacy],
			TEST_PI_MODEL,
			{ inspect },
		);

		expect(restored.map((message) => message.role)).toEqual([
			"assistant",
			"toolResult",
			"assistant",
		]);
		expect(restored[0]).toMatchObject({
			content: [expect.objectContaining({ type: "toolCall", id: "call-1" })],
		});
		expect(restored[2]).toMatchObject({
			content: [{ type: "text", text: "Inspection complete." }],
		});
	});
});

describe("Pi event fold", () => {
	const toolCallRound: AssistantMessage = {
		...testAssistantMessage(""),
		content: [
			{ type: "toolCall", id: "call-1", name: "inspect", arguments: {} },
		],
		stopReason: "toolUse",
	};

	it("keeps a turn's rounds in one assistant message split by steps", () => {
		const user = userChatMessage("look at the track");
		let messages = applyAgentEvent([user], {
			type: "message_start",
			message: toolCallRound,
		} as AgentEvent);
		messages = applyAgentEvent(messages, {
			type: "message_end",
			message: toolCallRound,
		} as AgentEvent);
		messages = applyAgentEvent(messages, {
			type: "message_start",
			message: testAssistantMessage("Inspection complete."),
		} as AgentEvent);

		expect(messages).toHaveLength(2);
		const assistant = messages[1] as AgentChatMessage;
		expect(
			assistant.parts.filter((part) => part.type === "step-start"),
		).toHaveLength(2);
		// The closed round keeps its tool call; the open round holds the text.
		expect(assistant.parts.map((part) => part.type)).toEqual([
			"step-start",
			"tool-inspect",
			"data-pi-message",
			"step-start",
			"text",
			"data-pi-message",
		]);
	});

	it("starts a new assistant message after a user message", () => {
		let messages = applyAgentEvent([userChatMessage("first")], {
			type: "message_start",
			message: testAssistantMessage("one"),
		} as AgentEvent);
		messages = applyAgentEvent([...messages, userChatMessage("second")], {
			type: "message_start",
			message: testAssistantMessage("two"),
		} as AgentEvent);

		expect(messages.map((message) => message.role)).toEqual([
			"user",
			"assistant",
			"user",
			"assistant",
		]);
	});
});
