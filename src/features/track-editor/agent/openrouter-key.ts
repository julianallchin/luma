import { useEffect, useState } from "react";

export const OPENROUTER_KEY_STORAGE = "luma:openrouter-api-key";
export const OPENROUTER_MODEL = "moonshotai/kimi-k2.5:nitro";
const KEY_CHANGED_EVENT = "luma:openrouter-key-changed";

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
