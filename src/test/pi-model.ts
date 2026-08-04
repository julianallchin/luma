import {
	type AssistantMessage,
	type AssistantMessageEventStream,
	createAssistantMessageEventStream,
	type Model,
} from "@earendil-works/pi-ai";

export const TEST_PI_MODEL: Model<"openai-completions"> = {
	id: "test-model",
	name: "Test model",
	api: "openai-completions",
	provider: "test",
	baseUrl: "http://test.invalid",
	reasoning: false,
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 100_000,
	maxTokens: 8_192,
};

const EMPTY_USAGE: AssistantMessage["usage"] = {
	input: 1,
	output: 1,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 2,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

export function testAssistantMessage(
	text: string,
	options: { error?: string; aborted?: boolean } = {},
): AssistantMessage {
	return {
		role: "assistant",
		content: text ? [{ type: "text", text }] : [],
		api: TEST_PI_MODEL.api,
		provider: TEST_PI_MODEL.provider,
		model: TEST_PI_MODEL.id,
		usage: EMPTY_USAGE,
		stopReason: options.aborted ? "aborted" : options.error ? "error" : "stop",
		...(options.error ? { errorMessage: options.error } : {}),
		timestamp: Date.now(),
	};
}

export function testTextStream(
	text: string,
	options: {
		error?: string;
		aborted?: boolean;
		beforeText?: () => void | Promise<void>;
		afterText?: () => void | Promise<void>;
	} = {},
): AssistantMessageEventStream {
	const stream = createAssistantMessageEventStream();
	void (async () => {
		await options.beforeText?.();
		const partial = testAssistantMessage("");
		stream.push({ type: "start", partial });
		if (text) {
			stream.push({ type: "text_start", contentIndex: 0, partial });
			const withText = testAssistantMessage(text);
			stream.push({
				type: "text_delta",
				contentIndex: 0,
				delta: text,
				partial: withText,
			});
			await options.afterText?.();
			stream.push({
				type: "text_end",
				contentIndex: 0,
				content: text,
				partial: withText,
			});
		} else {
			await options.afterText?.();
		}
		const message = testAssistantMessage(text, options);
		if (message.stopReason === "error" || message.stopReason === "aborted") {
			stream.push({
				type: "error",
				reason: message.stopReason,
				error: message,
			});
		} else {
			stream.push({ type: "done", reason: "stop", message });
		}
	})();
	return stream;
}
