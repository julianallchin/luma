import { describe, expect, it } from "vitest";
import { z } from "zod";
import { tool } from "@/shared/lib/agent/agent-tool";
import {
	type AgentChatMessage,
	chatMessagesToAgentMessages,
} from "@/shared/lib/agent/messages";
import { TEST_PI_MODEL } from "@/test/pi-model";

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
