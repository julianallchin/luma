import { useEffect, useState } from "react";

export const OPENROUTER_KEY_STORAGE = "luma:openrouter-api-key";
export const GATEWAY_KEY_STORAGE = "luma:ai-gateway-api-key";
export const AGENT_PROVIDER_STORAGE = "luma:agent-provider";
export const OPENROUTER_MODEL = "anthropic/claude-opus-5";
export const VENUE_EXPERT_MODEL = "moonshotai/kimi-k2.6:nitro";
const KEY_CHANGED_EVENT = "luma:openrouter-key-changed";

/** Which service the agents call. Both speak "creator/model" model ids and are
 * driven through the same Vercel AI SDK; only the key and routing differ. */
export type AgentProvider = "openrouter" | "vercel-ai-gateway";

export const AGENT_PROVIDER_LABELS: Record<AgentProvider, string> = {
	openrouter: "OpenRouter",
	"vercel-ai-gateway": "Vercel AI Gateway",
};

export function getAgentProvider(): AgentProvider {
	return localStorage.getItem(AGENT_PROVIDER_STORAGE) === "vercel-ai-gateway"
		? "vercel-ai-gateway"
		: "openrouter";
}

export function setAgentProvider(provider: AgentProvider): void {
	localStorage.setItem(AGENT_PROVIDER_STORAGE, provider);
	window.dispatchEvent(new Event(KEY_CHANGED_EVENT));
}

export function getGatewayKey(): string | null {
	const raw = localStorage.getItem(GATEWAY_KEY_STORAGE);
	if (!raw) return null;
	const trimmed = raw.trim();
	return trimmed.length > 0 ? trimmed : null;
}

export function setGatewayKey(value: string): void {
	const trimmed = value.trim();
	if (trimmed.length === 0) {
		localStorage.removeItem(GATEWAY_KEY_STORAGE);
	} else {
		localStorage.setItem(GATEWAY_KEY_STORAGE, trimmed);
	}
	window.dispatchEvent(new Event(KEY_CHANGED_EVENT));
}

/** The active provider's API key, or null when it isn't configured. */
export function getAgentApiKey(): string | null {
	return getAgentProvider() === "vercel-ai-gateway"
		? getGatewayKey()
		: getOpenRouterKey();
}

/** Store a key for whichever provider is currently active. */
export function setAgentApiKey(value: string): void {
	if (getAgentProvider() === "vercel-ai-gateway") {
		setGatewayKey(value);
	} else {
		setOpenRouterKey(value);
	}
}

/** Subscribes to the active provider and its key across settings changes. */
export function useAgentApiKey(): {
	provider: AgentProvider;
	key: string | null;
} {
	const [config, setConfig] = useState(() => ({
		provider: getAgentProvider(),
		key: getAgentApiKey(),
	}));

	useEffect(() => {
		const update = () =>
			setConfig({ provider: getAgentProvider(), key: getAgentApiKey() });
		window.addEventListener(KEY_CHANGED_EVENT, update);
		window.addEventListener("storage", update);
		return () => {
			window.removeEventListener(KEY_CHANGED_EVENT, update);
			window.removeEventListener("storage", update);
		};
	}, []);

	return config;
}

export function getOpenRouterKey(): string | null {
	const raw = localStorage.getItem(OPENROUTER_KEY_STORAGE);
	if (!raw) return null;
	const trimmed = raw.trim();
	return trimmed.length > 0 ? trimmed : null;
}

export function setOpenRouterKey(value: string): void {
	const trimmed = value.trim();
	if (trimmed.length === 0) {
		localStorage.removeItem(OPENROUTER_KEY_STORAGE);
	} else {
		localStorage.setItem(OPENROUTER_KEY_STORAGE, trimmed);
	}
	window.dispatchEvent(new Event(KEY_CHANGED_EVENT));
}

/** Subscribes to localStorage + same-tab updates of the OpenRouter key. */
export function useOpenRouterKey(): string | null {
	const [key, setKey] = useState<string | null>(() => getOpenRouterKey());

	useEffect(() => {
		const update = () => setKey(getOpenRouterKey());
		window.addEventListener(KEY_CHANGED_EVENT, update);
		window.addEventListener("storage", (e) => {
			if (e.key === OPENROUTER_KEY_STORAGE) update();
		});
		return () => {
			window.removeEventListener(KEY_CHANGED_EVENT, update);
		};
	}, []);

	return key;
}
