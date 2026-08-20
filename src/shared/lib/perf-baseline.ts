/**
 * Webview perf-baseline capture.
 *
 * Records, per hand-driven scenario, what the React/WKWebView build actually
 * costs: rAF frame deltas, a long-task proxy, and input-to-paint latency. The
 * numbers exist so the GPUI port has a falsifiable success criterion instead of
 * a vibe ("webview perf is too bad"). Procedure + acceptance metrics live in
 * `docs/specs/perf-baseline.md`.
 *
 * This module is **never** imported by a production path. `main.tsx` reads one
 * localStorage key at boot and only then dynamic-imports it, so when the flag is
 * off the code isn't even in the main chunk and nothing runs.
 *
 * Dumps ride the existing render-telemetry pipe (`append_render_telemetry`), so
 * they land durably in `render-telemetry.log` next to the visualizer's WebGL
 * snapshots — no second persistence mechanism. `harness/perf/extract-baseline.mjs`
 * lifts them back out into `harness/perf/*.json`.
 */

import { getVersion } from "@tauri-apps/api/app";

import { appendRenderTelemetry } from "@/features/visualizer/lib/render-telemetry";

export const PERF_BASELINE_FLAG = "luma:perf-baseline";

/** Histogram resolution: quarter-millisecond buckets up to 200ms. */
const BUCKET_MS = 0.25;
const MAX_BUCKET = Math.round(200 / BUCKET_MS);

/** rAF gap at or above this is counted as a jank event (long-task proxy). */
const JANK_MS = 50;

/** Common display rates a measured refresh estimate is snapped to. */
const REFRESH_RATES = [60, 90, 120, 144, 165, 240];

type SparseHistogram = {
	bucketMs: number;
	/** `[bucketIndex, count]` pairs, ascending. */
	buckets: [number, number][];
};

type Summary = {
	count: number;
	minMs: number;
	maxMs: number;
	meanMs: number;
	p50Ms: number;
	p95Ms: number;
	p99Ms: number;
};

type InputSample = {
	type: string;
	/** Event timestamp → start of the next rAF callback. */
	toFrameStartMs: number;
	/** Event timestamp → start of the frame after that (paint has landed). */
	toPaintMs: number;
};

export type PerfSegment = {
	label: string;
	route: string;
	startedAt: string;
	durationMs: number;
	estimatedRefreshHz: number | null;
	frames: Summary & {
		fps: number;
		histogram: SparseHistogram;
		/** Frames slower than N× the display frame budget. */
		overBudget: { x1: number; x2: number; x4: number };
		budgetMs: number;
	};
	jank: { count: number; totalMs: number; maxMs: number; thresholdMs: number };
	longTasks: {
		supported: boolean;
		count: number;
		totalMs: number;
		maxMs: number;
	};
	eventTiming: { supported: boolean; count: number; summary: Summary | null };
	input: { samples: InputSample[]; toPaint: Summary | null };
	jsHeapBytes: { start: number | null; end: number | null };
};

type Accumulator = {
	label: string;
	route: string;
	startedAt: string;
	startNow: number;
	lastFrameNow: number;
	counts: Map<number, number>;
	frameCount: number;
	sumMs: number;
	minMs: number;
	maxMs: number;
	jankCount: number;
	jankTotalMs: number;
	jankMaxMs: number;
	longTaskCount: number;
	longTaskTotalMs: number;
	longTaskMaxMs: number;
	eventDurationsMs: number[];
	inputSamples: InputSample[];
	heapStart: number | null;
	rafId: number | null;
	observers: PerformanceObserver[];
	inputSampleInFlight: boolean;
	detachInput: (() => void) | null;
	longTaskSupported: boolean;
	eventTimingSupported: boolean;
};

type PerformanceWithMemory = Performance & {
	memory?: { usedJSHeapSize: number };
};

