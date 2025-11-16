import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";

import type {
	AudioCrop,
	BeatGrid,
	PatternEntrySummary,
	PlaybackStateSnapshot,
} from "@/bindings/schema";

type EntriesMap = Record<string, PatternEntrySummary>;

type PatternPlaybackStore = {
	entries: EntriesMap;
	activeNodeId: string | null;
	isPlaying: boolean;
	currentTime: number;
	durationSeconds: number;
	setEntries: (entries: EntriesMap) => void;
	handleSnapshot: (snapshot: PlaybackStateSnapshot) => void;
	reset: () => void;
	play: (nodeId: string) => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
};

const initialState = {
	entries: {} as EntriesMap,
	activeNodeId: null,
	isPlaying: false,
	currentTime: 0,
	durationSeconds: 0,
};

function beatGridEquals(a: BeatGrid | null | undefined, b: BeatGrid | null | undefined) {
	if (!a && !b) return true;
	if (!a || !b) return false;
	if (a.beats.length !== b.beats.length) return false;
	if (a.downbeats.length !== b.downbeats.length) return false;
	for (let i = 0; i < a.beats.length; i += 1) {
		if (a.beats[i] !== b.beats[i]) return false;
	}
	for (let i = 0; i < a.downbeats.length; i += 1) {
		if (a.downbeats[i] !== b.downbeats[i]) return false;
	}
	return true;
}

function cropEquals(a: AudioCrop | null | undefined, b: AudioCrop | null | undefined) {
	if (!a && !b) return true;
	if (!a || !b) return false;
	return a.startSeconds === b.startSeconds && a.endSeconds === b.endSeconds;
}

function entriesEqual(prev: EntriesMap, next: EntriesMap) {
	const prevKeys = Object.keys(prev);
	const nextKeys = Object.keys(next);
	if (prevKeys.length !== nextKeys.length) return false;
	for (const key of prevKeys) {
		const a = prev[key];
		const b = next[key];
		if (!b) return false;
		if (
			a.durationSeconds !== b.durationSeconds ||
			a.sampleRate !== b.sampleRate ||
			a.sampleCount !== b.sampleCount ||
			!beatGridEquals(a.beatGrid, b.beatGrid) ||
			!cropEquals(a.crop, b.crop)
		) {
			return false;
		}
	}
	return true;
}

export const usePatternPlaybackStore = create<PatternPlaybackStore>((set) => ({
	...initialState,
	setEntries: (entries) =>
		set((state) => {
			if (entriesEqual(state.entries, entries)) {
				return state;
			}
			const activeNodeId = state.activeNodeId;
			const durationSeconds = activeNodeId
				? entries[activeNodeId]?.durationSeconds ?? 0
				: 0;
			const currentTime = activeNodeId
				? Math.min(state.currentTime, durationSeconds)
				: 0;
			return {
				entries,
				activeNodeId,
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
}));
