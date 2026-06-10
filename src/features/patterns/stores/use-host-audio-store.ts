import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";

import type { BeatGrid, HostAudioSnapshot } from "@/bindings/schema";

type HostAudioStore = {
	isLoaded: boolean;
	isPlaying: boolean;
	currentTime: number;
	durationSeconds: number;
	loopEnabled: boolean;
	/** performance.now() when the last snapshot arrived. Snapshots tick at only
	 * a few Hz; smooth playhead animation extrapolates from this timestamp. */
	snapshotAtMs: number;
	/** True while the user drags the transport scrubber. Backend snapshots are
	 * round-trips behind the pointer, so we hold the optimistic currentTime and
	 * ignore the snapshot's stale time until the drag ends. */
	isScrubbing: boolean;

	// Actions
	loadSegment: (
		trackId: string,
		startTime: number,
		endTime: number,
		beatGrid: BeatGrid | null,
	) => Promise<void>;
	play: () => Promise<void>;
	pause: () => Promise<void>;
	seek: (seconds: number) => Promise<void>;
	/** Optimistic seek for live scrubbing: writes currentTime immediately so all
	 * playhead consumers (visualizer, view-signal nodes) move with the pointer,
	 * then fires the backend seek without waiting. */
	scrub: (seconds: number) => void;
	setScrubbing: (scrubbing: boolean) => void;
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
	snapshotAtMs: 0,
	isScrubbing: false,
};

/** Current playback time, extrapolated between snapshots while playing. */
export function getExtrapolatedHostTime(): number {
	const s = useHostAudioStore.getState();
	if (!s.isPlaying) return s.currentTime;
	const elapsed = (performance.now() - s.snapshotAtMs) / 1000;
	const t = s.currentTime + Math.max(0, elapsed);
	return s.durationSeconds > 0 ? Math.min(t, s.durationSeconds) : t;
}

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

	scrub: (seconds) => {
		// Optimistically advance the shared clock so every playhead consumer moves
		// with the pointer this frame. snapshotAtMs is reset so extrapolation (if
		// playing) measures from the scrubbed position, not the stale snapshot.
		set({ currentTime: seconds, snapshotAtMs: performance.now() });
		void invoke("host_seek", { seconds });
	},

	setScrubbing: (scrubbing) => set({ isScrubbing: scrubbing }),

	setLoop: async (enabled) => {
		await invoke("host_set_loop", { enabled });
		set({ loopEnabled: enabled });
	},

	handleSnapshot: (snapshot) => {
		set((state) => ({
			isLoaded: snapshot.isLoaded,
			isPlaying: snapshot.isPlaying,
			// While dragging, the optimistic scrub time is authoritative — a
			// snapshot in flight from before the seek would otherwise yank the
			// playhead backward and stutter.
			currentTime: state.isScrubbing ? state.currentTime : snapshot.currentTime,
			durationSeconds: snapshot.durationSeconds,
			loopEnabled: snapshot.loopEnabled,
			snapshotAtMs: performance.now(),
		}));
	},

	reset: () => set({ ...initialState }),
}));
