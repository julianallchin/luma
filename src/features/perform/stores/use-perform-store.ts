import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";
import type { DeckEvent, DeckState } from "@/bindings/perform";

interface PerformState {
	connectionStatus: "idle" | "connecting" | "connected" | "error";
	source: "stagelinq" | null;
	deviceName: string | null;
	decks: Map<number, DeckState>;
	crossfader: number;
	masterTempo: number;
	error: string | null;
	unlisten: UnlistenFn | null;

	connect: (source: "stagelinq") => Promise<void>;
	disconnect: () => Promise<void>;
}

export const usePerformStore = create<PerformState>((set, get) => ({
	connectionStatus: "idle",
	source: null,
	deviceName: null,
	decks: new Map(),
	crossfader: 0,
	masterTempo: 0,
	error: null,
	unlisten: null,

	connect: async (source) => {
		set({ connectionStatus: "connecting", source, error: null });

		try {
			// Subscribe to events before connecting so we don't miss anything
			const unlisten = await listen<DeckEvent>("stagelinq_event", (event) => {
				const data = event.payload;

				switch (data.type) {
					case "DeviceDiscovered":
						set({ deviceName: `${data.name} (${data.version})` });
						break;
					case "Connected":
						set({ connectionStatus: "connected" });
						break;
					case "StateChanged": {
						const deckMap = new Map<number, DeckState>();
						for (const deck of data.decks) {
							deckMap.set(deck.id, deck);
						}
						set({
							decks: deckMap,
							crossfader: data.crossfader,
							masterTempo: data.master_tempo,
						});
						break;
					}
					case "Disconnected":
						set({ connectionStatus: "idle", source: null });
						break;
					case "Error":
						set({
							connectionStatus: "error",
							error: data.message,
						});
						break;
				}
			});

			set({ unlisten });

			await invoke("stagelinq_connect");
		} catch (err) {
			set({
				connectionStatus: "error",
				error: String(err),
			});
		}
	},

	disconnect: async () => {
		const { unlisten } = get();
		if (unlisten) {
			unlisten();
		}

		try {
			await invoke("stagelinq_disconnect");
		} catch {
			// ignore errors on disconnect
		}

		set({
			connectionStatus: "idle",
			source: null,
			deviceName: null,
			decks: new Map(),
			crossfader: 0,
			masterTempo: 0,
			error: null,
			unlisten: null,
		});
	},
}));
