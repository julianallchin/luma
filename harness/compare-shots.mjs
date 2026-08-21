#!/usr/bin/env node
// Compare two golden-capture directories frame by frame.
//
//   node harness/compare-shots.mjs harness/goldens harness/goldens-b
//   node harness/compare-shots.mjs A B --blur 4 --min 0.98
//
// Reports per-frame SSIM and mean absolute luma difference, and exits non-zero
// if any frame falls below --min (default 0.999, the phase-0b stability gate).
//
// SSIM is computed on grayscale with the standard 8x8 windowed formula. An
// optional Gaussian pre-blur (spec §7.3 uses sigma=4 px) kills stochastic haze
// grain while keeping structure — leave it at 0 for a run-to-run stability
// check, where any grain difference is exactly what you want to catch.
//
// Decoding runs inside headless Chromium's canvas, the same trick
// harness/measure-ink.mjs uses, so this stays dependency-free.
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { chromium } from "playwright";

const argv = process.argv.slice(2);
const VALUED = new Set(["--blur", "--min"]);
const opts = { blur: 0, min: 0.999 };
const dirs = [];
while (argv.length > 0) {
	const a = argv.shift();
	if (VALUED.has(a)) opts[a.slice(2)] = Number(argv.shift());
	else if (a.startsWith("--")) {
		console.error(`unknown flag: ${a}`);
		process.exit(1);
	} else dirs.push(a);
}

if (dirs.length !== 2) {
	console.error("usage: node harness/compare-shots.mjs DIR_A DIR_B [--blur PX] [--min SSIM]");
	process.exit(1);
}

const [dirA, dirB] = dirs.map((d) => path.resolve(d, "scenes"));
const names = readdirSync(dirA).filter((f) => f.endsWith(".png")).sort();
const namesB = new Set(readdirSync(dirB).filter((f) => f.endsWith(".png")));

const browser = await chromium.launch();
const page = await browser.newPage();

/**
 * SSIM + mean luma delta for one pair. Runs in the page so PNG decoding is the
 * browser's problem; returns only the two scalars.
 */
async function compare(fileA, fileB, blur) {
	return page.evaluate(
		async ([a, b, sigma]) => {
			async function luma(b64) {
				const img = new Image();
				img.src = `data:image/png;base64,${b64}`;
				await img.decode();
				const c = document.createElement("canvas");
				c.width = img.width;
				c.height = img.height;
				const ctx = c.getContext("2d");
				if (sigma > 0) ctx.filter = `blur(${sigma}px)`;
				ctx.drawImage(img, 0, 0);
				const { data } = ctx.getImageData(0, 0, c.width, c.height);
				const out = new Float64Array(c.width * c.height);
				for (let i = 0; i < out.length; i++) {
					out[i] = 0.2126 * data[i * 4] + 0.7152 * data[i * 4 + 1] + 0.0722 * data[i * 4 + 2];
				}
				return { px: out, w: c.width, h: c.height };
			}

			const A = await luma(a);
			const B = await luma(b);
			if (A.w !== B.w || A.h !== B.h) return { error: `size ${A.w}x${A.h} vs ${B.w}x${B.h}` };

			// SSIM constants for 8-bit dynamic range.
			const C1 = (0.01 * 255) ** 2;
			const C2 = (0.03 * 255) ** 2;
			const WIN = 8;
			let ssimSum = 0;
			let windows = 0;
			let absSum = 0;
			for (let i = 0; i < A.px.length; i++) absSum += Math.abs(A.px[i] - B.px[i]);

			for (let y = 0; y + WIN <= A.h; y += WIN) {
				for (let x = 0; x + WIN <= A.w; x += WIN) {
					let ma = 0;
					let mb = 0;
					for (let j = 0; j < WIN; j++) {
						for (let i = 0; i < WIN; i++) {
							ma += A.px[(y + j) * A.w + x + i];
							mb += B.px[(y + j) * A.w + x + i];
						}
					}
					const n = WIN * WIN;
					ma /= n;
					mb /= n;
					let va = 0;
					let vb = 0;
					let cov = 0;
					for (let j = 0; j < WIN; j++) {
						for (let i = 0; i < WIN; i++) {
							const da = A.px[(y + j) * A.w + x + i] - ma;
							const db = B.px[(y + j) * A.w + x + i] - mb;
							va += da * da;
							vb += db * db;
							cov += da * db;
						}
					}
					va /= n - 1;
					vb /= n - 1;
					cov /= n - 1;
					ssimSum +=
						((2 * ma * mb + C1) * (2 * cov + C2)) /
						((ma * ma + mb * mb + C1) * (va + vb + C2));
					windows++;
				}
			}
			return { ssim: ssimSum / windows, meanAbs: absSum / A.px.length };
		},
		[readFileSync(fileA).toString("base64"), readFileSync(fileB).toString("base64"), blur],
	);
}

let worst = 1;
let failures = 0;
try {
	for (const name of names) {
		if (!namesB.has(name)) {
			console.error(`missing in B: ${name}`);
			failures++;
			continue;
		}
		const r = await compare(path.join(dirA, name), path.join(dirB, name), opts.blur);
		if (r.error) {
			console.error(`${name}  ${r.error}`);
			failures++;
			continue;
		}
		const ok = r.ssim >= opts.min;
		if (!ok) failures++;
		worst = Math.min(worst, r.ssim);
		console.log(
			`${ok ? "ok  " : "FAIL"} ${name.padEnd(30)} ssim ${r.ssim.toFixed(6)}  meanAbsLuma ${r.meanAbs.toFixed(4)}`,
		);
	}
} finally {
	await browser.close();
}

console.log(`\n${names.length} frames, worst ssim ${worst.toFixed(6)}, threshold ${opts.min}`);
if (failures > 0) {
	console.error(`${failures} frame(s) failed`);
	process.exitCode = 1;
}
