import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";

import type { PatternEntrySummary, PlaybackStateSnapshot } from "@/bindings/schema";

type EntriesMap = Record<string, PatternEntrySummary>;

type PatternPlaybackStore = {
	entries: EntriesMap;
	activeNodeId: string | null;
	isPlaying: boolean;
	currentTime: number;
	durationSeconds: number;
	loopEnabled: boolean;
	setEntries: (entries: EntriesMap) => void;
	handleSnapshot: (snapshot: PlaybackStateSnapshot) => void;
	reset: () => void;
	play: (nodeId: string) => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
	setLoop: (enabled: boolean) => Promise<void>;
};

const initialState = {
	entries: {} as EntriesMap,
	activeNodeId: null,
	isPlaying: false,
	currentTime: 0,
	durationSeconds: 0,
	loopEnabled: false,
};

export const usePatternPlaybackStore = create<PatternPlaybackStore>((set) => ({
	...initialState,
	setEntries: (entries) =>
		set((state) => {
			const nextActive =
				state.activeNodeId && entries[state.activeNodeId]
					? state.activeNodeId
					: Object.keys(entries)[0] ?? null;
			const durationSeconds = nextActive
				? entries[nextActive]?.durationSeconds ?? 0
				: 0;
			const currentTime = nextActive
				? Math.min(state.currentTime, durationSeconds)
				: 0;
			return {
				entries,
				activeNodeId: nextActive,
				currentTime,
				durationSeconds,
				isPlaying: state.isPlaying,
			};
		}),
	handleSnapshot: (snapshot) =>
		set((state) => {
			const durationSeconds = snapshot.durationSeconds;
			const currentTime = Math.min(snapshot.currentTime, durationSeconds);
			return {
				...state,
				activeNodeId: snapshot.activeNodeId,
				isPlaying: snapshot.isPlaying,
				currentTime,
				durationSeconds,
			};
		}),
	reset: () => set({ ...initialState }),
	play: async (nodeId: string) => {
		await invoke("playback_play_node", { nodeId });
	},
	pause: async () => {
		await invoke("playback_pause");
	},
	seek: async (seconds: number) => {
		await invoke("playback_seek", { seconds });
	},
	setLoop: async (enabled: boolean) => {
		await invoke("playback_set_loop", { enabled });
		set({ loopEnabled: enabled });
	},
}));
