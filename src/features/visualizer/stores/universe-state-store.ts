import { listen } from "@tauri-apps/api/event";
import type { UniverseState } from "@/bindings/universe";

// Store universe state outside React for performance
let currentState: UniverseState = { primitives: {} };
const now = () =>
	typeof performance !== "undefined" ? performance.now() : Date.now();

type UniverseBufferFrame = {
	slot: number;
	audioTimeSec: number;
	data: UniverseState;
};

type UniverseBufferEvent = {
	bufferSize: number;
	frameDeltaSec: number;
	playheadAudioTime: number;
	frames: UniverseBufferFrame[];
};

// Event coalescing — only process the latest event per animation frame
// to prevent burst pile-ups when the JS thread falls behind.
let pendingLegacy: UniverseState | null = null;
let pendingBuffer: UniverseBufferEvent | null = null;
let rafScheduled = false;

function flushPending() {
	rafScheduled = false;
	if (pendingBuffer) {
		ingestBuffer(pendingBuffer);
		pendingBuffer = null;
	}
	if (pendingLegacy) {
		ingestLegacyFrame(pendingLegacy);
		pendingLegacy = null;
	}
}

function scheduleLegacy(state: UniverseState) {
	legacyEventsReceived += 1;
	if (pendingLegacy) legacyEventsCoalesced += 1;
	pendingLegacy = state;
	if (!rafScheduled) {
		rafScheduled = true;
		requestAnimationFrame(flushPending);
	}
}

function scheduleBuffer(payload: UniverseBufferEvent) {
	bufferEventsReceived += 1;
	if (pendingBuffer) bufferEventsCoalesced += 1;
	pendingBuffer = payload;
	if (!rafScheduled) {
		rafScheduled = true;
		requestAnimationFrame(flushPending);
	}
}

let buffer: (UniverseBufferFrame | undefined)[] = [];
let bufferSize = 0;
let lastBufferTime: number | null = null;
let renderAudioTime: number | null = null;

let lastSignalTs: number | null = null;
let signalFps = 0;
let signalDeltaMs = 0;
let lastReadTs: number | null = null;
let readFps = 0;
let readDeltaMs = 0;
let legacyEventsReceived = 0;
let bufferEventsReceived = 0;
let legacyEventsCoalesced = 0;
let bufferEventsCoalesced = 0;
let framesIngested = 0;
let lastPrimitiveCount = 0;
let maxPrimitiveCount = 0;
let initCount = 0;
let activeListeners = 0;

function ensureBuffer(size: number) {
	if (size === bufferSize && buffer.length === size) return;
	bufferSize = size;
	buffer = Array.from({ length: size });
}

function bumpSignalMetrics() {
	const ts = now();
	if (lastSignalTs !== null) {
		const delta = ts - lastSignalTs;
		signalDeltaMs = delta;
		const fps = delta > 0 ? 1000 / delta : signalFps;
		signalFps = signalFps === 0 ? fps : signalFps * 0.9 + fps * 0.1;
	}
	lastSignalTs = ts;
}

function ingestBuffer(payload: UniverseBufferEvent) {
	bumpSignalMetrics();
	ensureBuffer(payload.bufferSize);

	for (const frame of payload.frames) {
		const slot = frame.slot % Math.max(1, bufferSize);
		buffer[slot] = frame;
		lastBufferTime = frame.audioTimeSec;
		currentState = frame.data;
		framesIngested += 1;
		lastPrimitiveCount = Object.keys(frame.data.primitives).length;
		maxPrimitiveCount = Math.max(maxPrimitiveCount, lastPrimitiveCount);
	}
}

function ingestLegacyFrame(state: UniverseState) {
	bumpSignalMetrics();
	ensureBuffer(bufferSize || 1);
	const audioTimeSec = lastBufferTime ?? 0;
	buffer[0] = { slot: 0, audioTimeSec, data: state };
	lastBufferTime = audioTimeSec;
	currentState = state;
	framesIngested += 1;
	lastPrimitiveCount = Object.keys(state.primitives).length;
	maxPrimitiveCount = Math.max(maxPrimitiveCount, lastPrimitiveCount);
}

function findFrames(targetTime: number) {
	let prev: UniverseBufferFrame | undefined;
	let next: UniverseBufferFrame | undefined;

	for (const frame of buffer) {
		if (!frame) continue;
		if (frame.audioTimeSec <= targetTime) {
			if (!prev || frame.audioTimeSec > prev.audioTimeSec) {
				prev = frame;
			}
		}
		if (frame.audioTimeSec >= targetTime) {
			if (!next || frame.audioTimeSec < next.audioTimeSec) {
				next = frame;
			}
		}
	}

	return { prev, next };
}

export const universeStore = {
	/** Reset all cached state so fixtures read as off. */
	clear: () => {
		currentState = { primitives: {} };
		buffer = [];
		lastBufferTime = null;
		renderAudioTime = null;
		lastPrimitiveCount = 0;
	},

	init: async () => {
		initCount += 1;
		activeListeners += 1;
		console.log("Initializing Universe State Listener...");
		const unlistenBuffer = await listen<UniverseBufferEvent>(
			"universe-buffer",
			(event) => {
				scheduleBuffer(event.payload);
			},
		);

		// Back-compat: consume legacy single-frame updates if still emitted.
		const unlistenLegacy = await listen<UniverseState>(
			"universe-state-update",
			(event) => {
				scheduleLegacy(event.payload);
			},
		);

		return () => {
			unlistenBuffer();
			unlistenLegacy();
			activeListeners = Math.max(0, activeListeners - 1);
		};
	},

	getState: () => currentState,

	setRenderAudioTime: (audioTimeSec: number | null) => {
		const ts = now();
		if (lastReadTs !== null) {
			const delta = ts - lastReadTs;
			readDeltaMs = delta;
			const fps = delta > 0 ? 1000 / delta : readFps;
			readFps = readFps === 0 ? fps : readFps * 0.9 + fps * 0.1;
		}
		lastReadTs = ts;
		renderAudioTime = audioTimeSec;
	},

	getPrimitive: (id: string, atAudioTimeSec?: number) => {
		const targetTime =
			atAudioTimeSec ?? renderAudioTime ?? lastBufferTime ?? null;
		if (targetTime === null) {
			return currentState.primitives[id];
		}

		// Step-hold (NOT interpolation): show the exact computed value at/just-
		// before the playhead. The engine evaluates the precise value at every
		// frame, so lerping between frames would invent a fade across a hard cut
		// (PNG); holding the last exact frame keeps boundaries crisp (SVG).
		const { prev, next } = findFrames(targetTime);
		if (prev?.data.primitives[id]) return prev.data.primitives[id];
		if (next?.data.primitives[id]) return next.data.primitives[id];
		return currentState.primitives[id];
	},

	getSignalMetrics: () => ({
		fps: signalFps,
		deltaMs: signalDeltaMs,
		lastTs: lastSignalTs,
		readFps,
		readDeltaMs,
		readTs: lastReadTs,
		bufferSize,
		framesIngested,
		lastPrimitiveCount,
		maxPrimitiveCount,
		legacyEventsReceived,
		bufferEventsReceived,
		legacyEventsCoalesced,
		bufferEventsCoalesced,
		initCount,
		activeListeners,
	}),
};
