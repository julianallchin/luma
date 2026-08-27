/**
 * The client half of MCP stdio, against Luma's `luma-mcp` binary.
 *
 * Newline-delimited JSON-RPC on the child's stdin/stdout — the same wire any
 * MCP client speaks, so a script that uses this is testing the protocol, not a
 * convenience wrapper around the services underneath it.
 *
 * Requests are correlated by id and answered concurrently: the server runs one
 * task per request precisely so `cancel` can reach a `python` still in flight,
 * and a client that awaited serially could never deliver it.
 */

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

/** Where the app itself keeps `luma.db`, `state.db` and `tracks/`. */
export const REAL_CONFIG_DIR = join(homedir(), "Library/Application Support/com.luma.luma");
/** The managed venv and deployed `luma_exec`. Hosts only ever read from it. */
export const REAL_CACHE_DIR = join(homedir(), "Library/Caches/com.luma.luma");

export type Content = { type: string; text?: string; data?: string; mimeType?: string };
export type ToolResult = { content: Content[]; isError?: boolean };

export type McpServerOptions = {
	/** Library directory. Defaults to the real app config dir — be careful. */
	configDir?: string;
	cacheDir?: string;
	fixturesRoot?: string;
	/** Arms the write gate without a Supabase session; requires `configDir`. */
	fixturePrincipal?: string;
	/** What the handshake calls this client. Becomes the `client:<name>/<version>`
	 * half of every revision's actor. */
	clientInfo?: { name: string; version: string };
	/** Where the child's stderr goes. `"inherit"` by default. */
	stderr?: "inherit" | "ignore";
};

/** Path to the built binary, honouring `LUMA_MCP_BIN`. Throws with the build
 * command when it is missing, because "ENOENT" is not an actionable error. */
export function mcpBinary(): string {
	const binary = process.env.LUMA_MCP_BIN ?? join(REPO_ROOT, "src-tauri/target/debug/luma-mcp");
	if (!existsSync(binary)) {
		throw new Error(
			`luma-mcp not found at ${binary}. Build it first:\n` +
				"  cargo build --bin luma-mcp --manifest-path src-tauri/Cargo.toml",
		);
	}
	return binary;
}

/** The argv a host takes for a given library. Shared with anything that has to
 * spawn the same server a different way (an `--mcp-config` file, say). */
export function mcpArgs(options: McpServerOptions = {}): string[] {
	const args: string[] = [];
	if (options.configDir) args.push("--config-dir", options.configDir);
	args.push("--cache-dir", options.cacheDir ?? REAL_CACHE_DIR);
	if (options.fixturesRoot) args.push("--fixtures-root", options.fixturesRoot);
	if (options.fixturePrincipal) args.push("--fixture-principal", options.fixturePrincipal);
	return args;
}

export type McpServer = {
	request: <T = any>(method: string, params?: unknown) => Promise<T>;
	notify: (method: string) => void;
	callTool: (name: string, args?: Record<string, unknown>) => Promise<ToolResult>;
	/** `initialize` + `notifications/initialized`. Returns the server's reply. */
	handshake: () => Promise<any>;
	close: () => Promise<void>;
};

export function startMcpServer(options: McpServerOptions = {}): McpServer {
	const child: ChildProcess = spawn(mcpBinary(), mcpArgs(options), {
		stdio: ["pipe", "pipe", options.stderr ?? "inherit"],
		cwd: REPO_ROOT,
	});

	const pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
	let nextId = 1;
	let exited: Error | null = null;
	child.on("exit", (code, signal) => {
		exited = new Error(`luma-mcp exited (code=${code} signal=${signal})`);
		for (const p of pending.values()) p.reject(exited);
		pending.clear();
	});

	createInterface({ input: child.stdout as NodeJS.ReadableStream }).on("line", (line) => {
		if (!line.trim()) return;
		const frame = JSON.parse(line);
		const p = pending.get(frame.id);
		if (!p) return void process.stderr.write(`[mcp-client] unmatched frame: ${line}\n`);
		pending.delete(frame.id);
		if (frame.error) p.reject(new Error(JSON.stringify(frame.error)));
		else p.resolve(frame.result);
	});

	const request = <T = any>(method: string, params?: unknown): Promise<T> => {
		if (exited) return Promise.reject(exited);
		const id = nextId++;
		return new Promise<T>((res, rej) => {
			pending.set(id, { resolve: res, reject: rej });
			child.stdin?.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
		});
	};
	const notify = (method: string) => {
		child.stdin?.write(`${JSON.stringify({ jsonrpc: "2.0", method })}\n`);
	};

	return {
		request,
		notify,
		callTool: (name: string, args: Record<string, unknown> = {}) =>
			request<ToolResult>("tools/call", { name, arguments: args }),
		handshake: async () => {
			const initialized = await request("initialize", {
				protocolVersion: "2024-11-05",
				capabilities: {},
				clientInfo: options.clientInfo ?? { name: "luma-headless", version: "0" },
			});
			notify("notifications/initialized");
			return initialized;
		},
		close: () =>
			new Promise<void>((res) => {
				if (exited) return res();
				child.once("exit", () => res());
				child.stdin?.end();
				setTimeout(() => child.kill(), 5000).unref?.();
			}),
	};
}

/** Every text block of a tool result, joined. Images are dropped. */
export function textOf(result: ToolResult): string {
	return result.content
		.filter((block) => block.type === "text")
		.map((block) => block.text ?? "")
		.join("\n");
}
