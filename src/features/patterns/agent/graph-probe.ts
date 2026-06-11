import type { RunResult, Signal } from "@/bindings/schema";

/**
 * Sandboxed JavaScript probe for inspecting a graph run's signal data.
 *
 * The agent can't eyeball a 4096-float array, so instead of a fixed menu of
 * "summarize this signal" tools we give it a real JS execution environment:
 * it writes code, the code runs in an isolated Web Worker (no DOM, no app
 * state, hard timeout) with the latest run's view signals + helpers bound, and
 * whatever it returns (or logs) comes back as the tool result.
 *
 * Signal layout (see view-channel-node.tsx): a Signal is `{ n, t, c, data }`
 * where data is flat `[primitive][timeStep][channel]`, i.e.
 *   data[prim * t * c + time * c + ch].
 */

export type ProbeInput = {
	code: string;
	views: Record<string, Signal>;
	span: [number, number];
};

export type ProbeResult = {
	ok: boolean;
	/** JSON-serialized return value (or undefined if the code returned nothing). */
	result?: unknown;
	logs: string[];
	error?: string;
};

const TIMEOUT_MS = 4000;

// The worker body. Kept as a string so it can be turned into a Blob URL — no
// separate worker file to bundle. Helpers are defined in-scope so the agent's
// code can call them directly.
const WORKER_SOURCE = String.raw`
self.onmessage = async (e) => {
  const { code, views, span } = e.data;
  const logs = [];
  const log = (...args) => {
    logs.push(
      args
        .map((a) => {
          if (typeof a === "string") return a;
          try { return JSON.stringify(a); } catch { return String(a); }
        })
        .join(" "),
    );
  };
  const console = { log, warn: log, error: log, info: log };

  // --- signal helpers -----------------------------------------------------
  // A view signal: { n, t, c, data } with data[prim*t*c + time*c + ch].
  const at = (sig, prim, time, ch) =>
    sig.data[prim * sig.t * sig.c + time * sig.c + ch] ?? 0;
  // Time series of one (primitive, channel) across the whole span.
  const series = (sig, prim = 0, ch = 0) => {
    const out = new Array(sig.t);
    for (let i = 0; i < sig.t; i++) out[i] = at(sig, prim, i, ch);
    return out;
  };
  // Convert a sample index to seconds within the previewed span.
  const tOf = (sig, i) =>
    span[0] + ((span[1] - span[0]) * i) / Math.max(1, sig.t - 1);
  const stats = (arr) => {
    let min = Infinity, max = -Infinity, sum = 0, sq = 0;
    for (const v of arr) {
      if (v < min) min = v;
      if (v > max) max = v;
      sum += v;
      sq += v * v;
    }
    const len = arr.length || 1;
    return { len: arr.length, min, max, mean: sum / len, rms: Math.sqrt(sq / len) };
  };
  // Local maxima above 'threshold', separated by at least 'minGap' samples.
  const peaks = (arr, opts = {}) => {
    const threshold = opts.threshold ?? 0.5;
    const minGap = opts.minGap ?? 1;
    const out = [];
    let last = -Infinity;
    for (let i = 1; i < arr.length - 1; i++) {
      if (arr[i] > threshold && arr[i] >= arr[i - 1] && arr[i] > arr[i + 1]) {
        if (i - last >= minGap) {
          out.push(i);
          last = i;
        }
      }
    }
    return out;
  };

  try {
    const fn = new Function(
      "views", "at", "series", "tOf", "stats", "peaks", "console",
      "return (async () => {\n" + code + "\n})();",
    );
    const result = await fn(views, at, series, tOf, stats, peaks, console);
    let serialized;
    try {
      serialized = JSON.parse(JSON.stringify(result ?? null));
    } catch {
      serialized = String(result);
    }
    self.postMessage({ ok: true, result: serialized, logs });
  } catch (err) {
    self.postMessage({ ok: false, error: String(err && err.stack ? err.stack : err), logs });
  }
};
`;

let cachedUrl: string | null = null;
function workerUrl(): string {
	if (cachedUrl) return cachedUrl;
	const blob = new Blob([WORKER_SOURCE], { type: "application/javascript" });
	cachedUrl = URL.createObjectURL(blob);
	return cachedUrl;
}

/** Run agent-authored JS against the latest run's signals in an isolated,
 * time-limited Web Worker. Never throws — failures come back as `{ok:false}`. */
export function runProbe(input: ProbeInput): Promise<ProbeResult> {
	return new Promise((resolve) => {
		let worker: Worker;
		try {
			worker = new Worker(workerUrl());
		} catch (err) {
			resolve({
				ok: false,
				logs: [],
				error: `Failed to start sandbox: ${err}`,
			});
			return;
		}

		const timer = setTimeout(() => {
			worker.terminate();
			resolve({
				ok: false,
				logs: [],
				error: `Probe timed out after ${TIMEOUT_MS}ms (infinite loop?).`,
			});
		}, TIMEOUT_MS);

		worker.onmessage = (e: MessageEvent<ProbeResult>) => {
			clearTimeout(timer);
			worker.terminate();
			resolve(e.data);
		};
		worker.onerror = (e) => {
			clearTimeout(timer);
			worker.terminate();
			resolve({ ok: false, logs: [], error: e.message });
		};

		worker.postMessage({
			code: input.code,
			views: input.views,
			span: input.span,
		});
	});
}

/** Pull the bits of a RunResult the probe operates on. */
export function runResultToProbeViews(
	result: RunResult | null,
): Record<string, Signal> {
	if (!result) return {};
	const out: Record<string, Signal> = {};
	for (const [k, v] of Object.entries(result.views)) {
		if (v) out[k] = v;
	}
	return out;
}
