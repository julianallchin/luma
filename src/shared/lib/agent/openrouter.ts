import {
	createOpenRouter,
	type OpenRouterProvider,
} from "@openrouter/ai-sdk-provider";
import { getOpenRouterKey } from "@/features/track-editor/agent/openrouter-key";

/** The one place OpenRouter's app attribution is configured. */
export function createLumaOpenRouter(apiKey: string): OpenRouterProvider {
	return createOpenRouter({
		apiKey,
		appName: "Luma",
		appUrl: "https://luma.show",
	});
}

/** The provider bound to the user's stored key, or null when no key is set. */
export function lumaOpenRouter(): OpenRouterProvider | null {
	const apiKey = getOpenRouterKey();
	return apiKey ? createLumaOpenRouter(apiKey) : null;
}
