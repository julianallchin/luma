import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";
import type {
	DeckEvent,
	DeckState,
	PerformTrackMatch,
} from "@/bindings/perform";

// Module-level reconnect timer so it persists regardless of component mount state
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleReconnect(
	source: "stagelinq" | "prodjlink",
	deviceNum: number | null,
) {
	if (reconnectTimer) clearTimeout(reconnectTimer);
	console.warn("[perform] scheduleReconnect scheduled in 3s, source=", source);
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		console.warn(
			"[perform] scheduleReconnect firing, calling connect(",
			source,
			")",
		);
		usePerformStore.getState().connect(source, deviceNum ?? undefined);
	}, 3000);
}

function cancelReconnect() {
	if (reconnectTimer) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
}

export interface DeckMatchState {
	trackNetworkPath: string;
	matchedTrackId: string | null;
	hasLightShow: boolean;
	matching: boolean;
}

export interface MixerState {
	channelFaders: Record<number, number>;
	crossfader: number;
}

interface PerformState {
	connectionStatus: "idle" | "connecting" | "connected" | "error";
	source: "stagelinq" | "prodjlink" | null;
	lastSource: "stagelinq" | "prodjlink" | null;
	lastDeviceNum: number | null;
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

	// MIDI mixer state (null = not connected / no mixer MIDI)
	mixerState: MixerState | null;

	connect: (
		source: "stagelinq" | "prodjlink",
		deviceNum?: number,
	) => Promise<void>;
	disconnect: () => Promise<void>;
	reconnectIfNeeded: () => void;
	setMixerState: (state: MixerState | null) => void;
}

/** Compute crossfader weight for a deck (0 = silent, 1 = full). */
function crossfaderWeight(deckId: number, crossfader: number): number {
	if (deckId === 1) return 1 - crossfader;
	if (deckId === 2) return crossfader;
	return 1;
}

