/**
 * Point Claude Code at Luma and let it author a show:
 *
 * ```
 * bun run scripts/headless/author_score.ts <track> <venue> [--model opus] [--max-turns 40]
 * ```
 *
 * `<track>` and `<venue>` are ids or substrings of a title / venue name.
 *
 * The whole integration is `luma-mcp`: this script resolves the track/venue
 * pair with that server's read-only `find` (so a typo fails here, in a second,
 * rather than three turns into a paid agent), writes an `--mcp-config`
 * naming that same binary, and spawns `claude -p` with the *in-app* track
 * copilot's system prompt (`src-tauri/src/agent/prompts/track.md`). The agent
 * that comes up is therefore the same collaborator the track editor hosts —
 * same kernel, same bindings, same playbooks — with Claude Code's harness
 * around it instead of the editor's.
 *
 * MCP is its only tool surface (`--strict-mcp-config`, built-ins off), so the
 * stream-json trace this prints is a complete record of what it did.
 *
 * Writes to the real library by default: the score it authors is the one that
 * opens in the app. Pass `--config-dir` (with `--fixture-principal`) to work
 * against a scratch copy instead, exactly as `mcp_smoke.ts` does.
 */

import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
	mcpArgs,
	mcpBinary,
	type McpServerOptions,
	REAL_CACHE_DIR,
	startMcpServer,
	textOf,
} from "./mcp-client";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const TRACK_PROMPT = join(REPO_ROOT, "src-tauri/src/agent/prompts/track.md");

// -----------------------------------------------------------------------------
// Arguments
// -----------------------------------------------------------------------------

type Options = {
	track: string;
	venue: string;
	model: string;
	maxTurns: number;
	host: McpServerOptions;
};

function parseArgs(argv: string[]): Options {
	const positional: string[] = [];
	const host: McpServerOptions = { cacheDir: REAL_CACHE_DIR };
	let model = "opus";
	let maxTurns = 40;
	for (let i = 0; i < argv.length; i++) {
		const flag = argv[i];
		const value = () => {
			const next = argv[++i];
			if (next === undefined) throw new Error(`${flag} requires a value`);
			return next;
		};
		switch (flag) {
			case "--model":
				model = value();
				break;
			case "--max-turns":
				maxTurns = Number(value());
				break;
			case "--config-dir":
				host.configDir = value();
				break;
			case "--cache-dir":
				host.cacheDir = value();
				break;
			case "--fixtures-root":
				host.fixturesRoot = value();
				break;
			case "--fixture-principal":
				host.fixturePrincipal = value();
				break;
			default:
				if (flag.startsWith("--")) throw new Error(`unknown flag ${flag}`);
				positional.push(flag);
		}
	}
	const [track, venue] = positional;
	if (!track || !venue) {
		throw new Error(
			"usage: bun run scripts/headless/author_score.ts <track-id-or-title> <venue-id-or-name> " +
				"[--model M] [--max-turns N] [--config-dir D] [--fixture-principal P]",
		);
	}
	return { track, venue, model, maxTurns, host };
}

// -----------------------------------------------------------------------------
// Resolving the pair
// -----------------------------------------------------------------------------

type Binding = { label: string; trackId: string; venueId: string; venueName: string };

/**
 * Turn the two loose arguments into the ids the agent will be handed.
 *
 * `find` and nothing else: a lookup must not author, and `open` does — it pins
 * a thread and mints a score for a track the venue has never seen. The spawned
 * agent is the one that opens, so this script never leaves a revision behind
 * for a run that failed on a typo.
 */
