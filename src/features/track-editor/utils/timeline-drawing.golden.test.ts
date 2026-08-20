import { describe, expect, it, vi } from "vitest";
import type { BeatGrid } from "@/bindings/schema";
import goldens from "../../../../harness/goldens/timeline-drawing.json" with {
	type: "json",
};
import type { TrackWaveform } from "../stores/use-track-editor-store";
import type { TimelineLayout } from "./timeline-constants";

/**
 * Golden-vector characterization test for the timeline's pixel geometry:
 * `drawBeatGrid`, `drawTimeRuler`, `drawWaveform`.
 *
 * This file REGENERATES NOTHING. It loads `harness/goldens/timeline-drawing.json`
 * and asserts that today's draw calls deeply equal the recorded ones. The goldens
 * describe current behaviour, not desired behaviour — see "recorded quirks".
 *
 * ## How the goldens were produced
 *
 * The drawing functions take a canvas context and return nothing, so a throwaway
 * vitest file (same `vi.mock` of `./canvas-colors`, same `RecordingCtx`,
 * `materializeGrid` / `materializeWaveform` and `runCase` as below) built the
 * inputs, ran each case, and wrote `{ case, input, output }` triples. To
 * regenerate after a deliberate change, re-run an equivalent script — do not
 * hand-edit a golden.
 *
 * ## Encoding
 *
 * `output` is the recorded call log: every context mutation in order, as
 * `{ op, args }`. Style setters are recorded as `set:fillStyle` etc. so the
 * ordering of style changes relative to path ops is pinned, not just the paths.
 *
 * `input` stores *specs*, not materialized arrays — a 4096-bucket waveform of
 * raw floats would make the golden unreadable. `materializeGrid` /
 * `materializeWaveform` below expand them deterministically (waveform samples
 * come from a seeded mulberry32), so the fixtures are reproducible from the
 * short spec alone.
 *
 * ## Color mocking
 *
 * `./canvas-colors` is mocked to literal `color(--var)` / `color(--var/alpha)`
 * strings. The goldens therefore carry geometry and *which* token was chosen,
 * never a theme value, and the test needs no DOM or `getComputedStyle`.
 *
 * ## Float tolerance
 *
 * None — strict deep-equal. Every recorded coordinate is rounded to 1e-6 at
 * record time, which absorbs the last-bit noise of the `time / bucketsPerSecond`
 * and `beat * zoom` products while leaving every meaningful pixel difference
 * visible. Non-finite values (NaN/Infinity, reachable via `durationSeconds: 0`)
 * are recorded as-is; JSON serializes them as `null`, which is why the
 * zero-duration case's coordinates read as `null` in the golden.
 *
 * ## Recorded quirks (behaviour pinned here, NOT endorsed)
 *
 * - Downbeat de-duplication keys off `Math.round(t * 1000)` (millisecond
 *   rounding), so a downbeat 0.0004 s away from a beat suppresses that beat.
 *   A port using exact float equality — or a different rounding mode — draws a
 *   doubled line. See the `msCollide` cases.
 * - The 6px beat-culling compares against the last *drawn* beat's x, and the
 *   downbeat-suppressed beats never update `lastBeatX`. Real 120–174 bpm grids
 *   never get within 6px even at MIN_ZOOM, so the `dense250` / `dense260` /
 *   `dense400` cases exist purely to pin that branch (spacing exactly on, just
 *   under, and far under the threshold).
 * - `getBarLabelStep` derives the bar duration from the gap between the first
 *   two downbeats only; with drifting beats that diverges from
 *   `averageBeatDuration * beatsPerBar` (the single-downbeat fallback).
 * - `durationSeconds: 0` yields `bucketsPerSecond = Infinity` and NaN
 *   coordinates rather than an early return.
 */

vi.mock("./canvas-colors", () => ({
	getCanvasColor: (cssVar: string) => `color(${cssVar})`,
	getCanvasColorRgba: (cssVar: string, alpha: number) =>
		`color(${cssVar}/${alpha})`,
}));

import {
	type Ctx2D,
	drawBeatGrid,
	drawTimeRuler,
	drawWaveform,
} from "./timeline-drawing";

// ---------------------------------------------------------------------------
// Recording context — every call becomes one golden entry.
// ---------------------------------------------------------------------------

