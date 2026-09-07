/** Resolve a score job and launch Luma's shared Rust agent runtime. */
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { fetchClaudeUsage } from "./claude-usage";
import { fetchCodexUsage } from "./codex-usage";
import { summarizeUsage, untilReset } from "./usage";
import { mcpArgs, type McpServerOptions, REAL_CACHE_DIR, startMcpServer, textOf } from "./mcp-client";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const RUNNERS = { claude: { usage: fetchClaudeUsage }, codex: { usage: fetchCodexUsage } };
type RunnerName = keyof typeof RUNNERS;
const EX_TEMPFAIL = 75;

type Options = {
	track: string;
	venue: string;
	runner: RunnerName;
	model: string;
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
				"[--runner claude|codex] [--model M] [--max-weekly F] " +
				"[--skip-usage-check] [--config-dir D] [--fixture-principal P]\n" +
				"       bun run scripts/headless/author_score.ts --usage-only",
		);
	}
	return {
		track,
		venue,
		runner,
		model,
		maxWeekly,
		skipUsageCheck,
		usageOnly,
		host,
	};
}

type Binding = { label: string; trackId: string; venueId: string; venueName: string };

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

async function gate(runner: typeof RUNNERS[RunnerName]): Promise<void> {
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

const options = parseArgs(process.argv.slice(2));
const runner = RUNNERS[options.runner];
if (options.usageOnly) {
    console.log(summarizeUsage(await runner.usage()));
    process.exit(0);
}
const binary = process.env.LUMA_AGENT_BIN ?? join(REPO_ROOT, "src-tauri/target/debug/luma-agent");
if (!existsSync(binary)) throw new Error(`Build luma-agent first: cargo +1.97.1 build --manifest-path src-tauri/Cargo.toml --bin luma-agent`);
if (!options.skipUsageCheck) await gate(runner);
const binding = await resolveBinding(options);
const prompt = [
    `Author a complete lighting score for ${binding.label} in ${binding.venueName}.`,
    "Listen first: establish sections, phrases and drops using luma.features and luma.audio.",
    "Load the relevant genre and craft skills. Learn the rig from luma.venue.",
    "Author coherent sections with luma.track.edit(), checking and diffing before applying.",
    "Cover the whole track. Verify two or three contrasting moments with luma.venue.render().",
    "Finish with a brief description of the show and counts of clips, sections and groups.",
].join("\n");
const args = [...mcpArgs(options.host), "--track", binding.trackId, "--venue", binding.venueId,
    "--engine", options.runner, "--prompt", prompt];
if (options.model) args.push("--model", options.model);
if (!options.host.fixturePrincipal) args.push("--sync");
const child = spawn(binary, args, { stdio: "inherit", cwd: REPO_ROOT });
const code = await new Promise<number>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolve(code ?? 1));
    process.once("SIGINT", () => child.kill("SIGINT"));
    process.once("SIGTERM", () => child.kill("SIGTERM"));
});
if (!options.skipUsageCheck) {
    try { console.error(`${options.runner} usage: ${summarizeUsage(await runner.usage())}`); }
    catch (error) { console.error(`usage unavailable: ${error}`); }
}
process.exit(code);
