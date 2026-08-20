#!/usr/bin/env node
// Measure the ink bounding box (light pixels) in a shot — a cheap objective
// check for typography drift between web and gpui captures of a fixture.
//
//   node harness/measure-ink.mjs harness/shots/web/button.png harness/shots/gpui/button.png
import { readFileSync } from "node:fs";
import { chromium } from "playwright";

const files = process.argv.slice(2);
const browser = await chromium.launch();
const page = await browser.newPage();

for (const file of files) {
	const b64 = readFileSync(file).toString("base64");
	const r = await page.evaluate(async (b64) => {
		const img = new Image();
		img.src = `data:image/png;base64,${b64}`;
		await img.decode();
		const c = document.createElement("canvas");
		c.width = img.width;
		c.height = img.height;
		const ctx = c.getContext("2d");
		ctx.drawImage(img, 0, 0);
		const { data } = ctx.getImageData(0, 0, c.width, c.height);
		let minX = 1e9, minY = 1e9, maxX = -1, maxY = -1;
		for (let y = 0; y < c.height; y++)
			for (let x = 0; x < c.width; x++) {
				const i = (y * c.width + x) * 4;
				const lum = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
				if (lum > 140) {
					if (x < minX) minX = x;
					if (x > maxX) maxX = x;
					if (y < minY) minY = y;
					if (y > maxY) maxY = y;
				}
			}
		return maxX < 0
			? { empty: true, width: c.width, height: c.height }
			: { canvas: `${c.width}x${c.height}`, inkW: maxX - minX + 1, inkH: maxY - minY + 1 };
	}, b64);
	console.log(file, JSON.stringify(r));
}
await browser.close();