const round = (n: number) =>
	Number.isFinite(n) ? Math.round(n * 1e6) / 1e6 : n;

type Call = { op: string; args: (number | string | boolean)[] };

class RecordingCtx {
	calls: Call[] = [];
	#push(op: string, args: (number | string | boolean)[]) {
		this.calls.push({
			op,
			args: args.map((a) => (typeof a === "number" ? round(a) : a)),
		});
	}
	#style(op: string) {
		return (v: string | number | boolean) => this.#push(op, [v]);
	}
	set fillStyle(v: string) {
		this.#style("set:fillStyle")(v);
	}
	set strokeStyle(v: string) {
		this.#style("set:strokeStyle")(v);
	}
	set lineWidth(v: number) {
		this.#style("set:lineWidth")(v);
	}
	set font(v: string) {
		this.#style("set:font")(v);
	}
	set globalAlpha(v: number) {
		this.#style("set:globalAlpha")(v);
	}
	beginPath() {
		this.#push("beginPath", []);
	}
	closePath() {
		this.#push("closePath", []);
	}
	moveTo(x: number, y: number) {
		this.#push("moveTo", [x, y]);
	}
	lineTo(x: number, y: number) {
		this.#push("lineTo", [x, y]);
	}
	rect(x: number, y: number, w: number, h: number) {
		this.#push("rect", [x, y, w, h]);
	}
	fillRect(x: number, y: number, w: number, h: number) {
		this.#push("fillRect", [x, y, w, h]);
	}
	strokeRect(x: number, y: number, w: number, h: number) {
		this.#push("strokeRect", [x, y, w, h]);
	}
	fillText(t: string, x: number, y: number) {
		this.#push("fillText", [t, x, y]);
	}
	stroke() {
		this.#push("stroke", []);
	}
	fill() {
		this.#push("fill", []);
	}
	save() {
		this.#push("save", []);
	}
	restore() {
		this.#push("restore", []);
	}
	clip() {
		this.#push("clip", []);
	}
	setLineDash(d: number[]) {
		this.#push("setLineDash", d);
	}
}

// ---------------------------------------------------------------------------
// Fixture specs -> materialized inputs (deterministic).
// ---------------------------------------------------------------------------