let active: Accumulator | null = null;
const segments: PerfSegment[] = [];
let installed = false;
let appVersion: string | null = null;

export function isPerfBaselineEnabled() {
	try {
		return localStorage.getItem(PERF_BASELINE_FLAG) === "1";
	} catch {
		return false;
	}
}

/* ------------------------------------------------------------------ stats */

function record(acc: Accumulator, deltaMs: number) {
	const bucket = Math.min(Math.round(deltaMs / BUCKET_MS), MAX_BUCKET);
	acc.counts.set(bucket, (acc.counts.get(bucket) ?? 0) + 1);
	acc.frameCount += 1;
	acc.sumMs += deltaMs;
	if (deltaMs < acc.minMs) acc.minMs = deltaMs;
	if (deltaMs > acc.maxMs) acc.maxMs = deltaMs;
	if (deltaMs >= JANK_MS) {
		acc.jankCount += 1;
		acc.jankTotalMs += deltaMs;
		if (deltaMs > acc.jankMaxMs) acc.jankMaxMs = deltaMs;
	}
}

function sortedBuckets(counts: Map<number, number>): [number, number][] {
	return [...counts.entries()].sort((a, b) => a[0] - b[0]);
}

/** Percentile off the bucket histogram — exact enough at 0.25ms resolution. */
function histogramPercentile(
	buckets: [number, number][],
	total: number,
	q: number,
) {
	if (total === 0) return 0;
	const target = q * total;
	let seen = 0;
	for (const [bucket, count] of buckets) {
		seen += count;
		if (seen >= target) return bucket * BUCKET_MS;
	}
	return buckets[buckets.length - 1][0] * BUCKET_MS;
}

function summarize(values: number[]): Summary | null {
	if (values.length === 0) return null;
	const sorted = [...values].sort((a, b) => a - b);
	const at = (q: number) =>
		sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
	return {
		count: sorted.length,
		minMs: round(sorted[0]),
		maxMs: round(sorted[sorted.length - 1]),
		meanMs: round(sorted.reduce((a, b) => a + b, 0) / sorted.length),
		p50Ms: round(at(0.5)),
		p95Ms: round(at(0.95)),
		p99Ms: round(at(0.99)),
	};
}

function round(value: number) {
	return Math.round(value * 100) / 100;
}

/**
 * Snap the fastest observed frames to a plausible display rate. p10 (not min)
 * so one lucky sub-budget delta can't claim 240Hz.
 */
function estimateRefreshHz(buckets: [number, number][], total: number) {
	if (total < 30) return null;
	const p10 = histogramPercentile(buckets, total, 0.1);
	if (p10 <= 0) return null;
	const hz = 1000 / p10;
	let best = REFRESH_RATES[0];
	for (const rate of REFRESH_RATES) {
		if (Math.abs(rate - hz) < Math.abs(best - hz)) best = rate;
	}
	return best;
}

function jsHeapBytes() {
	return (performance as PerformanceWithMemory).memory?.usedJSHeapSize ?? null;
}

function route() {
	return window.location.hash || window.location.pathname;
}

/* -------------------------------------------------------------- observers */

function supportsEntryType(type: string) {
	return (PerformanceObserver.supportedEntryTypes ?? []).includes(type);
}

function observeLongTasks(acc: Accumulator) {
	if (!supportsEntryType("longtask")) return false;
	try {
		const observer = new PerformanceObserver((list) => {
			for (const entry of list.getEntries()) {
				acc.longTaskCount += 1;
				acc.longTaskTotalMs += entry.duration;
				if (entry.duration > acc.longTaskMaxMs)
					acc.longTaskMaxMs = entry.duration;
			}
		});
		observer.observe({ type: "longtask", buffered: false });
		acc.observers.push(observer);
		return true;
	} catch {
		return false;
	}
}

