import { listen } from "@tauri-apps/api/event";

// Store DMX data outside of React state for performance
// Map<UniverseID, ChannelData>
const dmxUniverseData = new Map<number, Uint8Array>();
// Map<UniverseID, Map<Address, Value>> for user overrides
const overrideData = new Map<number, Map<number, number>>();

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
		const base = dmxUniverseData.get(universe) ?? new Uint8Array(512);
		const overrides = overrideData.get(universe);
		if (!overrides || overrides.size === 0) return base;

		const merged = new Uint8Array(base); // copy
		overrides.forEach((val, addr) => {
			const idx = addr - 1;
			if (idx >= 0 && idx < merged.length) {
				merged[idx] = val;
			}
		});
		return merged;
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
		const overrides = overrideData.get(universe);
		if (overrides?.has(address)) return overrides.get(address) ?? 0;
		return data[idx];
	},

	setOverride: (universe: number, address: number, value: number) => {
		let map = overrideData.get(universe);
		if (!map) {
			map = new Map();
			overrideData.set(universe, map);
		}
		map.set(address, Math.max(0, Math.min(255, Math.floor(value))));
	},

	clearOverride: (universe: number, address?: number) => {
		if (address === undefined) {
			overrideData.delete(universe);
			return;
		}
		const map = overrideData.get(universe);
		if (!map) return;
		map.delete(address);
		if (map.size === 0) overrideData.delete(universe);
	},
};