/** Deterministic PRNG so waveform fixtures stay specs, not megabytes of floats. */
function mulberry32(seed: number) {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = Math.imul(a ^ (a >>> 15), 1 | a);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

type GridSpec =
	| { kind: "constant"; bpm: number; beatsPerBar: number; beatCount: number }
	| {
			kind: "drift";
			base: number;
			step: number;
			beatsPerBar: number;
			beatCount: number;
	  }
	| { kind: "single" }
	| { kind: "msCollide" }
	| { kind: "empty" };

type WaveformSpec = {
	kind: "bands" | "full";
	seed: number;
	numBuckets: number;
	colors: "exact" | "wrong" | null;
};

function materializeGrid(spec: GridSpec): BeatGrid {
	if (spec.kind === "empty") {
		return {
			beats: [],
			downbeats: [],
			bpm: 120,
			downbeatOffset: 0,
			beatsPerBar: 4,
		};
	}
	if (spec.kind === "single") {
		return {
			beats: [0.5],
			downbeats: [0.5],
			bpm: 120,
			downbeatOffset: 0.5,
			beatsPerBar: 4,
		};
	}
	if (spec.kind === "msCollide") {
		// Downbeats sit 0.0004 s off their beat: exact-float comparison sees two
		// distinct times, ms-rounded keys see one.
		const beats: number[] = [];
		for (let i = 0; i < 32; i++) beats.push(i * 0.5);
		const downbeats = beats
			.filter((_, i) => i % 4 === 0)
			.map((t) => t + 0.0004);
		return {
			beats,
			downbeats,
			bpm: 120,
			downbeatOffset: 0.0004,
			beatsPerBar: 4,
		};
	}
	const beats: number[] = [];
	if (spec.kind === "constant") {
		const dt = 60 / spec.bpm;
		for (let i = 0; i < spec.beatCount; i++) beats.push(i * dt);
	} else {
		let t = 0;
		for (let i = 0; i < spec.beatCount; i++) {
			beats.push(t);
			t += spec.base + spec.step * i;
		}
	}
	const beatsPerBar = spec.beatsPerBar;
	const downbeats = beats.filter((_, i) => i % beatsPerBar === 0);
	return {
		beats,
		downbeats,
		bpm: spec.kind === "constant" ? spec.bpm : 120,
		downbeatOffset: 0,
		beatsPerBar,
	};
}

function materializeWaveform(spec: WaveformSpec): TrackWaveform {
	const rnd = mulberry32(spec.seed);
	const n = spec.numBuckets;
	const base = {
		trackId: "golden",
		previewSamples: [],
		fullSamples: null,
		bands: null,
		previewBands: null,
		colors: null,
		previewColors: null,
		sampleRate: 44100,
		durationSeconds: 0,
	} as unknown as TrackWaveform;

	if (spec.kind === "bands") {
		const low: number[] = [];
		const mid: number[] = [];
		const high: number[] = [];
		for (let i = 0; i < n; i++) {
			low.push(rnd());
			mid.push(rnd());
			high.push(rnd());
		}
		return { ...base, bands: { low, mid, high } } as TrackWaveform;
	}

	const fullSamples: number[] = [];
	for (let i = 0; i < n; i++) {
		const a = rnd() * 2 - 1;
		const b = rnd() * 2 - 1;
		fullSamples.push(Math.min(a, b), Math.max(a, b));
	}
	let colors: number[] | null = null;
	if (spec.colors) {
		// "wrong" is deliberately numBuckets*3 + 1 so the exact-length branch
		// check is pinned, not just the null case.
		const len = spec.colors === "exact" ? n * 3 : n * 3 + 1;
		colors = [];
		for (let i = 0; i < len; i++) colors.push(Math.floor(rnd() * 256));
	}
	return { ...base, fullSamples, colors } as TrackWaveform;
}

// ---------------------------------------------------------------------------
// Case dispatch
// ---------------------------------------------------------------------------

type GoldenInput =
	| {
			fn: "drawBeatGrid";
			grid: GridSpec;
			startTime: number;
			endTime: number;
			zoom: number;
			scrollLeft: number;
			height: number;
			layout: TimelineLayout;
	  }
	| {
			fn: "drawTimeRuler";
			startTime: number;
			endTime: number;
			zoom: number;
			scrollLeft: number;
			layout: TimelineLayout;
	  }
	| {
			fn: "drawWaveform";
			waveform: WaveformSpec | null;
			startTime: number;
			endTime: number;
			durationSeconds: number;
			zoom: number;
			scrollLeft: number;
			width: number;
			layout: TimelineLayout;
	  };

function runCase(input: GoldenInput): Call[] {
	const rec = new RecordingCtx();
	const ctx = rec as unknown as Ctx2D;
	if (input.fn === "drawBeatGrid") {
		drawBeatGrid(
			ctx,
			materializeGrid(input.grid),
			input.startTime,
			input.endTime,
			input.zoom,
			input.scrollLeft,
			input.height,
			input.layout,
		);
	} else if (input.fn === "drawTimeRuler") {
		drawTimeRuler(
			ctx,
			input.startTime,
			input.endTime,
			input.zoom,
			input.scrollLeft,
			input.layout,
		);
	} else {
		drawWaveform(
			ctx,
			input.waveform ? materializeWaveform(input.waveform) : null,
			input.startTime,
			input.endTime,
			input.durationSeconds,
			input.zoom,
			input.scrollLeft,
			input.width,
			input.layout,
		);
	}
	// Round-trip through JSON so non-finite values compare the way they were
	// recorded (NaN / Infinity serialize to null).
	return JSON.parse(JSON.stringify(rec.calls));
}

type GoldenCase = { case: string; input: GoldenInput; output: Call[] };
const cases = goldens as unknown as GoldenCase[];

describe("timeline-drawing goldens", () => {
	it("golden file is populated", () => {
		expect(cases.length).toBeGreaterThanOrEqual(15);
		expect(new Set(cases.map((c) => c.case)).size).toBe(cases.length);
	});

	for (const c of cases) {
		it(c.case, () => {
			expect(runCase(c.input)).toEqual(c.output);
		});
	}
});