function observeEventTiming(acc: Accumulator) {
	if (!supportsEntryType("event")) return false;
	try {
		const observer = new PerformanceObserver((list) => {
			for (const entry of list.getEntries())
				acc.eventDurationsMs.push(entry.duration);
		});
		// `durationThreshold` is Event Timing-specific and missing from the DOM
		// lib's PerformanceObserverInit.
		observer.observe({
			type: "event",
			durationThreshold: 16,
			buffered: false,
		} as PerformanceObserverInit & { durationThreshold: number });
		acc.observers.push(observer);
		return true;
	} catch {
		return false;
	}
}

/**
 * Input-to-paint: from the event's timestamp to the start of the frame *after*
 * the one that handled it — by then the handling frame's pixels are on screen.
 * One sample in flight at a time, so a drag doesn't sample every move event.
 */
function attachInputProbe(acc: Accumulator) {
	const types = ["pointerdown", "pointermove", "wheel", "keydown"] as const;

	const onInput = (event: Event) => {
		if (acc.inputSampleInFlight) return;
		// Segment-control hotkeys are capture machinery, not user input.
		if (isControlChord(event)) return;
		const stamp = event.timeStamp;
		const now = performance.now();
		if (!(stamp > 0) || stamp > now) return;
		acc.inputSampleInFlight = true;
		requestAnimationFrame((frameStart) => {
			requestAnimationFrame((paintedAt) => {
				acc.inputSampleInFlight = false;
				acc.inputSamples.push({
					type: event.type,
					toFrameStartMs: round(frameStart - stamp),
					toPaintMs: round(paintedAt - stamp),
				});
			});
		});
	};

	for (const type of types)
		window.addEventListener(type, onInput, { capture: true, passive: true });

	acc.detachInput = () => {
		for (const type of types)
			window.removeEventListener(type, onInput, { capture: true });
	};
}

/* ------------------------------------------------------------- lifecycle */

function begin(label: string): Accumulator {
	const now = performance.now();
	const acc: Accumulator = {
		label,
		route: route(),
		startedAt: new Date().toISOString(),
		startNow: now,
		lastFrameNow: now,
		counts: new Map(),
		frameCount: 0,
		sumMs: 0,
		minMs: Number.POSITIVE_INFINITY,
		maxMs: 0,
		jankCount: 0,
		jankTotalMs: 0,
		jankMaxMs: 0,
		longTaskCount: 0,
		longTaskTotalMs: 0,
		longTaskMaxMs: 0,
		eventDurationsMs: [],
		inputSamples: [],
		heapStart: jsHeapBytes(),
		rafId: null,
		observers: [],
		inputSampleInFlight: false,
		detachInput: null,
		longTaskSupported: false,
		eventTimingSupported: false,
	};

	const tick = (frameNow: number) => {
		record(acc, frameNow - acc.lastFrameNow);
		acc.lastFrameNow = frameNow;
		acc.rafId = requestAnimationFrame(tick);
	};
	acc.rafId = requestAnimationFrame(tick);

	acc.longTaskSupported = observeLongTasks(acc);
	acc.eventTimingSupported = observeEventTiming(acc);
	attachInputProbe(acc);

	return acc;
}

