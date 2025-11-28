import { listen } from "@tauri-apps/api/event";

// Store DMX data outside of React state for performance
// Map<UniverseID, ChannelData>
const dmxUniverseData = new Map<number, Uint8Array>();

export const dmxStore = {
	init: async () => {
		console.log("Initializing DMX Listener...");
		const unlisten = await listen<[number, number[]]>(
			"dmx://update",
			(event) => {
				const [universe, data] = event.payload;
				// Convert number[] to Uint8Array if needed, or just store it
				dmxUniverseData.set(universe, new Uint8Array(data));
			},
		);
		return unlisten;
	},

	getUniverse: (universe: number): Uint8Array | undefined => {
		return dmxUniverseData.get(universe);
	},

	getChannel: (universe: number, address: number): number => {
		const data = dmxUniverseData.get(universe);
		if (!data) return 0;
		// DMX is 1-based in UI, usually 0-based in arrays?
		// QLC+ uses 0-based internally?
		// Let's assume 'address' passed in is 1-based DMX address.
		// So index is address - 1.
		const idx = address - 1;
		if (idx < 0 || idx >= 512) return 0;
		return data[idx];
	},
};
