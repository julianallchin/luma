#!/usr/bin/env node
// Capture the three.js golden scenes (docs/specs/wgpu-renderer.md §7.1).
//
//   node harness/shot-visualizer.mjs --all
//   node harness/shot-visualizer.mjs single-mover strobe-duty
//   node harness/shot-visualizer.mjs --all --out harness/goldens/run-b
//   node harness/shot-visualizer.mjs --all --browser chromium
//
// Output: <out>/scenes/, holding <scene>-<t>.png plus manifest.json — the full
// input description of every frame (camera, render settings, fixture poses,
// primitive state) so a golden can be re-derived from the manifest alone. The
// manifest lives *inside* scenes/ because <out> is shared with the unit-test
// golden JSONs.
//
// One page load per frame: `uFrame` and the temporal history must both start
// from zero, or a frame's noise depends on the frames captured before it.
import { createHash } from "node:crypto";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as playwright from "playwright";
import { createServer } from "vite";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
// `--all` is a bare switch; `--out` / `--browser` take the next token.
const VALUED = new Set(["--out", "--browser"]);
const argv = process.argv.slice(2);
const opts = { out: "harness/goldens", browser: "webkit" };
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

const outDir = path.resolve(root, opts.out);
const browserName = opts.browser;

if (!all && requested.length === 0) {
	console.error("usage: node harness/shot-visualizer.mjs [--all] [scene-id ...] [--out DIR] [--browser webkit|chromium]");
	process.exit(1);
}

const shotsDir = path.join(outDir, "scenes");
rmSync(shotsDir, { recursive: true, force: true });
mkdirSync(shotsDir, { recursive: true });

const server = await createServer({
	root,
	server: { port: 4189, strictPort: false },
	clearScreen: false,
	logLevel: "warn",
});
await server.listen();
const base = `http://localhost:${server.config.server.port}/harness-3d.html`;

const browser = await playwright[browserName].launch();
const page = await browser.newPage({
	viewport: { width: 1000, height: 700 },
	deviceScaleFactor: 2,
});
page.on("pageerror", (e) => console.error(`[page] ${e.message}`));

/** `4.2` -> `4.200`; stable, sortable, filesystem-safe frame ids. */
const stamp = (t) => t.toFixed(3);

try {
	await page.goto(base);
	const catalogue = await page.evaluate(() => window.__GOLDEN__.scenes);
	const ids = all ? catalogue.map((s) => s.id) : requested;

	const frames = [];
	const scenes = {};

	for (const id of ids) {
		const entry = catalogue.find((s) => s.id === id);
		if (!entry) {
			console.error(`unknown scene: ${id} (available: ${catalogue.map((s) => s.id).join(", ")})`);
			process.exitCode = 1;
			continue;
		}

		for (const t of entry.times) {
			await page.goto(`${base}?scene=${encodeURIComponent(id)}`);
			await page.waitForFunction(() => window.__GOLDEN__.ready(), null, { timeout: 30_000 });
			// `ready()` fires when the meshes are mounted; the effects that scale
			// them to their physical dimensions and register their lights run
			// after. Settle two frames so every post-mount effect has landed
			// before the clock is pinned — without it the heaviest scene
			// occasionally captures a fixture mid-setup.
			await page.evaluate(
				() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
			);
			await page.evaluate((time) => window.__GOLDEN__.frame(time), t);

			// One screenshot, then write and hash the same bytes — capturing
			// twice would let the two disagree and silently pass the compare.
			const png = await page.locator("canvas").screenshot();
			const file = path.join(shotsDir, `${id}-${stamp(t)}.png`);
			writeFileSync(file, png);

			if (!scenes[id]) scenes[id] = await page.evaluate(() => window.__GOLDEN__.describe());
			frames.push({
				scene: id,
				t,
				file: path.basename(file),
				bytes: png.length,
				sha256: createHash("sha256").update(png).digest("hex"),
			});
			console.log(`${file}  ${(png.length / 1024).toFixed(0)} KiB`);
		}
	}

	writeFileSync(
		path.join(shotsDir, "manifest.json"),
		`${JSON.stringify(
			{
				renderer: "three",
				capturedAt: new Date().toISOString(),
				browser: `${browserName} ${browser.version()}`,
				deviceScaleFactor: 2,
				scenes,
				frames,
			},
			null,
			"\t",
		)}\n`,
	);
	console.log(path.join(shotsDir, "manifest.json"));
} finally {
	await browser.close();
	await server.close();
}
