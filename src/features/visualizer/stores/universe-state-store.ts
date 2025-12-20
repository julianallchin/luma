import { listen } from "@tauri-apps/api/event";
import type { PrimitiveState, UniverseState } from "@/bindings/universe";

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

let buffer: (UniverseBufferFrame | undefined)[] = [];
let bufferSize = 0;
let frameDeltaSec = 0;
let lastBufferTime: number | null = null;
let renderAudioTime: number | null = null;

let lastSignalTs: number | null = null;
let signalFps = 0;
let signalDeltaMs = 0;
let lastReadTs: number | null = null;
let readFps = 0;
let readDeltaMs = 0;

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
	frameDeltaSec = payload.frameDeltaSec;

	for (const frame of payload.frames) {
		const slot = frame.slot % Math.max(1, bufferSize);
		buffer[slot] = frame;
		lastBufferTime = frame.audioTimeSec;
		currentState = frame.data;
	}
}

function ingestLegacyFrame(state: UniverseState) {
	bumpSignalMetrics();
	ensureBuffer(bufferSize || 1);
	const audioTimeSec = lastBufferTime ?? 0;
	buffer[0] = { slot: 0, audioTimeSec, data: state };
	lastBufferTime = audioTimeSec;
	currentState = state;
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

function interpolatePrimitive(
	id: string,
	prevFrame: UniverseBufferFrame,
	nextFrame: UniverseBufferFrame,
	targetTime: number,
): PrimitiveState | undefined {
	const a = prevFrame.data.primitives[id];
	const b = nextFrame.data.primitives[id];
	if (!a && !b) return undefined;
	if (!a) return b;
	if (!b) return a;

	const span = nextFrame.audioTimeSec - prevFrame.audioTimeSec || frameDeltaSec;
	if (!span || span <= 0) return a;
	const t = Math.max(
		0,
		Math.min(1, (targetTime - prevFrame.audioTimeSec) / span),
	);

	const lerp = (x: number, y: number) => x * (1 - t) + y * t;
	return {
		dimmer: lerp(a.dimmer, b.dimmer),
		color: [
			lerp(a.color[0], b.color[0]),
			lerp(a.color[1], b.color[1]),
			lerp(a.color[2], b.color[2]),
		],
		strobe: lerp(a.strobe, b.strobe),
		position: [
			lerp(a.position[0], b.position[0]),
			lerp(a.position[1], b.position[1]),
		],
		// Speed is effectively binary (0 frozen / 1 fast), so snap instead of lerp
		speed: t < 0.5 ? a.speed : b.speed,
	};
}

export const universeStore = {
	init: async () => {
		console.log("Initializing Universe State Listener...");
		const unlistenBuffer = await listen<UniverseBufferEvent>(
			"universe-buffer",
			(event) => {
				ingestBuffer(event.payload);
			},
		);

		// Back-compat: consume legacy single-frame updates if still emitted.
		const unlistenLegacy = await listen<UniverseState>(
			"universe-state-update",
			(event) => {
				ingestLegacyFrame(event.payload);
			},
		);

		return () => {
			unlistenBuffer();
			unlistenLegacy();
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

		const { prev, next } = findFrames(targetTime);
		if (prev && next) {
			return interpolatePrimitive(id, prev, next, targetTime);
		}
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
	}),
};