export const usePerformStore = create<PerformState>((set, get) => ({
	connectionStatus: "idle",
	source: null,
	lastSource: null,
	lastDeviceNum: null,
	deviceName: null,
	decks: new Map(),
	crossfader: 0,
	masterTempo: 0,
	error: null,
	unlisten: null,
	deckMatches: new Map(),
	activeDeckId: null,
	isCompositing: false,
	mixerState: null,

	setMixerState: (state) => {
		set({ mixerState: state });
		if (state) {
			const { decks } = get();
			const deckStates = Array.from(decks.values())
				.filter((d) => d.sample_rate > 0)
				.map((d) => {
					const fader = state.channelFaders[d.id] ?? 1.0;
					const volume = fader * crossfaderWeight(d.id, state.crossfader);
					return { deck_id: d.id, time: d.samples / d.sample_rate, volume };
				});
			if (deckStates.length > 0) {
				invoke("render_set_deck_states", { states: deckStates }).catch(
					() => {},
				);
			}
		}
	},

	reconnectIfNeeded: () => {
		const { connectionStatus, lastSource, lastDeviceNum } = get();
		if (connectionStatus === "idle" && lastSource) {
			get().connect(lastSource, lastDeviceNum ?? undefined);
		}
	},

	connect: async (source, deviceNum) => {
		console.log(
			"[perform] connect() called, source=",
			source,
			"deviceNum=",
			deviceNum,
		);
		// Clean up any existing listener before creating a new one
		const existingUnlisten = get().unlisten;
		if (existingUnlisten) {
			existingUnlisten();
		}
		cancelReconnect();
		set({
			connectionStatus: "connecting",
			source,
			lastSource: source,
			lastDeviceNum: deviceNum ?? null,
			error: null,
			unlisten: null,
		});

		try {
			// Subscribe to events before connecting so we don't miss anything
			const unlisten = await listen<DeckEvent>("perform_event", (event) => {
				const data = event.payload;

				switch (data.type) {
					case "DeviceDiscovered":
						console.log("[perform] DeviceDiscovered:", data.name);
						set({ deviceName: `${data.name} (${data.version})` });
						break;
					case "Connected":
						console.log("[perform] Connected");
						set({ connectionStatus: "connected" });
						break;
					case "StateChanged": {
						const playingDecks = data.decks
							.filter((d) => d.playing)
							.map((d) => d.id);
						const sampleDecks = data.decks
							.filter((d) => d.sample_rate > 0)
							.map((d) => d.id);
						console.debug(
							"[perform] StateChanged: playing=",
							playingDecks,
							"with_samples=",
							sampleDecks,
							"cf=",
							data.crossfader,
						);

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
								console.log(
									"[perform] deck",
									deck.id,
									"track changed →",
									deck.track_network_path,
									"— triggering match",
								);
								matchDeck(deck.id, deck);
							} else if (pathChanged && !deck.song_loaded) {
								console.log("[perform] deck", deck.id, "track unloaded");
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

						// Drive per-deck render states for all playing decks.
						// If mixer MIDI is connected, use its fader/crossfader values.
						// For Pro DJ Link without mixer MIDI: use deck.fader (1.0) directly.
						// For StageLinQ without mixer MIDI: use deck.fader × crossfader weight.
						const currentSource = get().source;
						const mixer = get().mixerState;
						const deckStates = data.decks
							.filter((d) => d.sample_rate > 0)
							.map((d) => {
								let volume: number;
								if (mixer) {
									const fader = mixer.channelFaders[d.id] ?? 1.0;
									volume = fader * crossfaderWeight(d.id, mixer.crossfader);
								} else if (currentSource === "prodjlink") {
									volume = d.fader;
								} else {
									volume = d.fader * crossfaderWeight(d.id, cf);
								}
								return {
									deck_id: d.id,
									time: d.samples / d.sample_rate,
									volume,
								};
							});

						if (deckStates.length > 0) {
							console.debug(
								"[perform] render_set_deck_states:",
								deckStates
									.map((d) => `deck${d.deck_id}(vol=${d.volume.toFixed(2)})`)
									.join(", "),
							);
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
					case "Disconnected": {
						invoke("render_clear_perform").catch(() => {});
						const { lastSource, lastDeviceNum } = get();
						console.warn(
							"[perform] Disconnected — will scheduleReconnect:",
							lastSource,
						);
						if (lastSource) {
							scheduleReconnect(lastSource, lastDeviceNum);
						}
						set({
							connectionStatus: "idle",
							source: null,
							deckMatches: new Map(),
							activeDeckId: null,
						});
						break;
					}
					case "Error":
						console.error("[perform] Error:", data.message);
						set({
							connectionStatus: "error",
							error: data.message,
						});
						break;
				}
			});

			set({ unlisten });
			console.log(
				"[perform] event listener registered, invoking connect command",
			);

			if (source === "prodjlink") {
				await invoke("prodjlink_connect", { deviceNum: deviceNum ?? 7 });
			} else {
				await invoke("stagelinq_connect");
			}
		} catch (err) {
			console.error("[perform] connect() failed:", err);
			set({
				connectionStatus: "error",
				error: String(err),
			});
		}
	},

	disconnect: async () => {
		console.log("[perform] disconnect() called");
		const { unlisten, source } = get();
		// Cancel any pending auto-reconnect — this is a user-initiated disconnect
		cancelReconnect();
		if (unlisten) {
			unlisten();
		}

		invoke("render_clear_perform").catch(() => {});

		try {
			if (source === "prodjlink") {
				await invoke("prodjlink_disconnect");
			} else {
				await invoke("stagelinq_disconnect");
			}
		} catch {
			// ignore errors on disconnect
		}

		set({
			connectionStatus: "idle",
			source: null,
			lastSource: null,
			lastDeviceNum: null,
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
async function matchDeck(deckId: number, deck: DeckState) {
	const store = usePerformStore;

	console.log(
		"[perform] matchDeck: deck",
		deckId,
		"path=",
		deck.track_network_path,
	);

	const matches = new Map(store.getState().deckMatches);
	matches.set(deckId, {
		trackNetworkPath: deck.track_network_path,
		matchedTrackId: null,
		hasLightShow: false,
		matching: true,
	});
	store.setState({ deckMatches: matches });

	try {
		const { useAppViewStore } = await import(
			"@/features/app/stores/use-app-view-store"
		);
		const venueId = useAppViewStore.getState().currentVenue?.id ?? 0;
		const source = store.getState().source;

		let result: PerformTrackMatch;

		if (source === "prodjlink") {
			// Pioneer: match by BPM + fuzzy title/artist
			const bpm =
				deck.beat_bpm > 0 ? deck.beat_bpm : deck.bpm > 0 ? deck.bpm : 0;
			result = await invoke<PerformTrackMatch>(
				"perform_match_track_by_metadata",
				{
					title: deck.title,
					artist: deck.artist,
					bpm,
					durationSecs: deck.track_length,
					venueId,
				},
			);
		} else {
			// StageLinQ: match by source filename from track_network_path
			result = await invoke<PerformTrackMatch>("perform_match_track", {
				trackNetworkPath: deck.track_network_path,
				venueId,
			});
		}

		console.log(
			"[perform] match result: deck",
			deckId,
			"trackId=",
			result.trackId,
			"hasAnnotations=",
			result.hasAnnotations,
		);

		// Verify the deck still has the same track (guard against races)
		const current = store.getState().deckMatches.get(deckId);
		if (current?.trackNetworkPath !== deck.track_network_path) return;

		const updated = new Map(store.getState().deckMatches);
		updated.set(deckId, {
			trackNetworkPath: deck.track_network_path,
			matchedTrackId: result.trackId,
			hasLightShow: result.hasAnnotations,
			matching: false,
		});
		store.setState({ deckMatches: updated });

		const currentVenueId = useAppViewStore.getState().currentVenue?.id;
		if (result.trackId !== null && result.hasAnnotations) {
			store.setState({ isCompositing: true });
			try {
				if (currentVenueId != null) {
					console.log(
						"[perform] calling render_composite_deck for deck",
						deckId,
						"track=",
						result.trackId,
					);
					await invoke("render_composite_deck", {
						deckId: deckId,
						trackId: result.trackId,
						venueId: currentVenueId,
					});
					await invoke("midi_compile_cues_for_deck", {
						deckId: deckId,
						trackId: result.trackId,
						venueId: currentVenueId,
					});
				}
			} catch (err) {
				console.error("[perform] render_composite_deck failed:", err);
			} finally {
				store.setState({ isCompositing: false });
			}
		} else if (result.trackId === null && currentVenueId != null) {
			// No match in Luma — compile MIDI cues with a synthetic beat grid
			// derived from the CDJ's live BPM and beat-in-bar so beat-reactive
			// patterns stay in phase with the music.
			const bpm = deck.beat_bpm > 0 ? deck.beat_bpm : deck.bpm;
			if (bpm > 0 && deck.song_loaded) {
				const beatNumber = deck.beat > 0 ? Math.round(deck.beat) : 1;
				const positionSecs =
					deck.sample_rate > 0 ? deck.samples / deck.sample_rate : 0;
				console.log(
					"[perform] unmatched deck",
					deckId,
					"— compiling cues with synthetic beat grid (bpm=",
					bpm,
					"beat=",
					beatNumber,
					"pos=",
					positionSecs.toFixed(2),
					")",
				);
				try {
					await invoke("render_composite_deck_unmatched", {
						deckId,
						bpm,
						beatNumber,
						positionSecs,
						durationSecs: deck.track_length,
						venueId: String(currentVenueId),
					});
				} catch (err) {
					console.error(
						"[perform] render_composite_deck_unmatched failed:",
						err,
					);
				}
			}
		}
	} catch (err) {
		console.error("Failed to match track:", err);
		const current = store.getState().deckMatches.get(deckId);
		if (current?.trackNetworkPath !== deck.track_network_path) return;

		const updated = new Map(store.getState().deckMatches);
		updated.set(deckId, {
			trackNetworkPath: deck.track_network_path,
			matchedTrackId: null,
			hasLightShow: false,
			matching: false,
		});
		store.setState({ deckMatches: updated });
	}
}
