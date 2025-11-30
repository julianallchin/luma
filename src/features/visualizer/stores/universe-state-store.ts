import { listen } from "@tauri-apps/api/event";
import type { UniverseState } from "@/bindings/universe";

// Store universe state outside React for performance
let currentState: UniverseState = { primitives: {} };

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
				currentState = event.payload;
			},
		);
		return unlisten;
	},

	getState: () => currentState,

	getPrimitive: (id: string) => {
		return currentState.primitives[id];
	},
};
