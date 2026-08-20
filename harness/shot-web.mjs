#!/usr/bin/env node
// Screenshot web components rendered by the Vite harness page (harness.html)
// using Playwright WebKit — the closest engine to the WKWebView Tauri uses.
//
//   node harness/shot-web.mjs --all
//   node harness/shot-web.mjs button select
//
// Output: harness/shots/web/<id>.png (2x device pixels).
import { mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { webkit } from "playwright";
import { createServer } from "vite";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const outDir = path.join(root, "harness", "shots", "web");
const args = process.argv.slice(2);
const all = args.includes("--all");
const requested = args.filter((a) => !a.startsWith("--"));

if (!all && requested.length === 0) {
	console.error("usage: node harness/shot-web.mjs [--all] [fixture-id ...]");
	process.exit(1);
}

mkdirSync(outDir, { recursive: true });

const server = await createServer({
	root,
	server: { port: 4188, strictPort: false },
	clearScreen: false,
	logLevel: "warn",
});
await server.listen();
const base = `http://localhost:${server.config.server.port}/harness.html`;

const browser = await webkit.launch();
const page = await browser.newPage({
	viewport: { width: 800, height: 600 },
	deviceScaleFactor: 2,
});

try {
	await page.goto(base);
	await page.waitForSelector("body[data-ready='1']");
	const available = await page.evaluate(() => window.__FIXTURE_IDS__);
	const ids = all ? available : requested;

	for (const id of ids) {
		if (!available.includes(id)) {
			console.error(`unknown fixture: ${id} (available: ${available.join(", ")})`);
			process.exitCode = 1;
			continue;
		}
		await page.goto(`${base}?id=${encodeURIComponent(id)}`);
		const el = page.locator("#fixture[data-ready='1']");
		await el.waitFor();
		const file = path.join(outDir, `${id}.png`);
		await el.screenshot({ path: file });
		console.log(file);
	}
} finally {
	await browser.close();
	await server.close();
}
