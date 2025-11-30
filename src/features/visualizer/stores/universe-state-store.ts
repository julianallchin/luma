import { listen } from "@tauri-apps/api/event";
import type { UniverseState } from "@/bindings/universe";

// Store universe state outside React for performance
let currentState: UniverseState = { primitives: {} };
const now = () =>
	(typeof performance !== "undefined" ? performance.now() : Date.now());

let lastSignalTs: number | null = null;
let signalFps = 0;
let signalDeltaMs = 0;

export const universeStore = {
	init: async () => {
		console.log("Initializing Universe State Listener...");
		const unlisten = await listen<UniverseState>(
			"universe-state-update",
			(event) => {
                // Debug log every ~60 frames approx? No, just log id count
                if (Math.random() < 0.01) {
                    console.log("[UniverseStore] Received update", Object.keys(event.payload.primitives).length, "primitives");
                }
				const ts = now();
				if (lastSignalTs !== null) {
					const delta = ts - lastSignalTs;
					signalDeltaMs = delta;
					const fps = delta > 0 ? 1000 / delta : signalFps;
					signalFps = signalFps === 0 ? fps : signalFps * 0.9 + fps * 0.1;
				}
				lastSignalTs = ts;
				currentState = event.payload;
			},
		);
		return unlisten;
	},

	getState: () => currentState,

	getPrimitive: (id: string) => {
		return currentState.primitives[id];
	},

	getSignalMetrics: () => ({
		fps: signalFps,
		deltaMs: signalDeltaMs,
		lastTs: lastSignalTs,
	}),
};