function finish(acc: Accumulator): PerfSegment {
	if (acc.rafId !== null) cancelAnimationFrame(acc.rafId);
	for (const observer of acc.observers) observer.disconnect();
	acc.detachInput?.();

	const buckets = sortedBuckets(acc.counts);
	const total = acc.frameCount;
	const durationMs = performance.now() - acc.startNow;
	const refreshHz = estimateRefreshHz(buckets, total);
	const budgetMs = 1000 / (refreshHz ?? 60);
	let x1 = 0;
	let x2 = 0;
	let x4 = 0;
	for (const [bucket, count] of buckets) {
		const ms = bucket * BUCKET_MS;
		if (ms > budgetMs * 1.5) x1 += count;
		if (ms > budgetMs * 2) x2 += count;
		if (ms > budgetMs * 4) x4 += count;
	}

	return {
		label: acc.label,
		route: acc.route,
		startedAt: acc.startedAt,
		durationMs: round(durationMs),
		estimatedRefreshHz: refreshHz,
		frames: {
			count: total,
			minMs: total ? round(acc.minMs) : 0,
			maxMs: round(acc.maxMs),
			meanMs: total ? round(acc.sumMs / total) : 0,
			p50Ms: round(histogramPercentile(buckets, total, 0.5)),
			p95Ms: round(histogramPercentile(buckets, total, 0.95)),
			p99Ms: round(histogramPercentile(buckets, total, 0.99)),
			fps: durationMs > 0 ? round((total / durationMs) * 1000) : 0,
			histogram: { bucketMs: BUCKET_MS, buckets },
			overBudget: { x1, x2, x4 },
			budgetMs: round(budgetMs),
		},
		jank: {
			count: acc.jankCount,
			totalMs: round(acc.jankTotalMs),
			maxMs: round(acc.jankMaxMs),
			thresholdMs: JANK_MS,
		},
		longTasks: {
			supported: acc.longTaskSupported,
			count: acc.longTaskCount,
			totalMs: round(acc.longTaskTotalMs),
			maxMs: round(acc.longTaskMaxMs),
		},
		eventTiming: {
			supported: acc.eventTimingSupported,
			count: acc.eventDurationsMs.length,
			summary: summarize(acc.eventDurationsMs),
		},
		input: {
			samples: acc.inputSamples,
			toPaint: summarize(acc.inputSamples.map((s) => s.toPaintMs)),
		},
		jsHeapBytes: { start: acc.heapStart, end: jsHeapBytes() },
	};
}

/* -------------------------------------------------------------- console API */

function start(label: string) {
	if (active) stop();
	if (!label) throw new Error("perf-baseline: start(label) needs a label");
	active = begin(label);
	renderBadge();
	console.info(
		`[perf-baseline] recording "${label}" — __lumaPerf.stop() to end`,
	);
	return label;
}

function stop() {
	if (!active) {
		console.warn("[perf-baseline] nothing is recording");
		return null;
	}
	const segment = finish(active);
	active = null;
	segments.push(segment);
	renderBadge();
	console.info(`[perf-baseline] "${segment.label}"`, segment);
	table();
	return segment;
}

function table() {
	console.table(
		segments.map((s) => ({
			label: s.label,
			seconds: round(s.durationMs / 1000),
			hz: s.estimatedRefreshHz,
			fps: s.frames.fps,
			p50: s.frames.p50Ms,
			p95: s.frames.p95Ms,
			p99: s.frames.p99Ms,
			worst: s.frames.maxMs,
			over2x: s.frames.overBudget.x2,
			jank: s.jank.count,
			"input p95": s.input.toPaint?.p95Ms ?? null,
		})),
	);
}

function envelope() {
	return {
		schema: "luma.perf-baseline/1",
		capturedAt: new Date().toISOString(),
		appVersion,
		userAgent: navigator.userAgent,
		devicePixelRatio: window.devicePixelRatio,
		viewport: { width: window.innerWidth, height: window.innerHeight },
		build: import.meta.env.MODE,
		supported: {
			longtask: supportsEntryType("longtask"),
			event: supportsEntryType("event"),
			jsHeap: jsHeapBytes() !== null,
		},
		segments,
	};
}

/**
 * Persist + surface the accumulated segments. Three routes, cheapest first:
 * the render-telemetry log on disk (durable, what the extract script reads),
 * the clipboard, and the console.
 */
function dump() {
	if (active) stop();
	const payload = envelope();
	appendRenderTelemetry("perf-baseline-dump", payload);
	const json = JSON.stringify(payload, null, 2);
	navigator.clipboard?.writeText(json).then(
		() => console.info("[perf-baseline] dump copied to clipboard"),
		() =>
			console.info("[perf-baseline] clipboard unavailable — copy from below"),
	);
	console.info(
		`[perf-baseline] wrote ${segments.length} segment(s) to render-telemetry.log\n` +
			"run: bun run perf:extract",
	);
	console.log(json);
	return payload;
}

