#!/usr/bin/env node
/**
 * Lift perf-baseline dumps out of the app's render-telemetry log into
 * `harness/perf/<name>.json`, and print the summary table.
 *
 * The webview writes dumps via the existing `append_render_telemetry` command
 * (see src/shared/lib/perf-baseline.ts), so they land as JSONL lines in the app
 * config dir. This script is the only thing that moves them into the repo.
 *
 *   node harness/perf/extract-baseline.mjs             # newest dump
 *   node harness/perf/extract-baseline.mjs --all       # every dump in the log
 *   node harness/perf/extract-baseline.mjs --list      # don't write, just list
 *   node harness/perf/extract-baseline.mjs --name web-v0.5.19
 */

import { createReadStream } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const IDENTIFIER = "com.luma.luma";
const LOG_NAME = "render-telemetry.log";
const OUT_DIR = path.dirname(fileURLToPath(import.meta.url));

function logDir() {
	if (process.platform === "darwin")
		return path.join(homedir(), "Library", "Application Support", IDENTIFIER);
	if (process.platform === "win32")
		return path.join(
			process.env.APPDATA ?? path.join(homedir(), "AppData", "Roaming"),
			IDENTIFIER,
		);
	return path.join(
		process.env.XDG_CONFIG_HOME ?? path.join(homedir(), ".config"),
		IDENTIFIER,
	);
}

async function readDumps(file) {
	const dumps = [];
	const rl = createInterface({
		input: createReadStream(file),
		crlfDelay: Number.POSITIVE_INFINITY,
	});
	for await (const line of rl) {
		if (!line.includes("perf-baseline-dump")) continue;
		let parsed;
		try {
			parsed = JSON.parse(line);
		} catch {
			continue;
		}
		if (parsed?.entry?.event !== "perf-baseline-dump") continue;
		dumps.push({ ts: parsed.ts, payload: parsed.entry.data });
	}
	return dumps;
}

function printSummary(payload) {
	const hz = payload.segments[0]?.estimatedRefreshHz ?? 60;
	console.log(
		`\n${payload.appVersion ?? "?"} · ${payload.build} · ${hz}Hz · dpr ${payload.devicePixelRatio}`,
	);
	console.table(
		payload.segments.map((s) => ({
			label: s.label,
			s: Math.round(s.durationMs / 1000),
			fps: s.frames.fps,
			p50: s.frames.p50Ms,
			p95: s.frames.p95Ms,
			p99: s.frames.p99Ms,
			worst: s.frames.maxMs,
			over2x: s.frames.overBudget.x2,
			jank: s.jank.count,
			inputP95: s.input.toPaint?.p95Ms ?? null,
		})),
	);
}

const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const value = (name) => {
	const i = args.indexOf(name);
	return i === -1 ? null : args[i + 1];
};

const file = value("--log") ?? path.join(logDir(), LOG_NAME);
let dumps;
try {
	dumps = await readDumps(file);
} catch (err) {
	console.error(`could not read ${file}: ${err.message}`);
	process.exit(1);
}

if (dumps.length === 0) {
	console.error(
		`no perf-baseline dumps in ${file}\n` +
			"did you run __lumaPerf.dump() in the app's web inspector?",
	);
	process.exit(1);
}

const selected = flag("--all") ? dumps : [dumps[dumps.length - 1]];

if (flag("--list")) {
	for (const [i, d] of dumps.entries())
		console.log(
			`${i}\t${d.ts}\t${d.payload.segments.length} segments\t${d.payload.appVersion ?? "?"}`,
		);
	process.exit(0);
}

await mkdir(OUT_DIR, { recursive: true });
for (const dump of selected) {
	const stamp = dump.ts.replace(/[:.]/g, "-").replace(/Z$/, "");
	const name = value("--name") ?? `baseline-${stamp}`;
	const out = path.join(OUT_DIR, `${name}.json`);
	await writeFile(out, `${JSON.stringify(dump.payload, null, 2)}\n`);
	printSummary(dump.payload);
	console.log(`\nwrote harness/perf/${name}.json`);
}
