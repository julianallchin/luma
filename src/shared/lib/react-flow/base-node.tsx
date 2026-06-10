import type * as React from "react";
import { useEffect, useRef } from "react";
import {
	Handle,
	type NodeProps,
	Position,
	useNodeId,
	useStore,
	useUpdateNodeInternals,
} from "reactflow";
import {
	getExtrapolatedHostTime,
	useHostAudioStore,
} from "@/features/patterns/stores/use-host-audio-store";
import { cn } from "@/shared/lib/utils";
import type { BaseNodeData } from "./types";
import { DEFAULT_PORT_COLOR, PORT_TYPE_COLORS } from "./types";

// Port geometry. Everything below a port — ring, dot, ghost, and the invisible
// React Flow handle — is centered on ONE anchor point: PORT_ANCHOR px in from
// the row's inner edge, vertically centered. The dot's center IS that anchor,
// so the wire (which connects to the handle at the same anchor) lands exactly
// in the dot. No second coordinate system to drift out of sync.
const PORT_ANCHOR = 6; // px from the row's inner edge to the port center
const PORT_RING = 9; // ring outer diameter
const PORT_RING_BORDER = 1.5;
const PORT_DOT = 4; // inner dot diameter (the canonical center)
const PORT_GHOST_H = 2; // lead-in stub thickness
const PORT_HIT = 14; // invisible handle hit-area diameter

function PortRow({
	side,
	type,
	id,
	label,
	color,
	connected,
}: {
	side: Position.Left | Position.Right;
	type: "source" | "target";
	id: string;
	label: string;
	color: string;
	connected: boolean;
}) {
	const isLeft = side === Position.Left;
	const edgeKey = isLeft ? "left" : "right";
	// Center an element on the anchor: pin its edge PORT_ANCHOR px in, then pull
	// it back by half its own size with a translate.
	const centered = {
		[edgeKey]: PORT_ANCHOR,
		top: "50%",
		transform: isLeft ? "translate(-50%, -50%)" : "translate(50%, -50%)",
	} as React.CSSProperties;

	return (
		<div
			className={cn(
				"relative flex items-center",
				isLeft ? "pl-4 pr-2" : "justify-end pr-4 pl-2",
			)}
		>
			<span>{label}</span>
			{/* Ghost lead-in (only when wired): the faint hidden segment of the wire
			    running from the card edge to the port, behind the card body. */}
			{connected && (
				<span
					className="pointer-events-none absolute opacity-40"
					style={
						{
							[edgeKey]: 0,
							top: "50%",
							width: PORT_ANCHOR,
							height: PORT_GHOST_H,
							transform: "translateY(-50%)",
							backgroundColor: color,
						} as React.CSSProperties
					}
				/>
			)}
			{/* Ring */}
			<span
				className="pointer-events-none absolute rounded-full"
				style={{
					...centered,
					width: PORT_RING,
					height: PORT_RING,
					border: `${PORT_RING_BORDER}px solid ${color}`,
				}}
			/>
			{/* Dot — the canonical center; everything else is aligned to it */}
			{connected && (
				<span
					className="pointer-events-none absolute rounded-full"
					style={{
						...centered,
						width: PORT_DOT,
						height: PORT_DOT,
						backgroundColor: color,
					}}
				/>
			)}
			{/* Invisible React Flow handle: connection anchor + hit target, sharing
			    the exact same center so the wire lands in the dot. */}
			<Handle
				type={type}
				id={id}
				position={side}
				className="!border-none !p-0"
				style={{
					...centered,
					width: PORT_HIT,
					height: PORT_HIT,
					backgroundColor: "transparent",
				}}
			/>
		</div>
	);
}

