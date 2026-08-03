import type { ModelMessage, UIMessageChunk } from "ai";
import { stepCountIs, streamText } from "ai";
import type { CreateAiSdkSubagentRunnerOptions, SubagentRunner } from "./types";

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function abortError(): Error {
	return new DOMException("Subagent was aborted.", "AbortError");
}

function insertSteering(
	messages: ModelMessage[],
	insertions: Array<{ index: number; messages: ModelMessage[] }>,
): ModelMessage[] {
	if (insertions.length === 0) return messages;
	const result = [...messages];
	let offset = 0;
	for (const insertion of insertions) {
		result.splice(insertion.index + offset, 0, ...insertion.messages);
		offset += insertion.messages.length;
	}
	return result;
}

/**
 * AI SDK 6 adapter for the runtime. Every invocation starts with exactly one
 * user message, so a child never inherits the parent's transcript.
 */
export function createAiSdkSubagentRunner(
	options: CreateAiSdkSubagentRunnerOptions,
): SubagentRunner {
	return async (request) => {
		let messages: ModelMessage[] = [{ role: "user", content: request.prompt }];
		const responseTexts: string[] = [];

		while (true) {
			let streamError: unknown;
			let finishError: string | undefined;
			let responseText = "";
			const steeringInsertions: Array<{
				index: number;
				messages: ModelMessage[];
			}> = [];
			const result = streamText({
				model: options.createModel(request.model),
				system: request.systemPrompt,
				messages,
				tools: request.tools,
				stopWhen: options.stopWhen ?? stepCountIs(1000),
				abortSignal: request.abortSignal,
				experimental_context: request.context,
				prepareStep: ({ messages: stepMessages }) => {
					const steering = request.drainSteering();
					if (steering.length > 0) {
						steeringInsertions.push({
							index: stepMessages.length,
							messages: steering.map(
								(content): ModelMessage => ({ role: "user", content }),
							),
						});
					}
					const preparedMessages = insertSteering(
						stepMessages,
						steeringInsertions,
					);
					return steeringInsertions.length > 0
						? { messages: preparedMessages }
						: {};
				},
				providerOptions: options.providerOptions?.({
					modelId: request.model,
					thinkingLevel: request.thinkingLevel,
				}),
			});

			const stream = result.toUIMessageStream({
				generateMessageId: () => crypto.randomUUID(),
				onError: (error) => {
					streamError ??= error;
					return errorMessage(error);
				},
			});
			for await (const chunk of stream) {
				await request.onUIMessageChunk(chunk as UIMessageChunk);
				if (chunk.type === "text-delta") responseText += chunk.delta;
				if (chunk.type === "error") finishError = chunk.errorText;
				if (chunk.type === "finish" && chunk.finishReason === "error") {
					finishError ??= "Model turn ended in error.";
				}
			}

			if (request.abortSignal.aborted) throw abortError();
			if (streamError !== undefined) throw streamError;
			if (finishError !== undefined) throw new Error(finishError);

			if (responseText) responseTexts.push(responseText);
			messages = insertSteering(
				[...messages, ...(await result.response).messages],
				steeringInsertions,
			);
			const steering = request.drainSteering();
			if (steering.length === 0) return responseTexts.join("\n\n");
			messages = [
				...messages,
				...steering.map((content): ModelMessage => ({ role: "user", content })),
			];
		}
	};
}