function reset() {
	if (active) {
		finish(active);
		active = null;
	}
	segments.length = 0;
	console.info("[perf-baseline] cleared");
}

export type PerfBaselineApi = {
	start: typeof start;
	stop: typeof stop;
	dump: typeof dump;
	table: typeof table;
	reset: typeof reset;
	segments: () => PerfSegment[];
};

declare global {
	interface Window {
		__lumaPerf?: PerfBaselineApi;
	}
}

/**
 * The eight canonical segments of docs/specs/perf-baseline.md, in its order.
 * Hotkey Ctrl+Alt+<1..8> starts the matching segment; the doc and this list
 * must not drift.
 */
const SEGMENT_HOTKEYS = [
	"track-list-scroll",
	"graph-pan-zoom",
	"track-editor-playback",
	"track-editor-scrub",
	"visualizer-live",
	"agent-pane-streaming",
	"idle-welcome",
	"venue-tab-switch",
] as const;

function isControlChord(event: Event): boolean {
	return event instanceof KeyboardEvent && event.ctrlKey && event.altKey;
}

/**
 * Ctrl+Alt+1..8 start a canonical segment, Ctrl+Alt+0 stops, Ctrl+Alt+9
 * dumps. Exists so a recording session doesn't need the Web Inspector open —
 * the inspector's own cost then never pollutes a segment.
 */
function attachHotkeys() {
	window.addEventListener(
		"keydown",
		(event) => {
			if (!isControlChord(event)) return;
			const digit = event.code.startsWith("Digit")
				? Number(event.code.slice(5))
				: Number.NaN;
			if (Number.isNaN(digit)) return;
			event.preventDefault();
			if (digit === 0) stop();
			else if (digit === 9) void dump();
			else if (digit <= SEGMENT_HOTKEYS.length)
				start(SEGMENT_HOTKEYS[digit - 1]);
			renderBadge();
		},
		{ capture: true },
	);
}

/**
 * Corner badge visible whenever the capture is armed: "PERF" at rest, the
 * segment label while recording. The armed state is otherwise invisible
 * without the inspector, which is exactly where the doc says not to be.
 */
function renderBadge() {
	let el = document.getElementById("__luma-perf-badge");
	if (!el) {
		el = document.createElement("div");
		el.id = "__luma-perf-badge";
		el.style.cssText =
			"position:fixed;right:4px;bottom:4px;z-index:2147483647;" +
			"font:bold 9px monospace;letter-spacing:.08em;padding:2px 5px;" +
			"background:rgb(8 8 8);color:rgb(228 228 228);pointer-events:none";
		document.body.appendChild(el);
	}
	el.textContent = active ? `REC ${active.label}` : "PERF";
	el.style.color = active ? "rgb(255 80 80)" : "rgb(228 228 228)";
}

/** Idempotent. Only called when {@link isPerfBaselineEnabled} is true. */
export function installPerfBaseline() {
	if (installed) return;
	installed = true;
	getVersion().then(
		(version) => {
			appVersion = version;
		},
		() => {},
	);
	window.__lumaPerf = {
		start,
		stop,
		dump,
		table,
		reset,
		segments: () => segments,
	};
	attachHotkeys();
	if (document.body) renderBadge();
	else window.addEventListener("DOMContentLoaded", () => renderBadge());
	console.info(
		"[perf-baseline] armed. __lumaPerf.start('<scenario>') / .stop() / .dump()\n" +
			"hotkeys: ctrl+alt+1..8 start segment, ctrl+alt+0 stop, ctrl+alt+9 dump\n" +
			"procedure: docs/specs/perf-baseline.md",
	);
}