// BaseNode component that auto-renders handles
export function BaseNode<T extends BaseNodeData>(props: NodeProps<T>) {
	const { data } = props;

	// Which of this node's handles are wired up — used to draw connected ports
	// as a dot-in-a-ring and unconnected ones as a bare ring. Selecting a stable
	// key string (not the edges array) keeps the node from re-rendering on
	// unrelated edge churn (e.g. the periodic edge-color refresh).
	const nodeId = useNodeId();
	const connectedKey = useStore((s) => {
		if (!nodeId) return "";
		const ids: string[] = [];
		for (const e of s.edges) {
			if (e.source === nodeId && e.sourceHandle)
				ids.push(`s:${e.sourceHandle}`);
			if (e.target === nodeId && e.targetHandle)
				ids.push(`t:${e.targetHandle}`);
		}
		return ids.sort().join("|");
	});
	const connectedHandles = new Set(connectedKey ? connectedKey.split("|") : []);

	// Our handles are positioned with inset CSS that settles after React Flow's
	// initial handle measurement, leaving the connection anchor cached at the
	// default edge position (wire connects offset from the visible port). Force
	// a re-measure once mounted and whenever the port set changes so the anchor
	// matches the rendered ring/dot.
	const updateNodeInternals = useUpdateNodeInternals();
	useEffect(() => {
		if (nodeId) updateNodeInternals(nodeId);
	}, [nodeId, updateNodeInternals, data.inputs, data.outputs]);

	return (
		<div className="relative bg-card text-muted-foreground text-xs text-foreground border-2 border-gutter overflow-hidden min-w-[170px] rounded-lg">
			{/* header */}
			<div className="px-2 pt-1 pb-1 font-medium tracking-tight bg-trim">
				{data.title}
			</div>

			{/* Inputs and outputs are two independent columns: outputs start at
			    the top alongside inputs rather than stacking beneath them.
			    justify-between pins each column to its edge (so a lone output
			    column still hugs the right) with gap-2 as the minimum gutter
			    between the two label columns. */}
			<div className="py-1 flex justify-between gap-2">
				<div className="flex flex-col gap-1.5">
					{data.inputs.map((port) => (
						<PortRow
							key={port.id}
							side={Position.Left}
							type="target"
							id={port.id}
							label={port.label}
							color={PORT_TYPE_COLORS[port.portType] ?? DEFAULT_PORT_COLOR}
							connected={connectedHandles.has(`t:${port.id}`)}
						/>
					))}
				</div>
				<div className="flex flex-col items-end gap-1.5">
					{data.outputs.map((port) => (
						<PortRow
							key={port.id}
							side={Position.Right}
							type="source"
							id={port.id}
							label={port.label}
							color={PORT_TYPE_COLORS[port.portType] ?? DEFAULT_PORT_COLOR}
							connected={connectedHandles.has(`s:${port.id}`)}
						/>
					))}
				</div>
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
	const isPlaying = useHostAudioStore((state) => state.isPlaying);
	const wrapperRef = useRef<HTMLDivElement>(null);

	// Audio snapshots arrive at only a few Hz; a React-rendered position would
	// visibly step AND re-render every view node per tick. Instead the line
	// self-animates: an rAF loop extrapolates time from the last snapshot and
	// writes the transform directly (composite-only — no React, no layout).
	useEffect(() => {
		const el = wrapperRef.current;
		if (!el) return;

		const position = () => {
			const { durationSeconds } = useHostAudioStore.getState();
			const t = getExtrapolatedHostTime();
			const progress =
				durationSeconds > 0 ? Math.min(1, Math.max(0, t / durationSeconds)) : 0;
			el.style.transform = `translateX(${progress * 100}%)`;
		};

		position();
		if (!isPlaying) {
			// Paused: track seeks without animating.
			return useHostAudioStore.subscribe((s, prev) => {
				if (s.currentTime !== prev.currentTime) position();
			});
		}
		let raf = requestAnimationFrame(function tick() {
			position();
			raf = requestAnimationFrame(tick);
		});
		return () => cancelAnimationFrame(raf);
	}, [isPlaying]);

	if (!isLoaded) return null;

	return (
		<div ref={wrapperRef} className="pointer-events-none absolute inset-0">
			<div className="absolute inset-y-0 left-0 w-px bg-red-500/80" />
		</div>
	);
}
