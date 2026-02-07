import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";
import type {
	DeckEvent,
	DeckState,
	PerformTrackMatch,
} from "@/bindings/perform";

export interface DeckMatchState {
	trackNetworkPath: string;
	matchedTrackId: number | null;
	hasLightShow: boolean;
	matching: boolean;
}

interface PerformState {
	connectionStatus: "idle" | "connecting" | "connected" | "error";
	source: "stagelinq" | null;
	deviceName: string | null;
	decks: Map<number, DeckState>;
	crossfader: number;
	masterTempo: number;
	error: string | null;
	unlisten: UnlistenFn | null;

	// Deck matching
	deckMatches: Map<number, DeckMatchState>;
	activeDeckId: number | null;
	isCompositing: boolean;

	connect: (source: "stagelinq") => Promise<void>;
	disconnect: () => Promise<void>;
}

/** Compute crossfader weight for a deck (0 = silent, 1 = full). */
function crossfaderWeight(deckId: number, crossfader: number): number {
	// Crossfader: 0 = deck 1 full, 1 = deck 2 full
	if (deckId === 1) return 1 - crossfader;
	if (deckId === 2) return crossfader;
	return 1; // decks 3/4 unaffected
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
	deckMatches: new Map(),
	activeDeckId: null,
	isCompositing: false,

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

						const state = get();

						// Detect track changes and trigger matching
						for (const deck of data.decks) {
							const prevMatch = state.deckMatches.get(deck.id);
							const pathChanged =
								prevMatch?.trackNetworkPath !== deck.track_network_path;

							if (pathChanged && deck.track_network_path && deck.song_loaded) {
								matchDeck(deck.id, deck.track_network_path);
							} else if (pathChanged && !deck.song_loaded) {
								// Track unloaded
								const matches = new Map(get().deckMatches);
								matches.delete(deck.id);
								set({ deckMatches: matches });
							}
						}

						// Determine active deck.
						// Prefer: playing deck with highest effective volume.
						// Fallback: any deck with a matched light show.
						const cf = data.crossfader;
						const currentMatches = get().deckMatches;
						let bestDeckId: number | null = null;
						let bestVolume = -1;
						let fallbackDeckId: number | null = null;
						for (const deck of data.decks) {
							const match = currentMatches.get(deck.id);
							if (!match?.hasLightShow) continue;
							if (deck.playing) {
								const vol = deck.fader * crossfaderWeight(deck.id, cf);
								if (vol > bestVolume) {
									bestVolume = vol;
									bestDeckId = deck.id;
								}
							} else if (fallbackDeckId === null) {
								fallbackDeckId = deck.id;
							}
						}
						if (bestDeckId === null) bestDeckId = fallbackDeckId;

						// Drive per-deck render states for crossfade blending
						const deckStates = data.decks
							.filter((d) => {
								const m = currentMatches.get(d.id);
								return m?.hasLightShow && d.sample_rate > 0;
							})
							.map((d) => ({
								deck_id: d.id,
								time: d.samples / d.sample_rate,
								volume: d.fader * crossfaderWeight(d.id, cf),
							}));

						if (deckStates.length > 0) {
							invoke("render_set_deck_states", {
								states: deckStates,
							}).catch(() => {});
						}

						set({
							decks: deckMap,
							crossfader: data.crossfader,
							masterTempo: data.master_tempo,
							activeDeckId: bestDeckId,
						});
						break;
					}
					case "Disconnected":
						// Clear all perform render state
						invoke("render_clear_perform").catch(() => {});
						set({
							connectionStatus: "idle",
							source: null,
							deckMatches: new Map(),
							activeDeckId: null,
						});
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

		// Clear all perform render state
		invoke("render_clear_perform").catch(() => {});

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
			deckMatches: new Map(),
			activeDeckId: null,
			isCompositing: false,
		});
	},
}));

/** Match a deck's track and composite if it has a light show. */
async function matchDeck(deckId: number, trackNetworkPath: string) {
	const store = usePerformStore;

	// Set matching state
	const matches = new Map(store.getState().deckMatches);
	matches.set(deckId, {
		trackNetworkPath,
		matchedTrackId: null,
		hasLightShow: false,
		matching: true,
	});
	store.setState({ deckMatches: matches });

	try {
		const result = await invoke<PerformTrackMatch>("perform_match_track", {
			trackNetworkPath,
		});

		// Verify the deck still has the same track (guard against races)
		const current = store.getState().deckMatches.get(deckId);
		if (current?.trackNetworkPath !== trackNetworkPath) return;

		const updated = new Map(store.getState().deckMatches);
		updated.set(deckId, {
			trackNetworkPath,
			matchedTrackId: result.trackId,
			hasLightShow: result.hasAnnotations,
			matching: false,
		});
		store.setState({ deckMatches: updated });

		// If matched with annotations, composite the track
		if (result.trackId !== null && result.hasAnnotations) {
			store.setState({ isCompositing: true });
			try {
				// Use the first available venue — the perform page will set this
				const { useAppViewStore } = await import(
					"@/features/app/stores/use-app-view-store"
				);
				const venueId = useAppViewStore.getState().currentVenue?.id;
				if (venueId != null) {
					await invoke("render_composite_deck", {
						deckId: deckId,
						trackId: result.trackId,
						venueId,
					});
				}
			} catch (err) {
				console.error("Failed to composite track:", err);
			} finally {
				store.setState({ isCompositing: false });
			}
		}
	} catch (err) {
		console.error("Failed to match track:", err);
		const current = store.getState().deckMatches.get(deckId);
		if (current?.trackNetworkPath !== trackNetworkPath) return;

		const updated = new Map(store.getState().deckMatches);
		updated.set(deckId, {
			trackNetworkPath,
			matchedTrackId: null,
			hasLightShow: false,
			matching: false,
		});
		store.setState({ deckMatches: updated });
	}
}
