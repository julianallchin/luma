import { invoke } from "@tauri-apps/api/core";
import type { Scene, WebGLRenderer } from "three";

type RenderTelemetryEntry = {
	event: string;
	route?: string;
	data?: Record<string, unknown>;
};

type RenderTelemetrySnapshotSource = {
	getSnapshot: () => Record<string, unknown>;
	intervalMs?: number;
};

type PerformanceWithMemory = Performance & {
	memory?: {
		usedJSHeapSize: number;
		totalJSHeapSize: number;
		jsHeapSizeLimit: number;
	};
};

let globalHandlersInstalled = false;

function getRoute() {
	return window.location.hash || window.location.pathname;
}

function getJsHeapSnapshot() {
	const memory = (performance as PerformanceWithMemory).memory;
	if (!memory) return null;
	return {
		usedBytes: memory.usedJSHeapSize,
		totalBytes: memory.totalJSHeapSize,
		limitBytes: memory.jsHeapSizeLimit,
	};
}

export function appendRenderTelemetry(
	event: string,
	data: Record<string, unknown> = {},
) {
	const entry: RenderTelemetryEntry = {
		event,
		route: getRoute(),
		data: {
			...data,
			jsHeap: getJsHeapSnapshot(),
		},
	};

	invoke("append_render_telemetry", { entry }).catch((err) => {
		console.error("[render-telemetry] append failed", err);
	});
}

export function installRenderTelemetryGlobalHandlers() {
	if (globalHandlersInstalled) return;
	globalHandlersInstalled = true;

	window.addEventListener("error", (event) => {
		appendRenderTelemetry("window-error", {
			message: event.message,
			filename: event.filename,
			lineno: event.lineno,
			colno: event.colno,
			error:
				event.error instanceof Error ? event.error.stack : String(event.error),
		});
	});

	window.addEventListener("unhandledrejection", (event) => {
		appendRenderTelemetry("unhandled-rejection", {
			reason:
				event.reason instanceof Error
					? event.reason.stack
					: String(event.reason),
		});
	});

	document.addEventListener("visibilitychange", () => {
		appendRenderTelemetry("visibility-change", {
			visibilityState: document.visibilityState,
		});
	});

	window.addEventListener("pagehide", () => {
		appendRenderTelemetry("page-hide");
	});
}

export function startRenderTelemetry({
	getSnapshot,
	intervalMs = 5000,
}: RenderTelemetrySnapshotSource) {
	appendRenderTelemetry("render-telemetry-start", getSnapshot());

	const id = window.setInterval(() => {
		appendRenderTelemetry("render-telemetry-snapshot", getSnapshot());
	}, intervalMs);

	return () => {
		window.clearInterval(id);
		appendRenderTelemetry("render-telemetry-stop", getSnapshot());
	};
}

export function getThreeTelemetrySnapshot(
	gl: WebGLRenderer | null,
	scene: Scene | null,
) {
	if (!gl) return { webgl: null };

	const context = gl.getContext();
	const canvas = gl.domElement;
	const debugInfo = context.getExtension("WEBGL_debug_renderer_info");
	const renderer = debugInfo
		? context.getParameter(debugInfo.UNMASKED_RENDERER_WEBGL)
		: context.getParameter(context.RENDERER);
	const vendor = debugInfo
		? context.getParameter(debugInfo.UNMASKED_VENDOR_WEBGL)
		: context.getParameter(context.VENDOR);

	return {
		webgl: {
			vendor,
			renderer,
			contextLost: context.isContextLost(),
			drawingBufferWidth: gl.getContext().drawingBufferWidth,
			drawingBufferHeight: gl.getContext().drawingBufferHeight,
			canvasClientWidth: canvas.clientWidth,
			canvasClientHeight: canvas.clientHeight,
			pixelRatio: gl.getPixelRatio(),
			info: {
				memory: { ...gl.info.memory },
				render: { ...gl.info.render },
				programs: gl.info.programs?.length ?? null,
			},
		},
		scene: scene
			? {
					children: scene.children.length,
					objects: countSceneObjects(scene),
				}
			: null,
	};
}

function countSceneObjects(scene: Scene) {
	let count = 0;
	scene.traverse(() => {
		count += 1;
	});
	return count;
}
