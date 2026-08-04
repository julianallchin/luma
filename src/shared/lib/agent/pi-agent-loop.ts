import { Agent, type StreamFn } from "@earendil-works/pi-agent-core";
import type { Api, Model, ThinkingLevel } from "@earendil-works/pi-ai";
import { type ToolSet, toPiTools } from "./agent-tool";
import { type AgentChatMessage, chatMessagesToAgentMessages } from "./messages";

export type PiAgentModel = {
	model: Model<Api>;
	streamFn: StreamFn;
};

export async function createPiAgent(args: {
	runtime: PiAgentModel;
	systemPrompt: string;
	tools: ToolSet;
	messages?: AgentChatMessage[];
	thinkingLevel?: ThinkingLevel;
	sessionId?: string;
	context?: unknown;
}): Promise<Agent> {
	return new Agent({
		sessionId: args.sessionId,
		initialState: {
			systemPrompt: args.systemPrompt,
			model: args.runtime.model,
			thinkingLevel: args.thinkingLevel ?? "off",
			tools: toPiTools(args.tools, args.context),
			messages: await chatMessagesToAgentMessages(
				args.messages ?? [],
				args.runtime.model,
				args.tools,
			),
		},
		steeringMode: "all",
		followUpMode: "all",
		streamFn: args.runtime.streamFn,
	});
}

export async function completePiText(args: {
	runtime: PiAgentModel;
	systemPrompt: string;
	prompt: string;
	signal?: AbortSignal;
	thinkingLevel?: Exclude<ThinkingLevel, "off">;
}): Promise<string> {
	const stream = await args.runtime.streamFn(
		args.runtime.model,
		{
			systemPrompt: args.systemPrompt,
			messages: [{ role: "user", content: args.prompt, timestamp: Date.now() }],
		},
		{ signal: args.signal, reasoning: args.thinkingLevel },
	);
	const message = await stream.result();
	if (message.stopReason === "error" || message.stopReason === "aborted") {
		throw new Error(message.errorMessage ?? "Pi model call failed.");
	}
	return message.content
		.filter((part) => part.type === "text")
		.map((part) => part.text)
		.join("");
}

export { Agent };
