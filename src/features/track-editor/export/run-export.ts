import { invoke } from "@tauri-apps/api/core";
import type { ExportStarted } from "@/bindings/export";
import type { UniverseState } from "@/bindings/universe";
import type { StageExportHandle } from "@/features/visualizer/components/stage-visualizer";
import { universeStore } from "@/features/visualizer/stores/universe-state-store";
import { useExportStore } from "./use-export-store";

export interface RunExportOptions {
	trackId: string;
	venueId: string;
	outputPath: string;
	fps: number;
	width: number;
	height: number;
	handle: StageExportHandle;
}

// Expose VideoEncoder from the Window so we can feature-detect without TS moans.
declare global {
	interface Window {
		VideoEncoder?: typeof VideoEncoder;
		VideoFrame?: typeof VideoFrame;
	}
}

/**
 * Drive an offline render. Assumes `composite_track` has already been called
 * for the (trackId, venueId) pair so the render engine holds the layer.
 *
 * Pipeline: sample compositor → render scene to WebGLRenderTarget →
 * readPixels → flip vertically → wrap as `VideoFrame` → encode via WebCodecs
 * hardware H.264 encoder → ship tiny encoded chunks to Rust → ffmpeg stream-
 * copies to MP4 with audio.
 */
export async function runExport(opts: RunExportOptions): Promise<string> {
	const { trackId, venueId, outputPath, fps, width, height, handle } = opts;
	const store = useExportStore.getState();

	if (!window.VideoEncoder || !window.VideoFrame) {
		throw new Error(
			"WebCodecs not available in this WebView. Update macOS/Windows/Tauri.",
		);
	}

	// Ensure the layer is current (idempotent + cheap when cached).
	store.setStatus("Compositing track…");
	await invoke("composite_track", {
		trackId,
		venueId,
		skipCache: false,
	});

	store.setStatus("Starting encoder…");
	const started = await invoke<ExportStarted>("export_start", {
		trackId,
		outputPath,
		fps,
		width,
		height,
	});
	const sessionId = started.sessionId;
	const totalFrames = Number(started.totalFrames);
	store.start(sessionId, totalFrames);

	handle.beginExport(width, height);

	// Buffers reused across frames.
	const rgba = new Uint8Array(width * height * 4);
	const flipped = new Uint8Array(width * height * 4);
	const rowStride = width * 4;

	// Chunk queue — encoder outputs are drained by a single async task that
	// ships them to Rust in order. We cap in-flight pushes via backpressure
	// from `await invoke(...)` inside the drain task, so memory stays bounded.
	const chunkQueue: Uint8Array[] = [];
	let drainResolve: (() => void) | null = null;
	let drainDone = false;
	let drainErr: unknown = null;

	const drainer = (async () => {
		while (!drainDone || chunkQueue.length > 0) {
			const next = chunkQueue.shift();
			if (!next) {
				await new Promise<void>((resolve) => {
					drainResolve = resolve;
				});
				continue;
			}
			try {
				await invoke("export_push_chunk", next, {
					headers: { "x-session-id": sessionId },
				});
			} catch (err) {
				drainErr = err;
				return;
			}
		}
	})();

	const pokeDrainer = () => {
		if (drainResolve) {
			const r = drainResolve;
			drainResolve = null;
			r();
		}
	};

	// Encoder setup.
	let encodedCount = 0;
	const encoder = new VideoEncoder({
		output: (chunk) => {
			const buf = new Uint8Array(chunk.byteLength);
			chunk.copyTo(buf);
			chunkQueue.push(buf);
			encodedCount++;
			pokeDrainer();
		},
		error: (e) => {
			drainErr = e;
			drainDone = true;
			pokeDrainer();
		},
	});
	// Bitrate sized to (pixels × fps × quality_factor). 0.2 is generous — a
	// 1080p30 stream gets ~12.4Mbps, enough for clean haze gradients without
	// blocky artifacts. We don't care about latency, so request quality mode
	// and a high-profile codec so the encoder uses every trick it has.
	const bitrate = Math.round(width * height * fps * 0.2);
	encoder.configure({
		codec: "avc1.640028", // H.264 High profile, level 4.0 (up to 1080p60)
		width,
		height,
		bitrate,
		bitrateMode: "variable",
		framerate: fps,
		hardwareAcceleration: "prefer-hardware",
		latencyMode: "quality",
		avc: { format: "annexb" },
	});
	console.log(
		`[export] encoder configured: ${width}x${height}@${fps}fps, ${(bitrate / 1_000_000).toFixed(1)}Mbps VBR, avc1.640028`,
	);

	// Rolling per-stage timers.
	const LOG_INTERVAL = 30;
	const totals = {
		sample: 0,
		inject: 0,
		advance: 0,
		glRender: 0,
		readPixels: 0,
		flip: 0,
		encode: 0,
		wallClock: 0,
	};
	const reset = () => {
		totals.sample = 0;
		totals.inject = 0;
		totals.advance = 0;
		totals.glRender = 0;
		totals.readPixels = 0;
		totals.flip = 0;
		totals.encode = 0;
		totals.wallClock = 0;
	};

	// Batched sample with prefetch: pull BATCH_SIZE universe states per invoke
	// so the 5ms Tauri IPC overhead is amortised over hundreds of frames, and
	// fire the next batch concurrently with rendering the current one so the
	// IPC wait time is hidden behind render work.
	const BATCH_SIZE = 120;
	let currentBatch: UniverseState[] = [];
	let currentBatchStart = 0;
	let nextBatchPromise: Promise<UniverseState[]> | null = null;
	let nextBatchStart = 0;

	const fetchBatch = (startFrame: number): Promise<UniverseState[]> => {
		const count = Math.min(BATCH_SIZE, totalFrames - startFrame);
		return invoke<UniverseState[]>("export_sample_batch", {
			sessionId,
			startFrame,
			count,
		});
	};

	try {
		const startedAt = performance.now();

		// Prime the pipeline: fetch the first batch synchronously, kick off the
		// second batch so it's in flight while we render the first.
		currentBatch = await fetchBatch(0);
		currentBatchStart = 0;
		nextBatchStart = currentBatch.length;
		if (nextBatchStart < totalFrames) {
			nextBatchPromise = fetchBatch(nextBatchStart);
		}

		for (let i = 0; i < totalFrames; i++) {
			if (useExportStore.getState().cancelRequested) {
				try {
					encoder.close();
				} catch {
					/* ignore */
				}
				drainDone = true;
				pokeDrainer();
				await invoke("export_cancel", { sessionId });
				store.finish();
				throw new Error("Export cancelled");
			}
			if (drainErr) throw drainErr;

			const loopStart = performance.now();
			const t = i / fps;

			const tA = performance.now();
			// Rotate batches when we've consumed the current one.
			if (i >= currentBatchStart + currentBatch.length) {
				if (!nextBatchPromise) {
					throw new Error(
						`No next batch available at frame ${i}/${totalFrames}`,
					);
				}
				currentBatch = await nextBatchPromise;
				currentBatchStart = nextBatchStart;
				nextBatchStart = currentBatchStart + currentBatch.length;
				nextBatchPromise =
					nextBatchStart < totalFrames ? fetchBatch(nextBatchStart) : null;
			}
			const universe = currentBatch[i - currentBatchStart];
			const tB = performance.now();

			universeStore.injectFrame(universe, t);
			const tC = performance.now();

			const timings = handle.renderFrame(t, rgba);
			const tD = performance.now();

			// Flip bottom-up WebGL output to top-down for H.264 (can't rely on
			// a post-encode vfilter because we're stream-copying to ffmpeg).
			for (let y = 0; y < height; y++) {
				const src = (height - 1 - y) * rowStride;
				const dst = y * rowStride;
				flipped.set(rgba.subarray(src, src + rowStride), dst);
			}
			const tE = performance.now();

			// Keyframe every 2 seconds so the output is seekable.
			const keyFrame = i % (fps * 2) === 0;
			const videoFrame = new window.VideoFrame(flipped, {
				format: "RGBA",
				codedWidth: width,
				codedHeight: height,
				timestamp: Math.round((i * 1_000_000) / fps),
				duration: Math.round(1_000_000 / fps),
			});
			encoder.encode(videoFrame, { keyFrame });
			videoFrame.close();
			const tF = performance.now();

			totals.sample += tB - tA;
			totals.inject += tC - tB;
			totals.advance += timings.advanceMs;
			totals.glRender += timings.renderMs;
			totals.readPixels += timings.readPixelsMs;
			totals.flip += tE - tD;
			totals.encode += tF - tE;
			totals.wallClock += tF - loopStart;

			store.setProgress(i + 1);

			if ((i + 1) % LOG_INTERVAL === 0) {
				const n = LOG_INTERVAL;
				const elapsedSec = (performance.now() - startedAt) / 1000;
				const contentSec = (i + 1) / fps;
				const speed = contentSec / elapsedSec;
				console.log(
					`[export] frames ${i + 1}/${totalFrames} | ` +
						`speed=${speed.toFixed(2)}x | encoded=${encodedCount} queue=${chunkQueue.length} | ` +
						`avg per-frame ms: ` +
						`sample=${(totals.sample / n).toFixed(1)} ` +
						`advance=${(totals.advance / n).toFixed(1)} ` +
						`glRender=${(totals.glRender / n).toFixed(1)} ` +
						`readPixels=${(totals.readPixels / n).toFixed(1)} ` +
						`flip=${(totals.flip / n).toFixed(1)} ` +
						`encode=${(totals.encode / n).toFixed(1)} ` +
						`loop=${(totals.wallClock / n).toFixed(1)}`,
				);
				reset();
			}
		}

		store.setStatus("Flushing encoder…");
		await encoder.flush();
		encoder.close();

		store.setStatus("Finalizing…");
		drainDone = true;
		pokeDrainer();
		await drainer;
		if (drainErr) throw drainErr;

		const outPath = await invoke<string>("export_finish", { sessionId });
		store.finish();
		return outPath;
	} catch (err) {
		try {
			encoder.state !== "closed" && encoder.close();
		} catch {
			/* ignore */
		}
		drainDone = true;
		pokeDrainer();
		try {
			await drainer;
		} catch {
			/* ignore */
		}
		try {
			await invoke("export_cancel", { sessionId });
		} catch {
			/* ignore */
		}
		store.finish();
		throw err;
	} finally {
		handle.endExport();
	}
}
