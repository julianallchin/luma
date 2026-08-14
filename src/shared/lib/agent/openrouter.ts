import { createModels, type Model } from "@earendil-works/pi-ai";
import { openrouterProvider } from "@earendil-works/pi-ai/providers/openrouter";
import { vercelAIGatewayProvider } from "@earendil-works/pi-ai/providers/vercel-ai-gateway";
import {
	getAgentProvider,
	getGatewayKey,
	getOpenRouterKey,
} from "@/features/track-editor/agent/openrouter-key";
import { installGatewayFetch } from "./gateway-fetch";
import type { PiAgentModel } from "./pi-agent-loop";

installGatewayFetch();

const piModels = createModels();
piModels.setProvider(openrouterProvider());
piModels.setProvider(vercelAIGatewayProvider());

function fallbackOpenRouterModel(modelId: string): Model<"openai-completions"> {
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

function fallbackGatewayModel(modelId: string): Model<"anthropic-messages"> {
	const template = piModels.getModel(
		"vercel-ai-gateway",
		"anthropic/claude-opus-4.6",
	);
	if (template?.api !== "anthropic-messages") {
		throw new Error("Pi Vercel Gateway catalog has no fallback descriptor.");
	}
	return {
		...(template as Model<"anthropic-messages">),
		id: modelId,
		name: modelId,
		reasoning: true,
	};
}

/** Pi-native runtime bound to the active provider and its stored key. */
export function lumaPiModel(modelId: string): PiAgentModel | null {
	const provider = getAgentProvider();
	const apiKey =
		provider === "vercel-ai-gateway" ? getGatewayKey() : getOpenRouterKey();
	if (!apiKey) return null;
	const model =
		piModels.getModel(provider, modelId) ??
		(provider === "vercel-ai-gateway"
			? fallbackGatewayModel(modelId)
			: fallbackOpenRouterModel(modelId));
	return {
		model,
		streamFn: (activeModel, context, options) =>
			piModels.streamSimple(activeModel, context, {
				...options,
				apiKey,
				...(provider === "openrouter"
					? {
							headers: {
								...options?.headers,
								"HTTP-Referer": "https://luma.show",
								"X-Title": "Luma",
							},
						}
					: {}),
			}),
	};
}
