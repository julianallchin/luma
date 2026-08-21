/**
 * Minimal `window.__TAURI_INTERNALS__` so `@tauri-apps/api` resolves instead of
 * throwing in a plain browser page.
 *
 * The 3D harness deliberately runs the visualizer with *no* backend: every
 * store it needs is seeded directly and fixture state comes through
 * `PrimitiveOverrideContext`. But `StageVisualizer` unconditionally mounts the
 * universe-state listener, and a rejected `listen()` there would surface as an
 * unhandled rejection on every capture. Stubbing the bridge to a no-op keeps
 * "the harness makes zero IPC calls" an observable property rather than a
 * swallowed error.
 */

interface TauriInternals {
	invoke: (cmd: string) => Promise<unknown>;
	transformCallback: (
		callback: (payload: unknown) => void,
		once: boolean,
	) => number;
	unregisterCallback: (id: number) => void;
	convertFileSrc: (filePath: string) => string;
}

declare global {
	interface Window {
		__TAURI_INTERNALS__?: TauriInternals;
		/** Commands the page attempted, so a capture can assert none were made. */
		__TAURI_CALLS__?: string[];
	}
}

export function installTauriStub(): void {
	const calls: string[] = [];
	window.__TAURI_CALLS__ = calls;
	let nextCallbackId = 1;

	window.__TAURI_INTERNALS__ = {
		invoke: (cmd) => {
			calls.push(cmd);
			// `plugin:event|listen` resolves to an unlisten handle id; every
			// other command resolves to null, which every caller here treats
			// as "nothing to show".
			return Promise.resolve(cmd === "plugin:event|listen" ? 0 : null);
		},
		transformCallback: () => nextCallbackId++,
		unregisterCallback: () => {},
		convertFileSrc: (filePath) => filePath,
	};
}