async function resolveBinding(options: Options): Promise<Binding> {
	const server = startMcpServer({
		...options.host,
		clientInfo: { name: "author_score", version: "0" },
		stderr: "ignore",
	});
	try {
		await server.handshake();
		const found = await server.callTool("find", { track: options.track, venue: options.venue });
		if (found.isError) throw new Error(textOf(found));
		const body = textOf(found);
		const [trackHalf = "", venueHalf = ""] = body.split(/\n\d+ venues:\n/);
		const rows = (text: string) =>
			text
				.split("\n")
				.map((line) => line.match(/^ {2}(\S+) {2}(.+)$/))
				.filter((m): m is RegExpMatchArray => Boolean(m))
				.map((m) => ({ id: m[1], label: m[2].trim() }));
		const one = (what: string, query: string, matched: { id: string; label: string }[]) => {
			if (matched.length === 1) return matched[0];
			const detail = matched.map((row) => `  ${row.id}  ${row.label}`).join("\n");
			throw new Error(`'${query}' matches ${matched.length} ${what}s${detail ? `:\n${detail}` : ""}`);
		};
		const track = one("track", options.track, rows(trackHalf));
		const venue = one("venue", options.venue, rows(venueHalf));
		return {
			label: track.label,
			trackId: track.id,
			venueId: venue.id,
			venueName: venue.label,
		};
	} finally {
		await server.close();
	}
}

// -----------------------------------------------------------------------------
// The brief
// -----------------------------------------------------------------------------

/**
 * The job, on top of `track.md`'s craft. Deliberately says what to *achieve*
 * and not how to code it: the catalog `open` returns is the API reference, and
 * a script that restated it would be a second copy to drift.
 */
function brief(binding: Binding, model: string): string {
	return [
		`Author a lighting score for **${binding.label}** in the venue **${binding.venueName}**.`,
		"",
		"1. `open` the workspace: " +
			`\`track_id: "${binding.trackId}"\`, \`venue_id: "${binding.venueId}"\`, ` +
			`\`model: "${model}"\`. The reply is your API reference — read it.`,
		"2. Listen before you author. Establish the sections, the phrase lengths, and where the" +
			" real drops are, from `luma.features` and `luma.audio`.",
		"3. Load the genre skill that fits what you heard with the `skill` tool, plus whatever" +
			" craft skill the track calls for.",
		"4. Learn the rig from `luma.venue` — groups, axes, what it can actually articulate.",
		"5. Author the show with `luma.track.edit()`. Work in coherent sections, `check()` and" +
			" `diff()` before each `apply()`. Cover the whole track, not a demo bar or two.",
		"6. Verify: render 2–3 `luma.venue.render(view=..., t=...)` frames at moments that should" +
			" look different from each other, and say whether they do.",
		"7. Finish with two or three sentences on what the room will feel like, and one line of" +
			" plain numbers: clips authored, sections covered, groups used.",
		"",
		"You have no filesystem and no shell. The `mcp__luma__*` tools are the entire world.",
	].join("\n");
}

// -----------------------------------------------------------------------------
// The trace
// -----------------------------------------------------------------------------

const CLIP = 240;

function oneLine(text: string, limit = CLIP): string {
	const flat = text.replace(/\s+/g, " ").trim();
	return flat.length > limit ? `${flat.slice(0, limit)}…` : flat;
}

/** What a tool call is *for*, in one line: the code's first statement, the
 * skill's name, the track being opened. */
function purpose(name: string, input: Record<string, any>): string {
	if (name.endsWith("python")) {
		const code = String(input.code ?? "");
		const lines = code.split("\n").filter((line) => line.trim() && !line.trim().startsWith("#"));
		const head = oneLine(lines[0] ?? "", 140);
		return lines.length > 1 ? `${head}  (+${lines.length - 1} lines)` : head;
	}
	return oneLine(JSON.stringify(input), 140);
}

type Usage = { turns: number; costUsd: number; result: string };

