import type { AgentEvent } from "@earendil-works/pi-agent-core";
import {
	type AgentChatMessage,
	applyAgentEvent,
	userAgentMessage,
	userChatMessage,
} from "@/shared/lib/agent/messages";
import { createPiAgent } from "@/shared/lib/agent/pi-agent-loop";
import type { CreatePiSubagentRunnerOptions, SubagentRunner } from "./types";

function abortError(): Error {
	return new DOMException("Subagent was aborted.", "AbortError");
}

/** A fresh Pi Agent per child, with Pi's native steering queue and loop. */
export function createPiSubagentRunner(
	options: CreatePiSubagentRunnerOptions,
): SubagentRunner {
	return async (request) => {
		const runtime = options.createModel(request.model);
		const prompt = userChatMessage(request.prompt);
		let messages: AgentChatMessage[] = [prompt];
		const responseTexts: string[] = [];
		let turnError: string | undefined;
		const agent = await createPiAgent({
			runtime,
			systemPrompt: request.systemPrompt,
			tools: request.tools,
			thinkingLevel:
				request.thinkingLevel === "off" ? undefined : request.thinkingLevel,
			context: request.context,
		});
		const unsubscribeAgent = agent.subscribe(async (event: AgentEvent) => {
			messages = applyAgentEvent(messages, event);
			await request.onMessages(messages);
			await request.onAgentEvent(event);
			if (event.type === "message_end" && event.message.role === "assistant") {
				const text = event.message.content
					.filter((part) => part.type === "text")
					.map((part) => part.text)
					.join("");
				if (text) responseTexts.push(text);
				turnError =
					event.message.stopReason === "error"
						? (event.message.errorMessage ?? "Model turn ended in error.")
						: undefined;
			}
		});
		const unsubscribeSteering = request.subscribeSteering((message) => {
			agent.steer({ role: "user", content: message, timestamp: Date.now() });
		});
		for (const message of request.drainSteering()) {
			agent.steer({ role: "user", content: message, timestamp: Date.now() });
		}
		const abort = () => agent.abort();
		request.abortSignal.addEventListener("abort", abort, { once: true });
		try {
			if (request.abortSignal.aborted) throw abortError();
			await agent.prompt(userAgentMessage(prompt));
			if (request.abortSignal.aborted) throw abortError();
			if (turnError) throw new Error(turnError);
			return responseTexts.join("\n\n");
		} finally {
			request.abortSignal.removeEventListener("abort", abort);
			unsubscribeSteering();
			unsubscribeAgent();
		}
	};
}
