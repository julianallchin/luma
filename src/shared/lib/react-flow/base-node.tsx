import type * as React from "react";
import { Handle, type NodeProps, Position } from "reactflow";
import { useHostAudioStore } from "@/features/patterns/stores/use-host-audio-store";
import type { BaseNodeData } from "./types";

// BaseNode component that auto-renders handles
export function BaseNode<T extends BaseNodeData>(props: NodeProps<T>) {
	const { data } = props;

	return (
		<div className="relative bg-card text-muted-foreground text-xs text-foreground border border-border shadow-sm overflow-hidden min-w-[170px] rounded-lg">
			{/* header */}
			<div className="px-2 pt-1 pb-1 font-medium tracking-tight border-b">
				{data.title}
			</div>

			<div className="px-2 py-1 space-y-1.5">
				{data.inputs.map((port) => (
					<div key={port.id} className="flex items-center gap-1">
						<Handle
							type="target"
							id={port.id}
							position={Position.Left}
							className="!w-2 !h-2 !bg-orange-400 !rounded-full !border-none !relative !p-0 !m-0 !left-0 !top-0"
							style={{ transform: "none" }}
						/>
						<span>{port.label}</span>
					</div>
				))}
				{data.outputs.map((port) => (
					<div key={port.id} className="flex items-center justify-end gap-1">
						<span>{port.label}</span>
						<Handle
							type="source"
							id={port.id}
							position={Position.Right}
							className="!w-2 !h-2 !bg-orange-400 !rounded-full !border-none !relative !p-0 !m-0 !right-0 !top-0"
							style={{ transform: "none" }}
						/>
					</div>
				))}
			</div>

			{/* custom content hook (graphs, knobs, etc.) */}
			{"body" in data && (data as { body?: React.ReactNode }).body}

			{/* parameters */}
			{"paramControls" in data &&
				(data as { paramControls?: React.ReactNode }).paramControls}
		</div>
	);
}

const DISABLED_PLAYBACK = {
	progress: 0,
	duration: 0,
	hasActive: false,
	currentTime: 0,
	isPlaying: false,
} as const;

export type PlaybackState = {
	progress: number;
	duration: number;
	hasActive: boolean;
	currentTime: number;
	isPlaying: boolean;
};

export function computePlaybackState(state: {
	isLoaded: boolean;
	currentTime: number;
	durationSeconds: number;
	isPlaying: boolean;
}): PlaybackState {
	if (!state.isLoaded) return DISABLED_PLAYBACK;

	const duration = state.durationSeconds || 0;
	const progress =
		duration > 0 ? Math.min(1, Math.max(0, state.currentTime / duration)) : 0;

	return {
		progress,
		duration,
		hasActive: true,
		currentTime: state.currentTime,
		isPlaying: state.isPlaying,
	};
}

export function formatTime(totalSeconds: number): string {
	if (!Number.isFinite(totalSeconds) || totalSeconds <= 0) {
		return "0:00";
	}
	const clamped = Math.max(0, totalSeconds);
	const minutes = Math.floor(clamped / 60);
	const seconds = Math.floor(clamped % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${seconds}`;
}

/**
 * Isolated playback indicator that subscribes to audio store independently.
 * This prevents parent canvas nodes from re-rendering on every currentTime update.
 */
export function PlaybackIndicator() {
	const isLoaded = useHostAudioStore((state) => state.isLoaded);
	const currentTime = useHostAudioStore((state) => state.currentTime);
	const durationSeconds = useHostAudioStore((state) => state.durationSeconds);
	const isPlaying = useHostAudioStore((state) => state.isPlaying);

	const playback = computePlaybackState({
		isLoaded,
		currentTime,
		durationSeconds,
		isPlaying,
	});

	if (!playback.hasActive) return null;

	return (
		<div
			className="pointer-events-none absolute inset-y-0 w-px bg-red-500/80"
			style={{ left: `${playback.progress * 100}%` }}
		/>
	);
}
