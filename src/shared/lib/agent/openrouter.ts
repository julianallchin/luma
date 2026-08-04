import { createModels, type Model } from "@earendil-works/pi-ai";
import { openrouterProvider } from "@earendil-works/pi-ai/providers/openrouter";
import { getOpenRouterKey } from "@/features/track-editor/agent/openrouter-key";
import type { PiAgentModel } from "./pi-agent-loop";

const piModels = createModels();
piModels.setProvider(openrouterProvider());

function fallbackModel(modelId: string): Model<"openai-completions"> {
	const family = modelId.split("/", 1)[0];
	const templateId =
		family === "anthropic"
			? "~anthropic/claude-opus-latest"
			: family === "moonshotai"
				? "~moonshotai/kimi-latest"
				: family === "google"
					? "~google/gemini-pro-latest"
					: "openai/gpt-5.4";
	const template = piModels.getModel("openrouter", templateId);
	if (template?.api !== "openai-completions") {
		throw new Error("Pi OpenRouter model catalog has no fallback descriptor.");
	}
	return {
		...(template as Model<"openai-completions">),
		id: modelId,
		name: modelId,
		reasoning: true,
	};
}

/** Pi-native OpenRouter runtime bound to the user's stored key. */
export function lumaPiOpenRouter(modelId: string): PiAgentModel | null {
	const apiKey = getOpenRouterKey();
	if (!apiKey) return null;
	const model =
		piModels.getModel("openrouter", modelId) ?? fallbackModel(modelId);
	return {
		model,
		streamFn: (activeModel, context, options) =>
			piModels.streamSimple(activeModel, context, {
				...options,
				apiKey,
				headers: {
					...options?.headers,
					"HTTP-Referer": "https://luma.show",
					"X-Title": "Luma",
				},
			}),
	};
}
