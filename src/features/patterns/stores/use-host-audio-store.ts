import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";

import type { BeatGrid, HostAudioSnapshot } from "@/bindings/schema";

type HostAudioStore = {
	isLoaded: boolean;
	isPlaying: boolean;
	currentTime: number;
	durationSeconds: number;
	loopEnabled: boolean;

	// Actions
	loadSegment: (
		trackId: number,
		startTime: number,
		endTime: number,
		beatGrid: BeatGrid | null,
	) => Promise<void>;
	play: () => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
	setLoop: (enabled: boolean) => Promise<void>;
	handleSnapshot: (snapshot: HostAudioSnapshot) => void;
	reset: () => void;
};

const initialState = {
	isLoaded: false,
	isPlaying: false,
	currentTime: 0,
	durationSeconds: 0,
	loopEnabled: false,
};

export const useHostAudioStore = create<HostAudioStore>((set) => ({
	...initialState,

	loadSegment: async (trackId, startTime, endTime, beatGrid) => {
		await invoke("host_load_segment", {
			trackId,
			startTime,
			endTime,
			beatGrid,
		});
	},

	play: async () => {
		await invoke("host_play");
	},

	pause: async () => {
		await invoke("host_pause");
	},

	seek: async (seconds) => {
		await invoke("host_seek", { seconds });
	},

	setLoop: async (enabled) => {
		await invoke("host_set_loop", { enabled });
		set({ loopEnabled: enabled });
	},

	handleSnapshot: (snapshot) => {
		set({
			isLoaded: snapshot.isLoaded,
			isPlaying: snapshot.isPlaying,
			currentTime: snapshot.currentTime,
			durationSeconds: snapshot.durationSeconds,
			loopEnabled: snapshot.loopEnabled,
		});
	},

	reset: () => set({ ...initialState }),
}));
