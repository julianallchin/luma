/**
 * Bun-side half of the headless harness.
 *
 * `@tauri-apps/api/core`'s `invoke` is a one-liner over
 * `window.__TAURI_INTERNALS__.invoke`. Install our own `__TAURI_INTERNALS__`
 * before any frontend module is imported and every `invoke(...)` call in `src/`
 * is transparently answered by the `agent_harness` binary over a pipe — with no
 * changes to the frontend code under test.
 *
 * The import order is load-bearing:
 *
 * ```ts
 * const h = await startHarness();          // installs the globals
 * const { buildTools } = await import("@/features/track-editor/agent/tools");
 * ```
 *
 * A static `import` of frontend code at the top of your file would be hoisted
 * above `startHarness()` and would capture an un-shimmed global — always use a
 * dynamic `import()` after the await.
 */

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createInterface } from "node:readline";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

export type HarnessOptions = {
	/** Config dir holding `luma.db` / `state.db`. Defaults to `LUMA_CONFIG_DIR`,
	 * else the harness's own default (the real app config dir — be careful). */
	configDir?: string;
	/** Fixture library root. Defaults to the repo's newest `resources/fixtures/*`. */
	fixturesRoot?: string;
	/** App cache dir holding the managed venv (`python-env/bin/python3`) and the
	 * deployed `luma_exec`. Defaults to `LUMA_CACHE_DIR`, else the real
	 * `~/Library/Caches/com.luma.luma`. The harness only ever reads from it —
	 * agent workspaces live under the *config* dir. */
	cacheDir?: string;
	/** Harness binary. Defaults to `LUMA_HARNESS_BIN`, else `src-tauri/target/debug/agent_harness`. */
	binary?: string;
	/** Forward the harness's stderr to ours. Default true. */
	verbose?: boolean;
};

export type Harness = {
	invoke: <T = unknown>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
	close: () => Promise<void>;
	/** Absolute path of the config dir the harness opened. */
	configDir?: string;
};

type Pending = {
	resolve: (v: unknown) => void;
	reject: (e: Error) => void;
	cmd: string;
};

function defaultBinary(): string {
	const fromEnv = process.env.LUMA_HARNESS_BIN;
	if (fromEnv) return fromEnv;
	const built = resolve(REPO_ROOT, "src-tauri/target/debug/agent_harness");
	if (!existsSync(built)) {
		throw new Error(
			`agent_harness not found at ${built}. Build it first:\n` +
				"  cargo build --manifest-path src-tauri/Cargo.toml --bin agent_harness\n" +
				"or set LUMA_HARNESS_BIN.",
		);
	}
	return built;
}

/** Spawn the harness and install the Tauri IPC globals. */
export async function startHarness(opts: HarnessOptions = {}): Promise<Harness> {
	const binary = opts.binary ?? defaultBinary();
	const configDir = opts.configDir ?? process.env.LUMA_CONFIG_DIR;

	const argv: string[] = [];
	if (configDir) argv.push("--config-dir", configDir);
	if (opts.fixturesRoot) argv.push("--fixtures-root", opts.fixturesRoot);
	if (opts.cacheDir) argv.push("--cache-dir", opts.cacheDir);

	const child: ChildProcess = spawn(binary, argv, {
		stdio: ["pipe", "pipe", opts.verbose === false ? "ignore" : "inherit"],
		cwd: REPO_ROOT,
	});

	const pending = new Map<string, Pending>();
	let nextId = 1;
	let exited: Error | null = null;

	child.on("exit", (code, signal) => {
		exited = new Error(`agent_harness exited (code=${code} signal=${signal})`);
		for (const p of pending.values()) p.reject(exited);
		pending.clear();
	});

	const rl = createInterface({ input: child.stdout as NodeJS.ReadableStream });
	rl.on("line", (line) => {
		if (!line.trim()) return;
		let frame: { id?: unknown; ok?: unknown; err?: unknown };
		try {
			frame = JSON.parse(line);
		} catch {
			process.stderr.write(`[shim] unparseable harness frame: ${line}\n`);
			return;
		}
		const key = String(frame.id);
		const p = pending.get(key);
		if (!p) {
			// id null = a frame the harness could not attribute (bad request JSON).
			process.stderr.write(`[shim] unmatched harness frame: ${line}\n`);
			return;
		}
		pending.delete(key);
		if (frame.err !== undefined) p.reject(new Error(String(frame.err)));
		else p.resolve(frame.ok);
	});

	const invoke = <T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
		if (exited) return Promise.reject(exited);
		const id = String(nextId++);
		return new Promise<T>((res, rej) => {
			pending.set(id, { resolve: res as (v: unknown) => void, reject: rej, cmd });
			child.stdin?.write(`${JSON.stringify({ id, cmd, args: args ?? {} })}\n`);
		});
	};

	installGlobals(invoke);

	return {
		invoke,
		configDir,
		close: () =>
			new Promise<void>((res) => {
				if (exited) return res();
				child.once("exit", () => res());
				child.stdin?.end();
				setTimeout(() => child.kill(), 2000).unref?.();
			}),
	};
}

/**
 * Everything `@tauri-apps/api` reaches for on `window`, plus the two browser
 * globals the agent modules touch (`localStorage` in `openrouter-key.ts`, and
 * `window.addEventListener`/`dispatchEvent` for its change notifications).
 */
function installGlobals(invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>) {
	const g = globalThis as Record<string, unknown>;

	const listeners = new Map<string, Set<(e: unknown) => void>>();
	const win = (g.window as Record<string, unknown> | undefined) ?? {};
	Object.assign(win, {
		addEventListener: (type: string, fn: (e: unknown) => void) => {
			if (!listeners.has(type)) listeners.set(type, new Set());
			listeners.get(type)?.add(fn);
		},
		removeEventListener: (type: string, fn: (e: unknown) => void) => {
			listeners.get(type)?.delete(fn);
		},
		dispatchEvent: (e: { type?: string }) => {
			for (const fn of listeners.get(e?.type ?? "") ?? []) fn(e);
			return true;
		},
	});

	let callbackId = 0;
	const callbacks = new Map<number, (payload: unknown) => void>();
	win.__TAURI_INTERNALS__ = {
		invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
		// Tauri hands a numeric token to Rust and calls back through
		// `window[token]`. Nothing on an agent path emits events, so a local
		// registry is enough to keep `Channel`/`listen` from throwing.
		transformCallback: (cb?: (payload: unknown) => void, once = false) => {
			const id = ++callbackId;
			(win as Record<string, unknown>)[`_${id}`] = (payload: unknown) => {
				if (once) callbacks.delete(id);
				cb?.(payload);
			};
			if (cb) callbacks.set(id, cb);
			return id;
		},
		unregisterCallback: (id: number) => {
			callbacks.delete(id);
			delete (win as Record<string, unknown>)[`_${id}`];
		},
		// The app serves files over the `asset:` protocol; under Bun a plain
		// `file://` URL is the honest equivalent.
		convertFileSrc: (filePath: string) => pathToFileURL(filePath).href,
	};

	g.window = win;

	if (!g.localStorage) {
		const store = new Map<string, string>();
		g.localStorage = {
			getItem: (k: string) => store.get(k) ?? null,
			setItem: (k: string, v: string) => void store.set(k, String(v)),
			removeItem: (k: string) => void store.delete(k),
			clear: () => store.clear(),
			key: (i: number) => [...store.keys()][i] ?? null,
			get length() {
				return store.size;
			},
		};
	}
	(win as Record<string, unknown>).localStorage = g.localStorage;
}
