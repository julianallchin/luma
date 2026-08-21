#!/usr/bin/env node
// Capture the pattern-graph canvas reference shots — the visual quality bar the
// GPUI graph editor has to hit.
//
//   node harness/gauntlet/shot-graph.mjs --all
//   node harness/gauntlet/shot-graph.mjs gradient
//   node harness/gauntlet/shot-graph.mjs --all --out /tmp/run-b   # stability diff
//
// Output: <out>/web-<pattern>-<view>.png at 2x, plus manifest.json recording
// the framing and a sha256 per shot. WebKit, because that is the engine inside
// the WKWebView the app ships in.
//
// One page load per shot: the whole-graph shot ends with a fitView, and a
// closeup taken after it would otherwise depend on capture order.
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as playwright from "playwright";
import { createServer } from "vite";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const VALUED = new Set(["--out", "--browser"]);
const argv = process.argv.slice(2);
const opts = { out: "harness/gauntlet", browser: "webkit" };
const requested = [];
let all = false;
while (argv.length > 0) {
	const a = argv.shift();
	if (a === "--all") all = true;
	else if (VALUED.has(a)) opts[a.slice(2)] = argv.shift();
	else if (a.startsWith("--")) {
		console.error(`unknown flag: ${a}`);
		process.exit(1);
	} else requested.push(a);
}
if (!all && requested.length === 0) {
	console.error(
		"usage: node harness/gauntlet/shot-graph.mjs [--all] [pattern ...] [--out DIR] [--browser webkit|chromium]",
	);
	process.exit(1);
}

// The view-node legend commits on a 250 ms trailing throttle
// (view-channel-node.tsx). Settling past it is what makes the shot a function
// of the fixture rather than of how fast the machine mounted the page.
const LEGEND_SETTLE_MS = 600;
const outDir = path.resolve(root, opts.out);
mkdirSync(outDir, { recursive: true });

const server = await createServer({
	root,
	server: { port: 4191, strictPort: false },
	clearScreen: false,
	logLevel: "warn",
});
await server.listen();
const base = `http://localhost:${server.config.server.port}/harness-graph.html`;

const browser = await playwright[opts.browser].launch();
const page = await browser.newPage({
	// Must be at least the largest canvas the page renders — the whole view sizes
	// itself to the graph, so give it room.
	viewport: { width: 2200, height: 1500 },
	deviceScaleFactor: 2,
});
page.on("pageerror", (e) => console.error(`[page] ${e.message}`));

const shots = [];
try {
	await page.goto(base);
	const catalogue = await page.evaluate(() => window.__GRAPH__.patterns);
	const views = await page.evaluate(() => window.__GRAPH__.views);
	const patterns = all ? catalogue : requested;

	for (const pattern of patterns) {
		if (!catalogue.includes(pattern)) {
			console.error(`unknown pattern: ${pattern} (have: ${catalogue.join(", ")})`);
			process.exitCode = 1;
			continue;
		}
		for (const view of views) {
			await page.goto(
				`${base}?pattern=${encodeURIComponent(pattern)}&view=${view}`,
			);
			await page.waitForFunction(() => window.__GRAPH__?.ready(), null, {
				timeout: 30_000,
			});
			await page.evaluate(() => document.fonts.ready);
			await page.waitForTimeout(LEGEND_SETTLE_MS);
			await page.evaluate(() => window.__GRAPH__.frame());
			// Two frames so the viewport transform and any post-measure layout
			// have committed before the shot.
			await page.evaluate(
				() =>
					new Promise((r) =>
						requestAnimationFrame(() => requestAnimationFrame(r)),
					),
			);

			const png = await page.locator("#canvas").screenshot();
			const file = path.join(outDir, `web-${pattern}-${view}.png`);
			writeFileSync(file, png);
			shots.push({
				pattern,
				view,
				file: path.basename(file),
				bytes: png.length,
				sha256: createHash("sha256").update(png).digest("hex"),
			});
			console.log(`${file}  ${(png.length / 1024).toFixed(0)} KiB`);
		}
	}

	writeFileSync(
		path.join(outDir, "manifest.json"),
		`${JSON.stringify(
			{
				subject: "pattern-graph canvas (react-flow, web)",
				capturedAt: new Date().toISOString(),
				browser: `${opts.browser} ${browser.version()}`,
				deviceScaleFactor: 2,
				fixtures: "harness/gauntlet/fixtures/<pattern>.json",
				nodeTypes: "harness/gauntlet/node-types.json",
				shots,
			},
			null,
			"\t",
		)}\n`,
	);
} finally {
	await browser.close();
	await server.close();
}
