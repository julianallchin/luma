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
 * MCP is its only tool surface (`--strict-mcp-config`, every built-in off but
 * `Agent`), so the stream-json trace this prints is a complete record of what it
 * did — the lead's turns and its children's, which `--forward-subagent-text`
 * folds into the same stream and [`trace`] indents apart.
 *
 * Writes to the real library by default. Every run authors a *new* score
 * (`open`'s `new_score`) rather than continuing the last one, so a second run
 * over the same track is a second reading of it, not an edit of the first; the
 * last line prints the id. Pass `--config-dir` (with `--fixture-principal`) to
 * work against a scratch copy instead, exactly as `mcp_smoke.ts` does.
 *
 * ## The subscription gate
 *
 * An authoring run is long and expensive in subscription quota, and a run that
 * dies on a rate limit halfway through leaves a half-authored score behind. So
 * the weekly window is checked *before* anything is spawned, and again after,
 * and a limit reached mid-run is recognised in the stream.
 *
 * Any of those paths exits **75** (`EX_TEMPFAIL`): the job did not fail, it ran
 * out of quota, and a caller that retries on a schedule can tell the two apart
 * from the exit code alone. Nothing here retries, waits or polls — 75 is the
 * whole answer. `--max-weekly` moves the threshold, `--skip-usage-check` drops
 * the pre-flight, and `--usage-only` prints the summary and exits.
 */

import { type ChildProcess, spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { fetchClaudeUsage } from "./claude-usage";
import { fetchCodexUsage } from "./codex-usage";
import { type PlanUsage, summarizeUsage, untilReset } from "./usage";
import {
	mcpArgs,
	mcpBinary,
	type McpServerOptions,
	REAL_CACHE_DIR,
	recordUsage,
	startMcpServer,
	textOf,
	type ThreadUsage,
} from "./mcp-client";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const TRACK_PROMPT = join(REPO_ROOT, "src-tauri/src/agent/prompts/track.md");

/** The one agent type a run may fan out to. See [`sectionAgent`]. */
const SECTION_AGENT = "section";

/**
 * The child agent, defined rather than left to Claude Code's own catalogue.
 *
 * Two reasons it has to be declared, and both are about the shared session.
 * `luma-mcp` holds *one* open workspace per process — one thread, one kernel —
 * so a child that called `open` or `reset` would silently steal the parent's,
 * and a general-purpose child comes with the tools to do exactly that. Naming
 * the child's tools takes those away: it can run cells and read playbooks in
 * the workspace the lead opened, and it cannot rebind it.
 *
 * It also carries `track.md`, the same craft the lead has. A child that only
 * knew its bar range would author competently and off-taste.
 */
function sectionAgent(): string {
	return JSON.stringify({
		[SECTION_AGENT]: {
			description:
				"Author one section of the show, deeply, in the Luma workspace the lead already " +
				"opened. Give it the bar range, the arc decisions it must honour, and what its " +
				"neighbours do at the boundaries.",
			tools: ["mcp__luma__python", "mcp__luma__skill", "mcp__luma__find"],
			prompt: [
				readFileSync(TRACK_PROMPT, "utf8"),
				"",
				"## This session",
				"",
				"You are a section author. The workspace is already open and shared with the lead" +
					" and your siblings: `luma` is bound, the kernel keeps its variables between" +
					" cells, and there is exactly one of it. Stay inside the bar range you were" +
					" given, and `apply()` only what belongs to it.",
				"",
				"Report back in a few lines: what you authored, the bars it covers, and anything" +
					" the lead must fix at your boundaries.",
			].join("\n"),
		},
	});
}

// -----------------------------------------------------------------------------
// Arguments
// -----------------------------------------------------------------------------

type Options = {
	track: string;
	venue: string;
	/** Which CLI drives the run. See [`RUNNERS`]. */
	runner: RunnerName;
	model: string;
	maxTurns: number;
	/** Refuse to start when the weekly window is at least this spent, 0–1. */
	maxWeekly: number;
	skipUsageCheck: boolean;
	usageOnly: boolean;
	host: McpServerOptions;
};

function parseArgs(argv: string[]): Options {
	const positional: string[] = [];
	const host: McpServerOptions = { cacheDir: REAL_CACHE_DIR };
	let runner: RunnerName = "claude";
	let model = "";
	let maxTurns = 40;
	let maxWeekly = 0.5;
	let skipUsageCheck = false;
	let usageOnly = false;
	for (let i = 0; i < argv.length; i++) {
		const flag = argv[i];
		const value = () => {
			const next = argv[++i];
			if (next === undefined) throw new Error(`${flag} requires a value`);
			return next;
		};
		switch (flag) {
			case "--runner": {
				const named = value();
				if (!(named in RUNNERS)) {
					throw new Error(`unknown runner '${named}' — one of ${Object.keys(RUNNERS).join(", ")}`);
				}
				runner = named as RunnerName;
				break;
			}
			case "--model":
				model = value();
				break;
			case "--max-turns":
				maxTurns = Number(value());
				break;
			case "--max-weekly":
				maxWeekly = Number(value());
				break;
			case "--skip-usage-check":
				skipUsageCheck = true;
				break;
			case "--usage-only":
				usageOnly = true;
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
	const [track = "", venue = ""] = positional;
	if (!usageOnly && (!track || !venue)) {
		throw new Error(
			"usage: bun run scripts/headless/author_score.ts <track-id-or-title> <venue-id-or-name> " +
				"[--runner claude|codex] [--model M] [--max-turns N] [--max-weekly F] " +
				"[--skip-usage-check] [--config-dir D] [--fixture-principal P]\n" +
				"       bun run scripts/headless/author_score.ts --usage-only",
		);
	}
	return {
		track,
		venue,
		runner,
		model: model || RUNNERS[runner].defaultModel,
		maxTurns,
		maxWeekly,
		skipUsageCheck,
		usageOnly,
		host,
	};
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
function brief(binding: Binding, model: string, fanout: string[]): string {
	return [
		`Author a lighting score for **${binding.label}** in the venue **${binding.venueName}**.`,
		"",
		"1. `open` the workspace: " +
			`\`track_id: "${binding.trackId}"\`, \`venue_id: "${binding.venueId}"\`, ` +
			`\`model: "${model}"\`, \`new_score: true\`. The reply is your API reference — read it.`,
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
		...fanout,
	].join("\n");
}

/**
 * The fan-out clause, for a harness that has one.
 *
 * Not part of [`brief`] because it is the one paragraph of the job that is
 * about the *harness* rather than about the show: a runner with no way to
 * declare a restricted child agent must not be told to spawn one, and a brief
 * that described a facility the CLI does not have would be instructions to
 * hallucinate.
 */
const CLAUDE_FANOUT = [
	"",
	`## Fanning out\n\nGo deep with the \`${SECTION_AGENT}\` subagent — it is the only agent` +
		" type there is, and it comes up already knowing the craft. Follow the Subagents" +
		" contract in your instructions, and mind the one thing this harness adds: every" +
		" agent here shares **one** Luma session and **one** Python kernel, so",
	`- spawn \`${SECTION_AGENT}\` children **one at a time** — two cells at once is an error,` +
		" not a race;",
	"- a child sees the workspace you opened, and the variables you left in it;",
	"- children cannot `open` or `reset`, so the session stays yours.",
];

/**
 * Codex's fan-out clause. Its `multi_agent` children are clones — there is no
 * way to declare a role with fewer tools — so the one rule the Claude harness
 * enforces with a tool list is stated here as an instruction instead, and
 * that is the whole difference between the two clauses.
 */
const CODEX_FANOUT = [
	"",
	"## Fanning out\n\nGo deep by spawning child agents (`spawn_agent`) — they come up" +
		" already knowing the craft. Follow the Subagents contract in your instructions, and" +
		" mind what this harness adds: every agent here shares **one** Luma session and" +
		" **one** Python kernel, so",
	"- spawn children **one at a time** and wait for each — two cells at once is an error," +
		" not a race;",
	"- a child sees the workspace you opened, and the variables you left in it;",
	"- children must **never** call `open` or `reset` — say so in every child's prompt. The" +
		" session is yours; a child that reopens it discards the score.",
];

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

type RunTotals = {
	turns: number;
	costUsd: number;
	result: string;
	/** The run ended on a subscription limit, not on its own terms. */
	limited: boolean;
	/** How many children the lead fanned out to. */
	subagents: number;
	/** The score the run authored, read off `open`'s reply — the one id worth
	 * having when the run is over, and only the agent knows it. */
	scoreId: string;
	/** The session's agent thread, also off `open`'s reply. It is what the
	 * run's cost is filed against, and what joins that cost back to the score:
	 * every revision the session wrote carries it. */
	threadId: string;
	/** Tokens, from the `result` event. Anthropic's four non-overlapping
	 * counts, which is the ledger's convention too. */
	inputTokens: number;
	outputTokens: number;
	cacheCreationTokens: number;
	cacheReadTokens: number;
	/** The harness's own wall clock for the run. */
	durationMs: number;
	/** Which model actually answered, from `modelUsage` — the flag asked for
	 * an alias (`opus`), and this is what it resolved to. */
	model: string;
};

/** Whether a message from the harness is the rate limiter talking. */
function looksLimited(text: string): boolean {
	return /usage limit|rate limit|limit reached|quota exceeded/i.test(text);
}

/** A run that has not reported anything yet. */
function noTotals(): RunTotals {
	return {
		turns: 0,
		costUsd: 0,
		result: "",
		limited: false,
		subagents: 0,
		scoreId: "",
		threadId: "",
		inputTokens: 0,
		outputTokens: 0,
		cacheCreationTokens: 0,
		cacheReadTokens: 0,
		durationMs: 0,
		model: "",
	};
}

/**
 * The two ids `open` answers with, taken off its reply text.
 *
 * Shared by every runner because it is the *server's* format, not any CLI's:
 * `open` prints them, and a second copy of these patterns would be a second
 * thing to forget when that reply changes.
 */
function readOpenReply(body: string, into: RunTotals): void {
	into.scoreId = body.match(/venue \S+, score (\S+)/)?.[1] ?? into.scoreId;
	into.threadId = body.match(/thread ([0-9a-f-]{36})/)?.[1] ?? into.threadId;
}

/**
 * Print the agent's stream as it arrives; answer with the run's totals.
 *
 * The lead and its children share one stream, told apart only by
 * `parent_tool_use_id`. Every child line is indented under a label minted from
 * the `Agent` call that spawned it, so a fan-out reads as a fan-out rather than
 * as one agent that suddenly forgot what it was doing.
 */
async function traceClaude(stream: NodeJS.ReadableStream): Promise<RunTotals> {
	const calls = new Map<string, string>();
	const children = new Map<string, string>();
	let usage: RunTotals = noTotals();
	for await (const line of createInterface({ input: stream })) {
		if (!line.trim()) continue;
		let event: any;
		try {
			event = JSON.parse(line);
		} catch {
			process.stdout.write(`${line}\n`);
			continue;
		}
		const child = event.parent_tool_use_id
			? `  ${children.get(event.parent_tool_use_id) ?? "child"} │ `
			: "";
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
						console.log(`\n${child}  ${oneLine(block.text, 600)}`);
					}
					if (block.type === "tool_use") {
						const short = block.name.replace(/^mcp__luma__/, "luma.");
						calls.set(block.id, short);
						if (block.name === "Agent") {
							usage.subagents += 1;
							children.set(
								block.id,
								`${block.input?.subagent_type ?? "agent"}#${usage.subagents}`,
							);
						}
						console.log(`\n${child}→ ${short}  ${purpose(block.name, block.input ?? {})}`);
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
					if (name === "luma.open") readOpenReply(body, usage);
					console.log(
						`${child}${mark} ${name}  ${oneLine(body) || "(no text)"}` +
							`${images ? `  [${images} image]` : ""}`,
					);
				}
				break;
			case "result": {
				const result = String(event.result ?? event.subtype ?? "");
				const tokens = event.usage ?? {};
				// `modelUsage` is keyed by the real model id; `--model opus` is an
				// alias, so this is the only place the run says what answered.
				const models = Object.keys(event.modelUsage ?? {});
				usage = {
					...usage,
					turns: event.num_turns ?? 0,
					costUsd: event.total_cost_usd ?? 0,
					result,
					inputTokens: tokens.input_tokens ?? 0,
					outputTokens: tokens.output_tokens ?? 0,
					cacheCreationTokens: tokens.cache_creation_input_tokens ?? 0,
					cacheReadTokens: tokens.cache_read_input_tokens ?? 0,
					durationMs: event.duration_ms ?? 0,
					model: models.join("+"),
					// A limit arrives as an errored result whose text names it, not
					// as a subtype of its own — so match the text.
					limited: Boolean(event.is_error) && looksLimited(`${result} ${event.subtype ?? ""}`),
				};
				// The accounting the totals line summarises, kept verbatim: a
				// per-model breakdown is the only way to read a run that fell
				// back, and it is gone once this process exits.
				console.log(`\n[usage] ${JSON.stringify(tokens)}`);
				console.log(`[modelUsage] ${JSON.stringify(event.modelUsage ?? {})}`);
				console.log(
					`[timing] duration_ms=${event.duration_ms ?? 0} api_ms=${event.duration_api_ms ?? 0}`,
				);
				break;
			}
			case "error": {
				const message = String(event.error?.message ?? event.message ?? event.subtype ?? "");
				console.log(`\n${child}✗ ${oneLine(message)}`);
				if (looksLimited(message)) usage.limited = true;
				break;
			}
		}
	}
	return usage;
}

// -----------------------------------------------------------------------------
// Runners
// -----------------------------------------------------------------------------

/**
 * A coding-agent CLI, reduced to the two things this script needs from one:
 * start it against Luma's MCP server, and read its stream.
 *
 * Everything either side of those two calls is the same for every runner — the
 * track/venue lookup, `new_score`, the brief, the cost record — because none of
 * it is about which model is driving. What differs is genuinely per-CLI: the
 * flags that pin the tool surface to `luma` alone, the event schema, and
 * whether a run is paid for out of a subscription window this script can check.
 */
type RunnerName = "claude" | "codex";

type Job = {
	brief: string;
	/** `track.md`. Delivered as a system prompt by a CLI that has one, and
	 * folded into the prompt by a CLI that does not. */
	systemPrompt: string;
	model: string;
	maxTurns: number;
	host: McpServerOptions;
};

type Runner = {
	/** The model flag's value when the caller names none. */
	defaultModel: string;
	/** The plan this CLI draws on, as [`gate`] reads it. Each vendor's windows
	 * come from its own endpoint; the gate only ever sees [`PlanUsage`]. */
	usage: () => Promise<PlanUsage>;
	/** The brief's fan-out clause. Empty for a CLI that cannot declare a child
	 * agent with a restricted tool list — see [`RUNNERS`]. */
	fanout: string[];
	spawn: (job: Job) => ChildProcess;
	trace: (stream: NodeJS.ReadableStream) => Promise<RunTotals>;
};

/** `--mcp-config`'s file, named for this process so two runs never share one. */
function mcpConfigFile(host: McpServerOptions): string {
	const path = join(process.env.TMPDIR ?? "/tmp", `luma-mcp-config-${process.pid}.json`);
	writeFileSync(
		path,
		JSON.stringify({
			mcpServers: { luma: { command: mcpBinary(), args: mcpArgs(host), cwd: REPO_ROOT } },
		}),
	);
	return path;
}

/**
 * Codex's `-c` overrides for the same server.
 *
 * Paired with `--ignore-user-config`, which is Codex's `--strict-mcp-config`:
 * the user's `config.toml` — and every MCP server in it — is not read, so the
 * only server the model sees is the one these overrides declare. It also steps
 * around a real config in the wild that 0.128 refuses to parse, while leaving
 * `$CODEX_HOME`'s credentials in play, which is the half that must survive.
 */
function codexServerOverrides(host: McpServerOptions): string[] {
	return [
		"-c",
		`mcp_servers.luma.command=${JSON.stringify(mcpBinary())}`,
		"-c",
		`mcp_servers.luma.args=${JSON.stringify(mcpArgs(host))}`,
		"-c",
		`mcp_servers.luma.cwd=${JSON.stringify(REPO_ROOT)}`,
		// `open` loads a venue and `python` runs unbounded cells; the 60s default
		// would cut both.
		"-c",
		"mcp_servers.luma.tool_timeout_sec=600",
	];
}

/**
 * Codex's JSONL, folded into the same totals Claude's stream produces.
 *
 * Two mappings are worth naming. Codex reports OpenAI's token convention, where
 * `input_tokens` *already counts* the cached prefix — the ledger and
 * `agent::model::Usage` use Anthropic's, where the counts do not overlap — so
 * the cached half is subtracted out here, the same correction
 * `agent::model::openrouter` makes on the way in. And Codex reports no price at
 * all, so `costUsd` stays 0 and the record files `null` rather than a guess.
 */
async function traceCodex(stream: NodeJS.ReadableStream): Promise<RunTotals> {
	const usage = noTotals();
	const started = Date.now();
	for await (const line of createInterface({ input: stream })) {
		if (!line.trim()) continue;
		let event: any;
		try {
			event = JSON.parse(line);
		} catch {
			// Codex interleaves tracing lines with its JSONL; pass them through
			// rather than swallowing what may be the reason a run died.
			process.stdout.write(`${line}\n`);
			continue;
		}
		switch (event.type) {
			case "thread.started":
				console.log(`[session ${event.thread_id}]`);
				break;
			case "item.completed": {
				const item = event.item ?? {};
				// 0.128 called the discriminator `item_type`; 0.150 calls it `type`.
				const kind = item.item_type ?? item.type;
				// Every action the model took is a turn in the sense the totals
				// line means: Codex has no `num_turns`, and counting only its
				// messages would make a tool-heavy run look idle.
				if (kind !== "reasoning" && kind !== "todo_list") {
					usage.turns += 1;
				}
				if (kind === "agent_message") {
					console.log(`\n  ${oneLine(String(item.text ?? ""), 600)}`);
					usage.result = String(item.text ?? usage.result);
				} else if (kind === "mcp_tool_call") {
					const short = `${item.server ?? "mcp"}.${item.tool ?? "?"}`;
					const body = mcpResultText(item);
					if (item.tool === "open") readOpenReply(body, usage);
					console.log(`\n→ ${short}  ${purpose(String(item.tool ?? ""), item.arguments ?? {})}`);
					console.log(
						`${item.status === "failed" ? "✗" : "←"} ${short}  ${oneLine(body) || "(no text)"}`,
					);
				} else if (kind !== "reasoning") {
					// Codex has no dedicated item for a spawned child; the spawn is a
					// tool call whose name says so.
					if (/spawn_agent/.test(`${kind} ${item.tool ?? item.name ?? ""}`)) {
						usage.subagents += 1;
					}
					console.log(`\n· ${kind}  ${oneLine(JSON.stringify(item), 300)}`);
				}
				break;
			}
			case "turn.completed": {
				const tokens = event.usage ?? {};
				const cached = tokens.cached_input_tokens ?? 0;
				usage.inputTokens += Math.max(0, (tokens.input_tokens ?? 0) - cached);
				usage.cacheReadTokens += cached;
				usage.outputTokens += tokens.output_tokens ?? 0;
				console.log(`\n[usage] ${JSON.stringify(tokens)}`);
				break;
			}
			case "turn.failed":
			case "error": {
				const message = String(event.error?.message ?? event.message ?? "");
				console.log(`\n✗ ${oneLine(message)}`);
				usage.result = message;
				if (looksLimited(message)) usage.limited = true;
				break;
			}
		}
	}
	usage.durationMs = Date.now() - started;
	return usage;
}

/** The text of an `mcp_tool_call` item's result, however Codex wrapped it. */
function mcpResultText(item: any): string {
	const content = item.result?.content;
	if (Array.isArray(content)) {
		return content
			.filter((block: any) => block?.type === "text")
			.map((block: any) => block.text ?? "")
			.join("\n");
	}
	return typeof item.result === "string" ? item.result : JSON.stringify(item.result ?? {});
}

/**
 * The two CLIs, and the one asymmetry between them worth knowing.
 *
 * Claude Code can *declare* a child agent — a name, a prompt, and the exact
 * tools it may call — which is what makes fanning out safe here: the children
 * share one Luma session, and a child that could call `open` would silently
 * rebind its parent's. Codex has a multi-agent facility but no way to restrict
 * a child's tools from the command line, so the codex runner does not fan out
 * and its brief does not mention children.
 */
const RUNNERS: Record<RunnerName, Runner> = {
	claude: {
		defaultModel: "opus",
		usage: fetchClaudeUsage,
		fanout: CLAUDE_FANOUT,
		spawn: (job) =>
			spawn(
				"claude",
				[
					"-p",
					job.brief,
					"--system-prompt",
					job.systemPrompt,
					"--mcp-config",
					mcpConfigFile(job.host),
					"--strict-mcp-config",
					"--allowedTools",
					"Agent,mcp__luma__find,mcp__luma__open,mcp__luma__python,mcp__luma__skill," +
						"mcp__luma__reset,mcp__luma__cancel",
					// Luma is the entire tool surface, plus the one built-in that is
					// not a way to touch the machine: no Bash, no Read, no Edit —
					// only `Agent`, so the lead can fan out the way `track.md` tells
					// it to.
					"--tools",
					"Agent",
					"--agents",
					sectionAgent(),
					// Children are the point of fanning out; a trace that showed only
					// the lead would hide most of the run.
					"--forward-subagent-text",
					"--permission-mode",
					"bypassPermissions",
					"--model",
					job.model,
					"--max-turns",
					String(job.maxTurns),
					"--output-format",
					"stream-json",
					"--verbose",
				],
				{ stdio: ["ignore", "pipe", "inherit"], cwd: REPO_ROOT },
			),
		trace: traceClaude,
	},
	codex: {
		// ChatGPT-plan logins get a plan-gated model list; bare `gpt-5` is
		// refused with a 400 there, so the default is the plan's frontier slug.
		defaultModel: "gpt-5.6-sol",
		usage: fetchCodexUsage,
		fanout: CODEX_FANOUT,
		spawn: (job) =>
			spawn(
				"codex",
				[
					"exec",
					"--json",
					// See `codexServerOverrides`: this is the strict-config half.
					"--ignore-user-config",
					// The agent never touches the tree; it is here for `cwd`, not
					// for the repo, and a git check would only be a way to fail.
					"--skip-git-repo-check",
					// Shell commands are not part of the job — the whole surface is
					// MCP — but Codex gates MCP calls behind an approval prompt that
					// `exec` has no one to answer (it reports the auto-denial as "user
					// cancelled"), and neither `approval_policy` nor `--full-auto`
					// lifts it. Only the bypass does, so the sandbox goes with it;
					// the trade is accepted because the job never touches the tree.
					"--dangerously-bypass-approvals-and-sandbox",
					// `--ignore-user-config` drops the user's feature table too; the
					// fan-out clause promises children, so the feature is pinned on.
					"--enable",
					"multi_agent",
					"-m",
					job.model,
					...codexServerOverrides(job.host),
					// No `--system-prompt`: `codex exec` has none, and overriding its
					// base instructions would take Codex's own tool scaffolding with
					// it, so the craft is prepended to the brief instead.
					`${job.systemPrompt}\n\n---\n\n${job.brief}`,
				],
				{ stdio: ["ignore", "pipe", "inherit"], cwd: REPO_ROOT },
			),
		trace: traceCodex,
	},
};

// -----------------------------------------------------------------------------
// The run
// -----------------------------------------------------------------------------

const options = parseArgs(process.argv.slice(2));
const started = Date.now();

/** `EX_TEMPFAIL` — "not now, try later". The one exit code that means quota. */
const EX_TEMPFAIL = 75;

/**
 * Refuse to start a long run that the weekly window cannot pay for.
 *
 * Checked before the binding resolves, so a gated invocation touches neither
 * the library nor the agent. It never waits: the caller owns the schedule, and
 * a script that slept for four hours holding a lease would be worse than one
 * that exits and is run again.
 */
async function gate(runner: Runner): Promise<void> {
	const usage = await runner.usage();
	console.log(`${options.runner} usage: ${summarizeUsage(usage)}`);
	const blocker =
		usage.weekly && usage.weekly.usedFraction >= options.maxWeekly
			? ({ label: "weekly", window: usage.weekly, threshold: options.maxWeekly } as const)
			: usage.short && usage.short.usedFraction >= 1
				? ({ label: "short", window: usage.short, threshold: 1 } as const)
				: null;
	if (blocker) {
		const pct = (n: number) => `${(n * 100).toFixed(0)}%`;
		console.error(
			`\nnot starting: ${blocker.label} window is at ${pct(blocker.window.usedFraction)} ` +
				`(limit ${pct(blocker.threshold)}), resets ${untilReset(blocker.window.resetsAt)}` +
				`${blocker.window.resetsAt ? ` (${blocker.window.resetsAt.toISOString()})` : ""}.` +
				"\nRe-run after the reset, or pass --max-weekly / --skip-usage-check.",
		);
		process.exit(EX_TEMPFAIL);
	}
}

const runner = RUNNERS[options.runner];

if (options.usageOnly) {
	console.log(summarizeUsage(await runner.usage()));
	process.exit(0);
}
if (!options.skipUsageCheck) await gate(runner);

const binding = await resolveBinding(options);
console.log(`track ${binding.trackId}  ${binding.label}`);
console.log(`venue ${binding.venueId}  ${binding.venueName}`);

const child = runner.spawn({
	brief: brief(binding, options.model, runner.fanout),
	systemPrompt: readFileSync(TRACK_PROMPT, "utf8"),
	model: options.model,
	maxTurns: options.maxTurns,
	host: options.host,
});
const usage = await runner.trace(child.stdout as NodeJS.ReadableStream);
const code = await new Promise<number>((res) => child.on("exit", (c) => res(c ?? 1)));

const wall = ((Date.now() - started) / 1000).toFixed(1);
console.log(`\n${"─".repeat(72)}\n${usage.result}\n${"─".repeat(72)}`);
console.log(
	`${usage.turns} turns, ${usage.subagents} subagents, $${usage.costUsd.toFixed(2)}, ` +
		`${wall}s wall — score for ${binding.label} in ${binding.venueName}`,
);

// What the run actually cost in quota terms, from the same source the gate read.
if (!options.skipUsageCheck) {
	try {
		console.log(`${options.runner} usage: ${summarizeUsage(await runner.usage())}`);
	} catch (error) {
		console.log(`${options.runner} usage: unavailable (${(error as Error).message})`);
	}
}

// File the receipt. Against the thread, not the score: a thread is one run and
// a score can be authored by several, and the join back — `authored_revisions`
// keeps the thread id of every revision — is already there.
if (usage.threadId) {
	const record: ThreadUsage = {
		threadId: usage.threadId,
		model: usage.model || options.model,
		turns: usage.turns,
		inputTokens: usage.inputTokens,
		outputTokens: usage.outputTokens,
		cacheCreationTokens: usage.cacheCreationTokens,
		cacheReadTokens: usage.cacheReadTokens,
		costUsd: usage.costUsd || null,
		durationMs: usage.durationMs,
		subagents: usage.subagents,
	};
	console.log(await recordUsage(options.host, record));
}

// Last line, and the only one a caller has to keep: the run authored a score
// that did not exist before it started, and nothing outside the stream knows it.
console.log(`score ${usage.scoreId || "unknown — open never answered"}`);

if (usage.limited) {
	console.error("\nended on a Claude subscription limit — the score may be half-authored.");
	process.exit(EX_TEMPFAIL);
}
process.exit(code);