/** Print the agent's stream as it arrives; answer with the run's totals. */
async function trace(stream: NodeJS.ReadableStream): Promise<Usage> {
	const calls = new Map<string, string>();
	let usage: Usage = { turns: 0, costUsd: 0, result: "" };
	for await (const line of createInterface({ input: stream })) {
		if (!line.trim()) continue;
		let event: any;
		try {
			event = JSON.parse(line);
		} catch {
			process.stdout.write(`${line}\n`);
			continue;
		}
		switch (event.type) {
			case "system":
				if (event.subtype === "init") {
					console.log(`[session ${event.session_id}] tools: ${(event.tools ?? []).join(", ")}`);
					const servers = (event.mcp_servers ?? [])
						.map((s: any) => `${s.name}=${s.status}`)
						.join(", ");
					console.log(`[mcp] ${servers || "none"}`);
				}
				break;
			case "assistant":
				for (const block of event.message?.content ?? []) {
					if (block.type === "text" && block.text.trim()) {
						console.log(`\n  ${oneLine(block.text, 600)}`);
					}
					if (block.type === "tool_use") {
						const short = block.name.replace(/^mcp__luma__/, "luma.");
						calls.set(block.id, short);
						console.log(`\n→ ${short}  ${purpose(block.name, block.input ?? {})}`);
					}
				}
				break;
			case "user":
				for (const block of event.message?.content ?? []) {
					if (block.type !== "tool_result") continue;
					const name = calls.get(block.tool_use_id) ?? "tool";
					const blocks = Array.isArray(block.content) ? block.content : [];
					const text = blocks
						.filter((b: any) => b.type === "text")
						.map((b: any) => b.text)
						.join("\n");
					const images = blocks.filter((b: any) => b.type === "image").length;
					const mark = block.is_error ? "✗" : "←";
					const body = typeof block.content === "string" ? block.content : text;
					console.log(
						`${mark} ${name}  ${oneLine(body) || "(no text)"}${images ? `  [${images} image]` : ""}`,
					);
				}
				break;
			case "result":
				usage = {
					turns: event.num_turns ?? 0,
					costUsd: event.total_cost_usd ?? 0,
					result: event.result ?? event.subtype ?? "",
				};
				break;
		}
	}
	return usage;
}

// -----------------------------------------------------------------------------
// The run
// -----------------------------------------------------------------------------

const options = parseArgs(process.argv.slice(2));
const started = Date.now();

const binding = await resolveBinding(options);
console.log(`track ${binding.trackId}  ${binding.label}`);
console.log(`venue ${binding.venueId}  ${binding.venueName}`);

// `--strict-mcp-config` means this file is the agent's whole MCP world, so it
// names exactly one server: the same binary, against the same library, this
// script just resolved the pair with.
const mcpConfig = join(
	process.env.TMPDIR ?? "/tmp",
	`luma-mcp-config-${process.pid}.json`,
);
writeFileSync(
	mcpConfig,
	JSON.stringify({
		mcpServers: {
			luma: { command: mcpBinary(), args: mcpArgs(options.host), cwd: REPO_ROOT },
		},
	}),
);

const argv = [
	"-p",
	brief(binding, options.model),
	"--system-prompt",
	readFileSync(TRACK_PROMPT, "utf8"),
	"--mcp-config",
	mcpConfig,
	"--strict-mcp-config",
	"--allowedTools",
	"mcp__luma__find,mcp__luma__open,mcp__luma__python,mcp__luma__skill,mcp__luma__reset,mcp__luma__cancel",
	// Luma is the entire tool surface: no Bash, no Read, no Edit.
	"--tools",
	"",
	"--permission-mode",
	"bypassPermissions",
	"--model",
	options.model,
	"--max-turns",
	String(options.maxTurns),
	"--output-format",
	"stream-json",
	"--verbose",
];
console.log(`\n$ claude -p <brief> --model ${options.model} --max-turns ${options.maxTurns}\n`);

const child = spawn("claude", argv, {
	stdio: ["ignore", "pipe", "inherit"],
	cwd: REPO_ROOT,
});
const usage = await trace(child.stdout as NodeJS.ReadableStream);
const code = await new Promise<number>((res) => child.on("exit", (c) => res(c ?? 1)));

const wall = ((Date.now() - started) / 1000).toFixed(1);
console.log(`\n${"─".repeat(72)}\n${usage.result}\n${"─".repeat(72)}`);
console.log(
	`${usage.turns} turns, $${usage.costUsd.toFixed(2)}, ${wall}s wall — score for ` +
		`${binding.label} in ${binding.venueName}`,
);
process.exit(code);
