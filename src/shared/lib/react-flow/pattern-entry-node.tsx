import * as React from "react";
import type { NodeProps } from "reactflow";
import { usePatternPlaybackStore } from "@/features/patterns/stores/use-pattern-playback-store";
import { BaseNode, formatTime, usePatternEntryPlayback } from "./base-node";
import type { PatternEntryNodeData } from "./types";

export function PatternEntryNode(props: NodeProps<PatternEntryNodeData>) {
	const { id, data } = props;
	const entry = data.patternEntry ?? null;
	const durationSeconds = entry?.durationSeconds ?? 0;
	const playback = usePatternEntryPlayback(id);
	const beatSummary = entry?.beatGrid
		? `${entry.beatGrid.downbeats.length} downbeats`
		: "No beat grid";
	const sampleRateLabel = entry?.sampleRate
		? `${entry.sampleRate} Hz`
		: "Unknown rate";
	const durationLabel = entry ? formatTime(durationSeconds) : "0:00";
	const [pending, setPending] = React.useState(false);

	const handlePlay = async () => {
		if (!entry) return;
		setPending(true);
		try {
			await usePatternPlaybackStore.getState().play(id);
		} catch (err) {
			console.error("[PatternEntryNode] Failed to play", err);
		} finally {
			setPending(false);
		}
	};

	const handlePause = async () => {
		setPending(true);
		try {
			await usePatternPlaybackStore.getState().pause();
		} catch (err) {
			console.error("[PatternEntryNode] Failed to pause", err);
		} finally {
			setPending(false);
		}
	};

	const body = (
		<div className="px-2 pb-2 space-y-2 text-[11px]">
			<div className="flex items-center justify-between gap-2">
				<div className="flex gap-2">
					<button
						type="button"
						onClick={handlePlay}
						disabled={!entry || pending}
						className="rounded px-2 py-1 text-[11px] font-medium bg-emerald-600 text-white disabled:opacity-50"
					>
						Play
					</button>
					<button
						type="button"
						onClick={handlePause}
						disabled={pending}
						className="rounded px-2 py-1 text-[11px] font-medium bg-slate-700 text-white/80 disabled:opacity-50"
					>
						Pause
					</button>
				</div>
				<span className="text-[10px] uppercase tracking-wider text-slate-400">
					{entry ? durationLabel : "Awaiting audio"}
				</span>
			</div>
			<div className="relative h-3 rounded bg-slate-800 overflow-hidden">
				<div
					className="absolute inset-y-0 left-0 bg-emerald-500/70 transition-[width]"
					style={{ width: `${playback.progress * 100}%` }}
					aria-hidden
				/>
				<div className="absolute inset-0 flex items-center justify-center text-[10px] text-white/80 mix-blend-screen">
					{entry && playback.hasActive
						? `${formatTime(playback.currentTime)} / ${durationLabel}`
						: null}
				</div>
			</div>
			<div className="flex items-center justify-between text-[10px] uppercase tracking-wide text-slate-400">
				<span>{beatSummary}</span>
				<span>{sampleRateLabel}</span>
			</div>
			{!entry && (
				<p className="text-[10px] text-slate-500">
					Connect audio and beat grid inputs to enable preview playback.
				</p>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}
