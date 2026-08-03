import {
	createOpenRouter,
	type OpenRouterProvider,
} from "@openrouter/ai-sdk-provider";
import { createGateway, type LanguageModel } from "ai";
import {
	getAgentProvider,
	getGatewayKey,
	getOpenRouterKey,
} from "@/features/track-editor/agent/openrouter-key";

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

/** OpenRouter routing suffixes (":nitro", ":floor") are not part of the
 * canonical "creator/model" id and other providers reject them. */
function canonicalModelId(modelId: string): string {
	return modelId.replace(/:[a-z-]+$/, "");
}

/** The gateway SDK stamps a `user-agent` header on every request. WebKit
 * surfaces it in the CORS preflight and the gateway's CORS policy rejects it,
 * failing every call from the webview — strip it before dispatch. */
const fetchWithoutUserAgent: typeof fetch = (input, init) => {
	const headers = new Headers(init?.headers);
	headers.delete("user-agent");
	return fetch(input, { ...init, headers });
};

/** A model on the user's chosen provider (OpenRouter or Vercel AI Gateway),
 * or null when that provider has no key configured. Model ids use the shared
 * "creator/model" form; OpenRouter-only routing suffixes are stripped for the
 * gateway. */
export function lumaLanguageModel(modelId: string): LanguageModel | null {
	if (getAgentProvider() === "vercel-ai-gateway") {
		const apiKey = getGatewayKey();
		if (!apiKey) return null;
		return createGateway({ apiKey, fetch: fetchWithoutUserAgent })(
			canonicalModelId(modelId),
		);
	}
	return lumaOpenRouter()?.(modelId) ?? null;
}
