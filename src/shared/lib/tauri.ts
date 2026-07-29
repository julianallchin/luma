import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/**
 * The single `invoke` seam for everything on the agent path.
 *
 * In the app this is just `@tauri-apps/api/core`'s `invoke`. The headless agent
 * harness (and unit tests) run outside a Tauri webview, where that import
 * throws, so they call `setInvoke()` once at startup to point the same seam at
 * a direct command dispatcher.
 *
 * Import `invoke` from here — never from `@tauri-apps/api/core` — in any file
 * an agent can reach.
 */
export type InvokeFn = <T>(
	command: string,
	args?: Record<string, unknown>,
) => Promise<T>;

const defaultInvoke: InvokeFn = (command, args) => tauriInvoke(command, args);

let current: InvokeFn = defaultInvoke;

/** Swap the invoke implementation (headless harness, tests). */
export function setInvoke(fn: InvokeFn): void {
	current = fn;
}

/** Restore the real Tauri `invoke`. */
export function resetInvoke(): void {
	current = defaultInvoke;
}

/** Dispatches through whatever `setInvoke` last installed. */
export const invoke: InvokeFn = (command, args) => current(command, args);
